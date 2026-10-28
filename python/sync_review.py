"""Audio-aware, per-speaking-turn review. Inconclusive evidence never passes.

SyncNet expects 25fps BGR face crops and 16kHz MFCCs. Input video is resampled
at its native timing, not reduced to the visual review's four frames/second.
Offset is positive when speech occurs later than its matching mouth motion.
"""
from __future__ import annotations

from difflib import SequenceMatcher
import hashlib
import importlib.util
from importlib.metadata import version
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

POLICY = "per-speaker-sync-v1"
FPS = 25
MAX_SHIFT = 20
TOLERANCE_MS = 120
MIN_CONFIDENCE = 3.0
MAX_DISTANCE = 10.0
MIN_FACE_MARGIN = 0.7
ROOT = Path(os.environ.get("AIVS_WORKSPACE", Path(__file__).resolve().parents[1]))
ASSETS = ROOT / ".cache/sync-review"


def sha(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def check():
    for line in Path(__file__).with_name("requirements-sync.txt").read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            package, expected = line.split("==", 1)
            if version(package) != expected:
                raise RuntimeError(f"Lip-sync review package {package} needs setup.")
    pins = json.loads(Path(__file__).with_name("sync-assets.json").read_text())
    receipt = json.loads((ASSETS / "assets.json").read_text())
    if receipt["pins"] != pins:
        raise RuntimeError("Lip-sync review assets need setup.")
    for asset in pins["files"]:
        if sha(ASSETS / asset["name"]) != asset["sha256"]:
            raise RuntimeError(f"Lip-sync review asset changed: {asset['name']}")
    for name, digest in receipt["asr_files"].items():
        if sha(ASSETS / "asr" / name) != digest:
            raise RuntimeError("Dialogue timing model changed; retry setup.")
    if set(receipt["asr_files"]) != {"model.bin", "config.json", "tokenizer.json", "vocabulary.txt"}:
        raise RuntimeError("Dialogue timing model is incomplete; retry setup.")
    import cv2  # noqa: F401
    import torch  # noqa: F401
    import python_speech_features  # noqa: F401
    from faster_whisper import WhisperModel  # noqa: F401
    return {"policy": POLICY, "assets_sha256": sha(ASSETS / "assets.json")}


def tokens(text):
    return re.findall(r"[a-z0-9]+", text.lower().replace("’", "'").replace("'", ""))


def locate_turns(audio, turns):
    from faster_whisper import WhisperModel
    recognizer = WhisperModel(str(ASSETS / "asr"), device="cpu", compute_type="int8", cpu_threads=4,
                             local_files_only=True)
    segments, _ = recognizer.transcribe(str(audio), language="en", word_timestamps=True,
                                       beam_size=5, condition_on_previous_text=False)
    heard = []
    for segment in segments:
        for word in segment.words or []:
            for token in tokens(word.word):
                heard.append({"text": token, "start": word.start, "end": word.end,
                              "probability": word.probability})
    expected = [word for turn in turns for word in tokens(turn["text"])]
    aligned = {}
    for block in SequenceMatcher(None, expected, [w["text"] for w in heard], autojunk=False).get_matching_blocks():
        for index in range(block.size):
            aligned[block.a + index] = heard[block.b + index]
    matched_heard = {id(word) for word in aligned.values()}
    speech_coverage = sum(id(word) in matched_heard for word in heard) / max(len(heard), 1)
    cursor, result = 0, []
    for turn in turns:
        words = tokens(turn["text"])
        matches = [aligned[i] for i in range(cursor, cursor + len(words))
                   if i in aligned and aligned[i]["probability"] >= 0.6]
        cursor += len(words)
        coverage = len(matches) / max(len(words), 1)
        row = {**turn, "text_coverage": coverage}
        boundary_known = bool(words) and all(
            index in aligned and aligned[index]["probability"] >= 0.6
            for index in (cursor - len(words), cursor - 1))
        if coverage < 0.85 or len(matches) < 2 or not boundary_known:
            row.update(status="unassessed", reason="Could not reliably locate this scripted line in the final audio.")
        elif speech_coverage < 0.95:
            row.update(status="unassessed", reason="Additional or unlocated speech prevents a complete speaking-turn assessment.")
        else:
            row.update(start_seconds=matches[0]["start"], end_seconds=matches[-1]["end"], words=matches)
        result.append(row)
    return result, speech_coverage


def load_syncnet(torch):
    spec = importlib.util.spec_from_file_location("aivs_pinned_syncnet", ASSETS / "SyncNetModel.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    model = module.S()
    model.load_state_dict(torch.load(ASSETS / "syncnet_v2.model", map_location="cpu", weights_only=True), strict=True)
    model.eval()
    return model


def audio_features(audio, model, torch, np):
    from scipy.io import wavfile
    import python_speech_features
    rate, samples = wavfile.read(audio)
    mfcc = python_speech_features.mfcc(samples, rate).T
    features = []
    count = (mfcc.shape[1] - 20) // 4 + 1
    with torch.inference_mode():
        for first in range(0, count, 32):
            batch = np.stack([mfcc[:, i * 4:i * 4 + 20] for i in range(first, min(first + 32, count))])
            features.append(model.forward_aud(torch.from_numpy(batch[:, None]).float()).numpy())
    return np.concatenate(features) if features else np.empty((0, 1024))


def measure(track, audio, start, end, np):
    # Evaluate every shift over identical, real samples. Zero-padding distances
    # would create false confidence near the start/end of a clip.
    positions = [i for i in track["features"] if start <= (i + 2.5) / FPS <= end
                 and MAX_SHIFT <= i < len(audio) - MAX_SHIFT]
    if len(positions) < 15:
        return None
    visual = np.stack([track["features"][i] for i in positions])
    distances = np.array([np.linalg.norm(visual - audio[np.array(positions) + shift], axis=1).mean()
                          for shift in range(-MAX_SHIFT, MAX_SHIFT + 1)])
    index = int(distances.argmin())
    confidence = float(np.median(distances) - distances[index])
    return {"face_track": track["id"], "audio_lag_ms": (index - MAX_SHIFT) * 1000 // FPS,
            "confidence": round(confidence, 3), "distance": round(float(distances[index]), 3),
            "samples": len(positions), "search_limit": index in (0, 2 * MAX_SHIFT)}


def assess_turn(turn, tracks, audio, np):
    if turn.get("status") == "unassessed":
        return turn
    start, end = turn["start_seconds"], turn["end_seconds"]
    if end - start < 0.7:
        return {**turn, "status": "unassessed", "reason": "Speaking turn is too short for a reliable sync measurement."}
    windows = []
    for centre in np.arange(start + 0.35, end - 0.15, 0.4):
        lo, hi = max(start, float(centre) - 0.5), min(end, float(centre) + 0.5)
        # Exclude pauses; mouth motion during a breath is not a phonetic error.
        if not any(word["start"] <= centre <= word["end"] + 0.08 for word in turn["words"]):
            continue
        candidates = [value for track in tracks if (value := measure(track, audio, lo, hi, np))]
        candidates.sort(key=lambda value: value["confidence"], reverse=True)
        window = {"start_seconds": round(lo, 3), "end_seconds": round(hi, 3), "status": "unassessed"}
        if candidates:
            best = candidates[0]
            window.update(best)
            ambiguous = len(candidates) > 1 and best["confidence"] - candidates[1]["confidence"] < MIN_FACE_MARGIN
            if best["confidence"] >= MIN_CONFIDENCE and best["distance"] <= MAX_DISTANCE and not ambiguous and not best["search_limit"]:
                window["status"] = "issues_detected" if abs(best["audio_lag_ms"]) > TOLERANCE_MS else "passed"
        windows.append(window)
    known = [w for w in windows if w["status"] != "unassessed"]
    faces = {w["face_track"] for w in known}
    # Every sampled speech window must be assessable. A clear defect still wins
    # over missing evidence elsewhere, but the missing evidence is retained.
    issues = any(w["status"] == "issues_detected" for w in known)
    complete = bool(windows) and len(known) == len(windows) and len(faces) == 1
    status = "issues_detected" if issues else "passed" if complete else "unassessed"
    result = {**turn, "status": status, "windows": windows,
              "face_track": next(iter(faces)) if len(faces) == 1 else None}
    if status == "unassessed":
        result["reason"] = "Insufficient visible speech, uncertain active face, or low audiovisual confidence."
    result.pop("words", None)
    return result


def review(clip, start_seconds, seconds, turns, on_progress):
    identity = check()
    import numpy as np
    import torch
    from sync_faces import embeddings
    torch.set_num_threads(4)
    with tempfile.TemporaryDirectory(prefix="aivs-sync-") as temporary:
        directory = Path(temporary)
        audio, video = directory / "audio.wav", directory / "video.mp4"
        common = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-i", str(clip),
                  "-ss", str(start_seconds), "-t", str(seconds)]
        # Both decodes use the same final presentation timeline and shot bounds.
        subprocess.run([*common, "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", str(audio)], check=True)
        subprocess.run([*common, "-an", "-vf", "fps=25,scale=w='min(960,iw)':h=-2",
                        "-c:v", "libx264", "-crf", "16", "-preset", "ultrafast", str(video)], check=True)
        on_progress("Locating scripted speaking turns in the final audio")
        located, speech_coverage = locate_turns(audio, turns)
        model = load_syncnet(torch)
        sound = audio_features(audio, model, torch, np)
        tracks, frames = embeddings(video, ASSETS / "face_detection_yunet.onnx", model, torch, on_progress)
        assessed = [assess_turn(turn, tracks, sound, np) for turn in located]
    # A face cannot establish two different scripted identities in one shot.
    assignments = {}
    for turn in assessed:
        face = turn.get("face_track")
        if face is not None:
            assignments.setdefault(face, set()).add(turn["speaker"])
    for turn in assessed:
        if turn["status"] == "passed" and len(assignments.get(turn.get("face_track"), set())) > 1:
            turn.update(status="unassessed", reason="The same face was associated with different scripted speakers.")
    issues = [turn for turn in assessed if turn["status"] == "issues_detected"]
    unknown = [turn for turn in assessed if turn["status"] == "unassessed"]
    status = "issues_detected" if issues else "unassessed" if unknown else "passed"
    summary = (f"Lip-sync issues in {len(issues)} speaking turn(s)." if issues else
               f"Lip sync could not be assessed for {len(unknown)} speaking turn(s)." if unknown else
               f"Lip sync passed for {len(assessed)} speaking turn(s).")
    return {**identity, "status": status, "summary": summary, "turns": assessed,
            "recognized_speech_coverage": speech_coverage,
            "frames_reviewed": frames, "review_fps": FPS, "tolerance_ms": TOLERANCE_MS,
            "minimum_confidence": MIN_CONFIDENCE,
            "offset_convention": "Positive audio_lag_ms means the mouth leads the audio.",
            "limitations": "Automated English speech and audiovisual assessment; low confidence, short lines, hidden mouths and ambiguous faces remain unassessed. Does not certify voice identity or acting quality."}
