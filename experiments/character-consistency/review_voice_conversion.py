"""Remux and independently review a converted voice take without accepting it."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import wave

ROOT = Path(__file__).resolve().parents[2]
EXPECTED_TEXT = "We still have some time."
SAMPLE_RATE = 22050
TARGET_SECONDS = 4
TARGET_FRAMES = SAMPLE_RATE * TARGET_SECONDS
MAX_TAIL_PAD = 255
EXPECTED_VIDEO_FRAMES = 96
EXPECTED_FPS = 24
EXPECTED_CORRELATION = 0.99
WHISPER_MODEL = ROOT / ".cache/whisper/ggml-base.en.bin"
WHISPER_MODEL_RECORD = ROOT / ".cache/whisper/model.json"


def _hash(path: Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _json(path: Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"Expected a JSON object: {path}")
    return value


def _run(command: list[str], *, stdout_path: Path | None = None,
         stderr_path: Path | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(command, capture_output=True, check=False)
    if stdout_path is not None:
        stdout_path.write_bytes(result.stdout)
    if stderr_path is not None:
        stderr_path.write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}): {command[0]}")
    return result


def _read_pcm(path: Path, *, expected_rate: int | None = None) -> tuple[bytes, int, int, int]:
    try:
        with wave.open(str(path), "rb") as audio:
            channels, width, rate, frames = (audio.getnchannels(), audio.getsampwidth(),
                                              audio.getframerate(), audio.getnframes())
            data = audio.readframes(frames)
            if (channels < 1 or width != 2 or rate < 1 or frames < 1
                    or len(data) != frames * channels * width or audio.readframes(1)):
                raise ValueError("Audio must be non-empty, complete PCM16 WAV.")
    except (wave.Error, EOFError, OSError) as exc:
        raise ValueError(f"Malformed PCM16 WAV: {path}") from exc
    if expected_rate is not None and rate != expected_rate:
        raise ValueError(f"PCM16 WAV must be {expected_rate} Hz; got {rate} Hz.")
    return data, channels, rate, frames


def _make_mux_audio(converted: Path, target: Path) -> dict:
    raw, channels, rate, frames = _read_pcm(converted, expected_rate=SAMPLE_RATE)
    if channels != 1:
        raise ValueError("Converted audio must be mono PCM16 WAV.")
    if frames > TARGET_FRAMES:
        raise ValueError(f"Converted audio exceeds the 4-second target by {frames - TARGET_FRAMES} samples; refusing to trim.")
    missing = TARGET_FRAMES - frames
    if missing > MAX_TAIL_PAD:
        raise ValueError(f"Converted audio is short by {missing} samples; maximum permitted tail padding is {MAX_TAIL_PAD}.")
    original = raw
    padded = raw + b"\0\0" * missing
    target.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(target), "wb") as mux:
        mux.setnchannels(1)
        mux.setsampwidth(2)
        mux.setframerate(rate)
        mux.writeframes(padded)
    written, out_channels, out_rate, out_frames = _read_pcm(target, expected_rate=SAMPLE_RATE)
    if (out_channels != 1 or out_frames != TARGET_FRAMES or written[:len(original)] != original
            or written[len(original):] != b"\0\0" * missing):
        raise ValueError("Mux WAV did not preserve converter PCM exactly or append only the requested zero tail.")
    return {"input_frames": frames, "target_frames": TARGET_FRAMES,
            "tail_padding_samples": missing, "tail_padding_seconds": missing / rate,
            "delay_correction_seconds": 0.0,
            "delay_correction_applied": False,
            "note": "No measured delay correction or time stretch was applied.",
            "sha256": _hash(target), "sample_rate_hz": rate, "channels": 1,
            "sample_width_bytes": 2}


def _frame_md5(ffmpeg: str, video: Path, raw_output: Path | None = None) -> list[dict]:
    command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
               "-i", str(video), "-map", "0:v:0", "-f", "framemd5", "-"]
    result = _run(command)
    if raw_output is not None:
        raw_output.write_bytes(result.stdout)
    output = result.stdout.decode("utf-8", errors="strict")
    time_base = None
    rows = []
    for line in output.splitlines():
        if line.startswith("#tb 0: "):
            time_base = line.removeprefix("#tb 0: ").strip()
            continue
        if not line or line.startswith("#"):
            continue
        fields = [part.strip() for part in line.split(",")]
        if len(fields) < 6:
            raise ValueError(f"Unexpected FFmpeg framemd5 row: {line}")
        rows.append({"hash": fields[-1], "pts": fields[2], "duration": fields[3], "time_base": time_base})
    return rows


def _compare_frames(source: list[dict], converted: list[dict]) -> dict:
    aligned = len(source) == len(converted)
    row_matches = []
    for index, (original, result) in enumerate(zip(source, converted)):
        row_matches.append({"frame": index, "hash_match": original["hash"] == result["hash"],
                            "pts_match": original["pts"] == result["pts"],
                            "duration_match": original["duration"] == result["duration"],
                            "time_base_match": original["time_base"] == result["time_base"]})
    summary = {key: aligned and all(row[key] for row in row_matches)
               for key in ("hash_match", "pts_match", "duration_match", "time_base_match")}
    summary.update(frame_count_source=len(source), frame_count_converted=len(converted),
                   frame_count_match=aligned,
                   all_match=aligned and all(all(row[key] for key in ("hash_match", "pts_match",
                                                                        "duration_match", "time_base_match"))
                                             for row in row_matches),
                   frames=row_matches)
    return summary


def _ffprobe(ffprobe: str, path: Path, raw_output: Path | None = None) -> dict:
    result = _run([ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)])
    if raw_output is not None:
        raw_output.write_bytes(result.stdout)
    return json.loads(result.stdout.decode("utf-8"))


def _decode_pcm(ffmpeg: str, video: Path, target: Path, *, codec: str) -> bytes:
    command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
               "-i", str(video), "-map", "0:a:0", "-vn", "-t", str(TARGET_SECONDS),
               "-ac", "1", "-ar", str(SAMPLE_RATE), "-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"]
    output = _run(command, stderr_path=target.with_suffix(target.suffix + ".log")).stdout
    target.write_bytes(output)
    if len(output) != TARGET_FRAMES * 2:
        raise ValueError(f"Decoded {codec} audio has {len(output) // 2} samples; expected {TARGET_FRAMES}.")
    return output


def _remux_and_verify(ffmpeg: str, source_video: Path, mux_wav: Path, output_dir: Path) -> dict:
    output_dir.mkdir(parents=True, exist_ok=True)
    mp4 = output_dir / "converted-video.mp4"
    mp4_command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin", "-y",
                   "-i", str(source_video), "-i", str(mux_wav), "-map", "0:v:0", "-map", "1:a:0",
                   "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-ar", str(SAMPLE_RATE),
                   "-ac", "1", "-t", str(TARGET_SECONDS), "-frames:v", str(EXPECTED_VIDEO_FRAMES),
                   "-movflags", "+faststart", str(mp4)]
    _run(mp4_command, stderr_path=output_dir / "remux-mp4.log")
    mov = output_dir / "converted-video.mov"
    mov_command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin", "-y",
                   "-i", str(source_video), "-i", str(mux_wav), "-map", "0:v:0", "-map", "1:a:0",
                   "-c:v", "copy", "-c:a", "alac", "-ar", str(SAMPLE_RATE), "-ac", "1",
                   "-t", str(TARGET_SECONDS), "-frames:v", str(EXPECTED_VIDEO_FRAMES), str(mov)]
    _run(mov_command, stderr_path=output_dir / "remux-mov.log")
    source_frames = _frame_md5(ffmpeg, source_video, output_dir / "source-frames.framemd5")
    converted_frames = _frame_md5(ffmpeg, mp4, output_dir / "converted-frames.framemd5")
    lossless_frames = _frame_md5(ffmpeg, mov, output_dir / "lossless-frames.framemd5")
    mp4_comparison = _compare_frames(source_frames, converted_frames)
    mov_comparison = _compare_frames(source_frames, lossless_frames)
    (output_dir / "source-frames.json").write_text(json.dumps(source_frames, indent=2) + "\n")
    (output_dir / "converted-frames.json").write_text(json.dumps(converted_frames, indent=2) + "\n")
    (output_dir / "lossless-frames.json").write_text(json.dumps(lossless_frames, indent=2) + "\n")
    if not mp4_comparison["all_match"] or not mov_comparison["all_match"]:
        raise ValueError("Decoded H.264 frame hashes or timestamps changed in an MP4 or MOV remux.")
    mux_pcm, _, _, mux_frames = _read_pcm(mux_wav, expected_rate=SAMPLE_RATE)
    alac_pcm = _decode_pcm(ffmpeg, mov, output_dir / "alac-decoded.pcm", codec="ALAC")
    if alac_pcm != mux_pcm:
        raise ValueError("Decoded ALAC PCM does not exactly match mux-audio.wav.")
    aac_pcm = _decode_pcm(ffmpeg, mp4, output_dir / "aac-decoded.pcm", codec="AAC")
    correlation = _correlation(mux_pcm, aac_pcm)
    if correlation < EXPECTED_CORRELATION:
        raise ValueError(f"AAC decoded PCM correlation {correlation:.6f} is below {EXPECTED_CORRELATION}.")
    def frame_command(path):
        return [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
                "-i", str(path), "-map", "0:v:0", "-f", "framemd5", "-"]
    def pcm_command(path):
        return [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
                "-i", str(path), "-map", "0:a:0", "-vn", "-t", str(TARGET_SECONDS),
                "-ac", "1", "-ar", str(SAMPLE_RATE), "-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"]
    return {"mp4": mp4, "mov": mov, "source_frames": source_frames,
            "converted_frames": converted_frames, "mp4_command": mp4_command,
            "lossless_frames": lossless_frames,
            "frame_comparisons": {"mp4": mp4_comparison, "mov": mov_comparison},
            "mov_command": mov_command, "source_framemd5_command": frame_command(source_video),
            "converted_framemd5_command": frame_command(mp4),
            "alac_decode_command": pcm_command(mov), "aac_decode_command": pcm_command(mp4),
            "alac_pcm": {"decoded_sample_count": mux_frames,
                "exact_pcm_match": True, "method": "decoded ALAC s16le bytes equal mux PCM16 bytes"},
            "aac_pcm": {"decoded_sample_count": len(aac_pcm) // 2, "mux_sample_count": mux_frames,
                "correlation": correlation, "threshold": EXPECTED_CORRELATION, "offset_applied_samples": 0,
                "method": "Pearson correlation of decoded AAC and mux PCM at zero offset; codec check only"}}


def _correlation(left: bytes, right: bytes) -> float:
    from array import array

    a, b = array("h"), array("h")
    a.frombytes(left)
    b.frombytes(right)
    if sys.byteorder != "little":
        a.byteswap()
        b.byteswap()
    if len(a) != len(b) or not a:
        raise ValueError("PCM correlation requires equal non-empty samples.")
    mean_a, mean_b = sum(a) / len(a), sum(b) / len(b)
    covariance = sum((x - mean_a) * (y - mean_b) for x, y in zip(a, b))
    energy_a = sum((x - mean_a) ** 2 for x in a)
    energy_b = sum((y - mean_b) ** 2 for y in b)
    if energy_a == 0 or energy_b == 0:
        return 0.0
    return covariance / math.sqrt(energy_a * energy_b)


def _normalize(text: str) -> str:
    return " ".join(re.findall(r"[a-z0-9]+", text.casefold()))


def _word_timings(transcript: dict) -> list[dict]:
    words = []
    for segment in transcript.get("transcription", []):
        for token in segment.get("tokens", []):
            text = token.get("text", "")
            if not text or text.startswith("[_"):
                continue
            parts = re.findall(r"[A-Za-z]+", text)
            if not parts:
                continue
            timing = token.get("offsets", {})
            start = timing.get("from")
            end = timing.get("to")
            dtw = token.get("t_dtw")
            alignment = dtw * 0.01 if isinstance(dtw, int) and dtw >= 0 else None
            for index, part in enumerate(parts):
                begins_word = bool(text[:1].isspace()) or not words or index > 0
                if begins_word:
                    words.append({"word": part.casefold(), "whisper_offset_start_estimate_seconds": None,
                                  "whisper_offset_end_estimate_seconds": None,
                                  "t_dtw_token_alignment_seconds": []})
                else:
                    words[-1]["word"] += part.casefold()
                word = words[-1]
                if isinstance(start, (int, float)):
                    sec = start / 1000.0
                    current = word["whisper_offset_start_estimate_seconds"]
                    word["whisper_offset_start_estimate_seconds"] = sec if current is None else min(current, sec)
                if isinstance(end, (int, float)):
                    sec = end / 1000.0
                    current = word["whisper_offset_end_estimate_seconds"]
                    word["whisper_offset_end_estimate_seconds"] = sec if current is None else max(current, sec)
                if alignment is not None:
                    word["t_dtw_token_alignment_seconds"].append(alignment)
    return words


def _compare_word_timings(source: list[dict], converted: list[dict],
                           expected_text: str = EXPECTED_TEXT) -> list[dict]:
    expected = _normalize(expected_text.replace("'", "").replace("’", "")).split()
    source_map = {word["word"]: word for word in source}
    converted_map = {word["word"]: word for word in converted}
    result = []
    for word in expected:
        original, new = source_map.get(word), converted_map.get(word)
        source_dtw = original.get("t_dtw_token_alignment_seconds", []) if original else []
        converted_dtw = new.get("t_dtw_token_alignment_seconds", []) if new else []
        delta = None
        if len(source_dtw) == len(converted_dtw) and source_dtw:
            deltas = [right - left for left, right in zip(source_dtw, converted_dtw)]
            delta = deltas[0] if len(deltas) == 1 else deltas
        result.append({"word": word, "source": original, "converted": new,
                       "t_dtw_alignment_delta_seconds": delta})
    return result


def _audio_stats_from_wav(path: Path) -> dict:
    from array import array

    raw, channels, rate, frames = _read_pcm(path)
    values = array("h")
    values.frombytes(raw)
    if sys.byteorder != "little":
        values.byteswap()
    peak = max(abs(item) for item in values)
    rms = math.sqrt(sum(item * item for item in values) / len(values))
    threshold = 32768.0 * 10 ** (-45.0 / 20)
    window_frames = max(1, round(rate * 0.020))
    active = []
    for first in range(0, frames, window_frames):
        last = min(frames, first + window_frames)
        section = values[first * channels:last * channels]
        mean = sum(section) / len(section)
        demeaned_rms = math.sqrt(sum((sample - mean) ** 2 for sample in section) / len(section))
        if demeaned_rms >= threshold:
            active.append((first, last))
    return {"sample_rate_hz": rate, "channels": channels, "frames": frames,
            "duration_seconds": frames / rate, "peak_pcm16": peak,
            "rms_pcm16": rms, "clipped_samples": sum(x in (-32768, 32767) for x in values),
            "activity": {"method": "20 ms demeaned PCM RMS; approximate activity, not VAD",
                         "threshold_dbfs": -45.0, "active_windows": len(active),
                         "window_count": math.ceil(frames / window_frames),
                         "first_active_seconds": active[0][0] / rate if active else None,
                         "last_active_end_seconds": active[-1][1] / rate if active else None}}


def review_voice_conversion(run_dir: Path) -> dict:
    run_dir = Path(run_dir).expanduser().resolve()
    status_path = run_dir / "conversion-status.json"
    output_dir = run_dir / "voice-review"
    if output_dir.exists():
        raise FileExistsError(f"Voice review output already exists: {output_dir}")

    sys.path.insert(0, str(ROOT / "python"))
    import run_voice_conversion as conversion_runner

    prepared = conversion_runner.validate_prepared(run_dir)
    expected_text = prepared["dialogue"]
    character_id = prepared["character"]["id"]
    voice_id = prepared["voice_id"]
    status = _json(status_path)
    if (status.get("status") != "converted-needs-review"
            or status.get("fixed_voice_acceptance") is not False):
        raise ValueError("Conversion status must be converted-needs-review with fixed voice unaccepted.")
    if status.get("voice_id") != voice_id:
        raise ValueError("Conversion status voice ID differs from the validated prepared run.")
    if status.get("source_hashes") != prepared["source_hashes"]:
        raise ValueError("Conversion status source hashes differ from the validated prepared run.")
    output_record = status.get("converted_output", {})
    if output_record.get("file") != "conversion/converted.wav":
        raise ValueError("Conversion status must name conversion/converted.wav explicitly.")
    converted = run_dir / output_record["file"]
    if not converted.is_file() or _hash(converted) != output_record.get("sha256"):
        raise ValueError("Converted WAV is missing or its hash differs from conversion status.")
    output_dir.mkdir()
    review = {"version": 1, "status": "reviewing", "fixed_voice_acceptance": False,
              "human_review": {"status": "pending", "reviewer": None},
              "character_id": character_id, "voice_id": voice_id,
              "source_hashes": prepared["source_hashes"],
              "conversion_status_sha256": _hash(status_path),
              "converted_input": {"file": "conversion/converted.wav", "sha256": _hash(converted)},
              "expected_text": expected_text, "created_at": datetime.now(timezone.utc).isoformat(),
              "limitations": ["Transcription, codec checks, and frame hashes do not establish lip sync.",
                              "Whisper t_dtw values are rough token-alignment estimates, not word centers, onsets, or ends.",
                              "No independent listening, speaker identity, or acceptance decision is claimed."]}
    report_path = output_dir / "review.json"

    def save():
        report_path.write_text(json.dumps(review, indent=2) + "\n", encoding="utf-8")

    save()
    try:
        mux_wav = output_dir / "mux-audio.wav"
        pcm_info = _make_mux_audio(converted, mux_wav)
        ffmpeg, ffprobe, whisper = (shutil.which("ffmpeg"), shutil.which("ffprobe"), shutil.which("whisper-cli"))
        if not ffmpeg or not ffprobe or not whisper:
            raise RuntimeError("FFmpeg, ffprobe, and whisper-cli must already be installed.")
        model = WHISPER_MODEL.resolve()
        model_record_path = WHISPER_MODEL_RECORD.resolve()
        if not model.is_file() or not model_record_path.is_file():
            raise RuntimeError("Pinned local Whisper base.en model and provenance record are required; downloads are disabled.")
        model_record = _json(model_record_path)
        if model_record.get("sha256") != _hash(model):
            raise ValueError("Local Whisper model hash differs from its provenance record.")
        (output_dir / "review_voice_conversion.py").write_bytes(Path(__file__).read_bytes())
        shutil.copyfile(model_record_path, output_dir / "whisper-model.json")

        tools = {}
        for name, executable in (("ffmpeg", ffmpeg), ("ffprobe", ffprobe), ("whisper-cli", whisper)):
            version_command = [executable, "-version" if name != "whisper-cli" else "--version"]
            version = _run(version_command)
            tools[name] = {"executable": executable, "sha256": _hash(Path(executable)),
                           "version_command": version_command,
                           "version": (version.stdout + version.stderr).decode("utf-8", "replace").strip()}
        review["provenance"] = {"tools": tools,
                                "whisper_model": {**model_record, "actual_sha256": _hash(model)},
                                "scripts": {"review_voice_conversion.py": _hash(output_dir / "review_voice_conversion.py"),
                                            "whisper-model.json": _hash(output_dir / "whisper-model.json")}}
        review["mux_audio"] = {**pcm_info, "file": "mux-audio.wav",
                               "pcm16_stats": _audio_stats_from_wav(mux_wav)}
        review["audio_stats"] = {"accepted_native_audio": _audio_stats_from_wav(prepared["source_audio"]),
                                 "converted_pcm16": _audio_stats_from_wav(converted)}

        source_video = prepared["source_video"]
        source_probe = _ffprobe(ffprobe, source_video, output_dir / "source-video.ffprobe.json")
        video_streams = [item for item in source_probe.get("streams", []) if item.get("codec_type") == "video"]
        if (len(video_streams) != 1 or video_streams[0].get("codec_name") != "h264"
                or video_streams[0].get("avg_frame_rate") != "24/1"):
            raise ValueError("Accepted source video must contain one 24 fps H.264 stream.")
        remux = _remux_and_verify(ffmpeg, source_video, mux_wav, output_dir)
        original_frames = remux["source_frames"]
        if len(original_frames) != EXPECTED_VIDEO_FRAMES:
            raise ValueError(f"Accepted video has {len(original_frames)} decoded frames; expected {EXPECTED_VIDEO_FRAMES}.")
        mp4, mov = remux["mp4"], remux["mov"]
        converted_probe = _ffprobe(ffprobe, mp4, output_dir / "converted-video.ffprobe.json")
        output_video = [item for item in converted_probe.get("streams", []) if item.get("codec_type") == "video"]
        output_audio = [item for item in converted_probe.get("streams", []) if item.get("codec_type") == "audio"]
        if (len(output_video) != 1 or output_video[0].get("codec_name") != "h264"
                or len(output_audio) != 1 or output_audio[0].get("codec_name") != "aac"
                or output_audio[0].get("sample_rate") != str(SAMPLE_RATE)
                or output_audio[0].get("channels") != 1):
            raise ValueError("Converted MP4 must retain H.264 and contain 22.05 kHz mono AAC.")
        review["video_preservation"] = {
            "source_video_sha256": _hash(source_video), "converted_mp4_sha256": _hash(mp4),
            "converted_mov_sha256": _hash(mov), "source_video_codec": "h264",
            "frame_count": len(original_frames), "verified_containers": ["converted-video.mp4", "converted-video.mov"],
            "container_comparisons": remux["frame_comparisons"],
            "all_decoded_frame_hashes_pts_durations_and_time_bases_match": True,
            "method": "FFmpeg framemd5 hashes and timing fields for every decoded source and remuxed frame",
            "converted_mp4_file": "converted-video.mp4", "converted_mov_file": "converted-video.mov",
            "commands": {"mp4": remux["mp4_command"], "mov": remux["mov_command"]}}
        review["audio_codec_checks"] = {"alac": remux["alac_pcm"], "aac": remux["aac_pcm"]}

        transcriptions = {}
        for label, input_audio in (("source", prepared["source_audio"]), ("converted", mux_wav)):
            wav_path = output_dir / f"{label}-16k-mono.wav"
            extract = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin", "-y",
                       "-i", str(input_audio), "-vn", "-ac", "1", "-ar", "16000",
                       "-c:a", "pcm_s16le", str(wav_path)]
            _run(extract, stderr_path=output_dir / f"{label}-extract.log")
            prefix = output_dir / f"whisper-{label}"
            whisper_command = [whisper, "-ng", "-nfa", "-dtw", "base.en", "-m", str(model),
                               "-f", str(wav_path), "-l", "en", "-ojf", "-otxt", "-of", str(prefix)]
            _run(whisper_command, stdout_path=output_dir / f"whisper-{label}.stdout.log",
                 stderr_path=output_dir / f"whisper-{label}.stderr.log")
            transcript_path = prefix.with_suffix(".json")
            transcript = _json(transcript_path)
            text_path = prefix.with_suffix(".txt")
            observed = " ".join(segment.get("text", "").strip()
                                for segment in transcript.get("transcription", [])).strip()
            word_times = _word_timings(transcript)
            transcriptions[label] = {"command": whisper_command, "audio_file": wav_path.name,
                                     "audio_sha256": _hash(wav_path), "json_file": transcript_path.name,
                                     "json_sha256": _hash(transcript_path), "text_file": text_path.name,
                                     "text_sha256": _hash(text_path) if text_path.is_file() else None,
                                     "observed": observed, "normalized_observed": _normalize(observed),
                                     "expected_text": expected_text,
                                     "normalized_expected": _normalize(expected_text),
                                     "automated_result": "match" if _normalize(observed) == _normalize(expected_text) else "mismatch",
                                     "estimated_word_timings": word_times,
                                     "timing_reliability": "rough estimates only; not calibrated phoneme or lip-sync timing"}
        review["transcriptions"] = transcriptions
        review["word_timing_comparison"] = {
            "words": _compare_word_timings(transcriptions["source"]["estimated_word_timings"],
                                             transcriptions["converted"]["estimated_word_timings"],
                                             expected_text=expected_text),
            "method": "Whisper token offsets and t_dtw token-alignment timestamps; integers interpreted as 10 ms units",
            "use_limit": "Rough token alignment only, not a word center/onset/end or calibrated speech boundary; cannot establish lip sync or acceptance."}
        review["commands"] = {"source_framemd5": remux["source_framemd5_command"],
                              "converted_framemd5": remux["converted_framemd5_command"],
                              "source_ffprobe": [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(source_video)],
                              "converted_ffprobe": [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(mp4)],
                              "alac_decode": remux["alac_decode_command"], "aac_decode": remux["aac_decode_command"]}
        review["status"] = ("review-ready" if all(item["automated_result"] == "match"
                                                   for item in transcriptions.values()) else "failed-transcript")
        review["review_ready_means"] = "Technical review artifacts are complete; human review remains pending."
    except BaseException as exc:
        review.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                      error=str(exc) or type(exc).__name__, error_type=type(exc).__name__)
        save()
        raise
    review["completed_at"] = datetime.now(timezone.utc).isoformat()
    review["artifacts"] = {path.name: _hash(path) for path in sorted(output_dir.iterdir())
                            if path.is_file() and path.name != "review.json"}
    save()
    return review


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path, help="validated prepared run with completed conversion")
    args = parser.parse_args(argv)
    try:
        result = review_voice_conversion(args.run_dir)
    except KeyboardInterrupt:
        return 130
    except Exception as exc:
        print(f"Voice conversion review failed: {exc}", file=sys.stderr, flush=True)
        return 1
    print(json.dumps({"status": result["status"], "review": str(args.run_dir / "voice-review/review.json")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
