"""Opt-in LTX-2.5 Pro comparison renderer using the documented V2 async API.

https://docs.ltx.io/api-documentation/api-reference/async-video-generation/submit-image-to-video
https://docs.ltx.io/async-jobs

Submitting creates a paid job. Checkpoints deliberately stop an uncertain POST
from being submitted again. GET polling and downloading can safely be resumed.
"""

from __future__ import annotations

import base64
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen

import quality_backend
from validation import validate_video

API_ROOT = "https://api.ltx.io/v2/image-to-video"
MODEL = "ltx-2-5-pro"
FPS = 24
_RESOLUTIONS = {(1280, 720), (1920, 1080), (720, 1280), (1080, 1920)}


def profile_config(model=MODEL):
    if model != MODEL:
        raise ValueError(f"This comparison adapter supports the documented model {MODEL}.")
    return {"backend": "ltx-cloud", "model": model, "api_version": "v2",
            "endpoint": API_ROOT, "fps": FPS, "generate_audio": True,
            "seed_supported": False, "model_revision": None}


def validate_request(prompt, frames, width, height, *, model=MODEL):
    """Validate before a paid request; return provider dimensions and padding."""
    profile_config(model)
    if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 5000:
        raise ValueError("LTX Pro needs a nonempty prompt of at most 5000 characters.")
    if type(frames) is not int or frames not in (6 * FPS, 8 * FPS, 10 * FPS):
        raise ValueError("LTX-2.5 Pro comparison shots must be exactly 6, 8, or 10 seconds.")
    if type(width) is not int or type(height) is not int:
        raise ValueError("Cloud output dimensions must be integers.")
    # The normal assembler crops an aligned MLX canvas to delivery dimensions.
    # Preserve provider pixels by padding that canvas, never stretching them.
    dimensions = {(1280, 768): (1280, 720), (1088, 1920): (1080, 1920)}.get(
        (width, height), (width, height))
    if dimensions not in _RESOLUTIONS:
        raise ValueError("LTX Pro comparison supports 720p/1080p landscape or portrait delivery.")
    provider_width, provider_height = dimensions
    return {"width": provider_width, "height": provider_height,
            "pad_left": (width - provider_width) // 2,
            "pad_top": (height - provider_height) // 2}


def _atomic_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent,
                                     prefix=f".{path.name}.", delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(data, stream, indent=2)
        stream.write("\n")
    temporary.replace(path)


def _sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Never send the API credential to a redirect destination.
        raise HTTPError(req.full_url, code, "API redirect refused", headers, fp)


def _api_request(url, api_key, payload=None):
    body = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = Request(url, data=body,
                      headers={"Authorization": f"Bearer {api_key}",
                               "Content-Type": "application/json"},
                      method="POST" if payload is not None else "GET")
    attempts = 1 if payload is not None else 4
    for attempt in range(attempts):
        try:
            with build_opener(_NoRedirect()).open(request, timeout=60) as response:
                result = json.load(response)
            if not isinstance(result, dict):
                raise ValueError("LTX returned an invalid JSON object.")
            return result
        except HTTPError as exc:
            if attempt + 1 < attempts and (exc.code >= 500 or exc.code == 429):
                time.sleep(5 * 2**attempt)
                continue
            # Provider bodies can echo prompts or signed media URIs. Keep
            # credentials and asset URLs out of console errors and manifests.
            raise RuntimeError(f"LTX API returned HTTP {exc.code}.") from None
        except (URLError, TimeoutError, OSError):
            if attempt + 1 < attempts:
                time.sleep(5 * 2**attempt)
                continue
            raise RuntimeError("LTX API request could not complete; resume the saved job.") from None


def _download(url, destination):
    if not isinstance(url, str) or urlsplit(url).scheme != "https":
        raise ValueError("LTX result did not contain an HTTPS download URL.")
    partial = destination.with_suffix(".download.partial")
    for attempt in range(4):
        try:
            # This request has no API authorization header.
            with urlopen(url, timeout=60) as response, partial.open("wb") as stream:
                if urlsplit(response.url).scheme != "https":
                    raise ValueError("LTX download redirected away from HTTPS.")
                shutil.copyfileobj(response, stream)
            if partial.stat().st_size == 0:
                raise ValueError("LTX downloaded an empty video.")
            partial.replace(destination)
            return
        except HTTPError as exc:
            if attempt < 3 and (exc.code >= 500 or exc.code == 429):
                time.sleep(5 * 2**attempt)
                continue
            raise RuntimeError(f"LTX video download returned HTTP {exc.code}; resume to refresh its URL.") from None
        except (URLError, TimeoutError, OSError):
            if attempt < 3:
                time.sleep(5 * 2**attempt)
                continue
            raise RuntimeError("LTX video download failed; the saved job can be resumed.") from None


def render(prompt, seed, frames, output, *, image_file=None, width=1280, height=720,
           on_progress=None, model=MODEL, hold_image_framing=False, audio_file=None,
           keep_native=False, timeout_seconds=3600):
    """Render one candidate. ``seed`` is recorded but never sent as an API field."""
    if image_file is None:
        raise ValueError("Premium comparison requires a storyboard starting image.")
    if audio_file is not None:
        raise ValueError("Premium comparison generates native audio; supplied audio is unsupported.")
    if hold_image_framing:
        raise ValueError("Premium comparison uses a first-frame storyboard, not a repeated last frame.")
    if (type(timeout_seconds) not in (int, float) or not math.isfinite(timeout_seconds)
            or timeout_seconds <= 0):
        raise ValueError("Cloud timeout must be positive.")
    dimensions = validate_request(prompt, frames, width, height, model=model)
    image = quality_backend.prepare_source_image(image_file)
    source_path = Path(image["source_file"])
    from PIL import Image

    with Image.open(source_path) as picture:
        mime = {"PNG": "image/png", "JPEG": "image/jpeg", "WEBP": "image/webp"}.get(picture.format)
    if mime is None:
        raise ValueError("LTX storyboard must be PNG, JPEG, or WEBP.")
    image_bytes = source_path.read_bytes()
    if hashlib.sha256(image_bytes).hexdigest() != image["source_sha256"]:
        raise ValueError("Storyboard image changed while preparing the cloud request.")
    encoded = f"data:{mime};base64," + base64.b64encode(image_bytes).decode("ascii")
    if len(encoded) > 7_000_000:
        raise ValueError("LTX storyboard exceeds the 7 MB encoded data URI limit.")
    api_key = os.environ.get("LTX_API_KEY", "").strip()
    if not api_key:
        raise RuntimeError("Set LTX_API_KEY before running the premium comparison.")
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    checkpoint = output.parent / "cloud-job.json"
    native = output.parent / "video.cloud.mp4"
    config = profile_config(model)
    request = {"prompt": prompt, "model": model, "duration": frames // FPS,
               "resolution": f"{dimensions['width']}x{dimensions['height']}",
               "fps": FPS, "generate_audio": True}
    identity = {"config": config, "request": request, "source_image_sha256": image["source_sha256"],
                "requested_seed": seed, "output_width": width, "output_height": height}
    fingerprint = hashlib.sha256(json.dumps(identity, sort_keys=True).encode("utf-8")).hexdigest()
    if checkpoint.exists():
        state = json.loads(checkpoint.read_text(encoding="utf-8"))
        if not isinstance(state, dict) or state.get("fingerprint") != fingerprint:
            raise ValueError("Cloud candidate inputs changed; use a new candidate directory.")
        if not isinstance(state.get("job_id"), str) or not state["job_id"]:
            raise RuntimeError("Cloud submission has no recorded job ID and may have been charged. "
                               "Check the LTX console before starting a new candidate; automatic resubmission is disabled.")
        if state.get("status") == "failed":
            raise RuntimeError("The saved LTX job failed; use a new candidate to request another generation.")
    else:
        state = {"version": 1, "fingerprint": fingerprint, "inputs": identity,
                 "status": "submitting", "job_id": None}
        _atomic_json(checkpoint, state)
        # A crash or timeout across this boundary cannot establish whether the
        # provider accepted and charged the job. Never retry the POST.
        job = _api_request(API_ROOT, api_key, {**request, "image_uri": encoded})
        if not isinstance(job.get("id"), str) or not job["id"]:
            raise RuntimeError("LTX submission did not return a job ID; check the provider console.")
        state.update(status="submitted", job_id=job["id"], created_at=job.get("created_at"))
        _atomic_json(checkpoint, state)

    started = time.monotonic()
    native_hash = _sha256(native) if native.is_file() else None
    if native_hash and state.get("download_sha256") and state["download_sha256"] != native_hash:
        raise ValueError("Saved cloud source video changed; use a new candidate directory.")
    if not (native_hash and state.get("download_sha256") == native_hash):
        status_url = f"{API_ROOT}/{quote(state['job_id'], safe='')}"
        while True:
            if time.monotonic() - started > timeout_seconds:
                raise TimeoutError("LTX job is still saved; resume this candidate to continue polling.")
            result = _api_request(status_url, api_key)
            status = result.get("status")
            if status not in ("pending", "processing", "completed", "failed"):
                raise RuntimeError("LTX returned an unrecognized job status; the job remains saved.")
            state["status"] = status
            _atomic_json(checkpoint, state)
            if status == "failed":
                raise RuntimeError("LTX could not generate this candidate; inspect the job in the provider console.")
            if status == "completed":
                outputs = result.get("result")
                video_url = outputs.get("video_url") if isinstance(outputs, dict) else None
                _download(video_url, native)
                state["download_sha256"] = _sha256(native)
                _atomic_json(checkpoint, state)
                break
            if on_progress is not None:
                on_progress(label=f"LTX Pro: {status}", fraction=0.25, remaining_seconds=60)
            time.sleep(random.uniform(5, 6))

    # Keep the raw provider result for provenance. Technical validation cannot
    # establish acting quality or script fidelity; those remain review inputs.
    native_info = validate_video(native, frames, dimensions["width"], dimensions["height"], expect_audio=True)
    partial = output.with_name("video.partial.mp4")
    filters = (f"pad={width}:{height}:{dimensions['pad_left']}:{dimensions['pad_top']}:black,"
               "setsar=1,setpts=PTS-STARTPTS")
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i", str(native),
        "-map", "0:v:0", "-map", "0:a:0", "-vf", filters, "-frames:v", str(frames),
        "-af", f"apad,atrim=duration={frames / FPS},asetpts=PTS-STARTPTS", "-t", str(frames / FPS),
        "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-movflags", "+faststart", str(partial),
    ], check=True, capture_output=True, text=True)
    info = validate_video(partial, frames, width, height, expect_audio=True)
    partial.replace(output)
    state.update(status="complete", output_sha256=_sha256(output))
    _atomic_json(checkpoint, state)
    return {**config, "pipeline": "cloud-image-to-video", "audio": "generated",
            "requested_seed": seed, "applied_seed": None, "job_id": state["job_id"],
            "output_frames": frames, "source_image": image, "source_audio": None,
            "provider_dimensions": dimensions, "native_video": native_info,
            "native_file": native.name, "native_sha256": state["download_sha256"],
            "elapsed_seconds": round(time.monotonic() - started, 2), "video": info}
