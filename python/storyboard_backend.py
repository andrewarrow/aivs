"""Generate reference-aware storyboard images through the signed-in Codex app-server."""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
from pathlib import Path
import subprocess
import time

import shooting_script
from quality_backend import ROOT

POLICY = "codex-imagegen-v1"
GENERATION_TIMEOUT_SECONDS = 1200
_SETUP = "Update Codex CLI and sign in through Codex → Sign in with ChatGPT, then Resume."


def configuration() -> dict:
    return {"backend": "codex", "policy": POLICY, "transport": "app-server",
            "image_generation": "built-in", "model": "codex-configured",
            "seed_supported": False, "normalization": "lanczos-cover-v1"}


def check() -> dict:
    """Check CLI availability without generating an image or requiring local weights."""
    executable = shooting_script._executable(None)
    try:
        result = subprocess.run([executable, "--version"], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RuntimeError(f"Cannot start Codex for storyboard images. {_SETUP}") from exc
    if result.returncode:
        raise RuntimeError(f"Cannot start Codex for storyboard images. {_SETUP}")
    return {**configuration(), "codex_version": result.stdout.strip()}


def _bootstrap_directory():
    # Match Studio's app-server bootstrap: keep workspace configuration and tools
    # available while the individual thread works in an isolated image folder.
    for start in (ROOT, Path.cwd()):
        for directory in (start, *start.parents):
            if (directory / ".codex-root").is_file():
                return directory
    return ROOT


def _instruction(prompt, references, width, height, composition=None):
    description = prompt + ("\nComposition: " + composition if composition else "")
    images = "\n".join(f"Image {index}: {path}" for index, path in enumerate(references, 1))
    return (
        "$imagegen\nCreate exactly one original raster photograph using your built-in image generation tool. "
        "Produce a single opaque image, not a contact sheet or a description. "
        f"Compose for a {width} × {height}px frame, with the same aspect ratio and orientation. "
        "Keep important faces, hands, full bodies when requested, and complete props clear of the edges "
        "so the image can be fitted to the exact video dimensions. "
        "Do not use an API key, another image service, SVG, or programmatic placeholder artwork. "
        "If image generation is unavailable, report that explicitly.\n\n"
        + ("Use ALL of these attached local reference images in this order as actual image-tool inputs. "
           "Preserve their identities and follow the role mapping in the brief. "
           "References establish appearance, not current pose, clothing, visibility or placement.\n"
           + images + "\n\n" if references else "")
        + "Image brief:\n" + description
    )


def _image_bytes(item):
    failure = item.get("failure") or {}
    if failure.get("type") == "usageLimitExceeded":
        raise RuntimeError("Codex image-generation usage limit reached. Resume when your image allowance resets.")
    if item.get("status") != "completed":
        raise RuntimeError("Codex image generation did not complete. See the saved image-generation log.")
    saved = item.get("savedPath")
    if saved and Path(saved).is_file():
        return Path(saved).read_bytes()
    encoded = item.get("result")
    if isinstance(encoded, str) and encoded:
        if encoded.startswith("data:"):
            encoded = encoded.split(",", 1)[-1]
        try:
            return base64.b64decode(encoded, validate=True)
        except ValueError as exc:
            raise RuntimeError("Codex returned invalid image bytes.") from exc
    raise RuntimeError(f"Codex returned no saved image or image bytes. {_SETUP}")


def generate(prompt: str, references: list[Path], output: Path, seed: int, *,
             width: int, height: int, composition: str | None = None, on_activity=None) -> dict:
    """Accept actual image-tool output, then preserve and normalize its pixels for LTX."""
    from PIL import Image, ImageOps

    output = Path(output).resolve()
    if output.exists():
        raise FileExistsError(f"Refusing to overwrite storyboard image: {output}")
    if type(width) is not int or type(height) is not int or min(width, height) <= 0:
        raise ValueError("Storyboard dimensions must be positive integers.")
    references = [Path(path).resolve(strict=True) for path in references]
    expected_inputs = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in references}
    identity = check()
    instruction = _instruction(prompt, references, width, height, composition)
    binding = {"backend": configuration(), "instruction": instruction, "inputs": expected_inputs,
               "width": width, "height": height, "requested_seed": seed}
    fingerprint = hashlib.sha256(json.dumps(binding, sort_keys=True).encode()).hexdigest()
    workspace = output.parent / (output.stem + ".codex-" + fingerprint[:16])
    workspace.mkdir(parents=True, exist_ok=True)
    request_path = workspace / "request.txt"
    request_path.write_text(instruction, encoding="utf-8")
    checkpoint = workspace / "images.json"
    images, thread = {}, {}
    activity = on_activity or (lambda message: None)
    started = time.monotonic()

    def receive_image(item):
        previous = images.get(item["id"])
        if previous and previous.get("status") == "completed" and not item.get("result") and not item.get("savedPath"):
            item = dict(previous, **{key: value for key, value in item.items()
                                    if value is not None and key != "savedPath"})
        images[item["id"]] = item
        if item.get("status") == "completed":
            data = _image_bytes(item)
            artifact = workspace / (hashlib.sha256(item["id"].encode()).hexdigest()[:16] + ".image")
            if artifact.exists():
                if artifact.read_bytes() != data:
                    raise ValueError("A completed Codex image changed during generation.")
            else:
                with artifact.open("xb") as stream:
                    stream.write(data)
            images[item["id"]] = {key: value for key, value in item.items() if key != "result"}
            images[item["id"]].update(savedPath=str(artifact), artifact_sha256=hashlib.sha256(data).hexdigest())
            shooting_script._atomic_json(checkpoint, {
                "binding": binding, "thread": thread,
                "images": [row for row in images.values() if row.get("status") == "completed"],
            })

    if checkpoint.is_file():
        saved = json.loads(checkpoint.read_text(encoding="utf-8"))
        if saved.get("binding") != binding:
            raise ValueError("Saved Codex image belongs to different storyboard inputs.")
        for item in saved["images"]:
            data = _image_bytes(item)
            if hashlib.sha256(data).hexdigest() != item["artifact_sha256"]:
                raise ValueError("Saved Codex image bytes changed.")
            images[item["id"]] = item
        thread.update(saved.get("thread", {}))
        # Preserve a rejected tool result, but let Resume request a corrected
        # generation instead of reusing an image that omitted its references.
        if any({str(Path(row["path"]).resolve()): row["sha256"] for row in item.get("inputImages", [])}
               != expected_inputs for item in images.values()):
            checkpoint.rename(workspace / f"images.rejected-{time.time_ns()}.json")
            images.clear()
            thread.clear()
    if not images:
        attempt = workspace / f"attempt-{time.time_ns()}"
        attempt.mkdir()
        try:
            shooting_script._stream_codex(
                [shooting_script._executable(None), "app-server", "--enable", "image_generation"],
                instruction, cwd=workspace, bootstrap_cwd=_bootstrap_directory(),
                deadline=started + GENERATION_TIMEOUT_SECONDS,
                events_path=attempt / "events.jsonl", errors_path=attempt / "errors.txt",
                response_path=attempt / "response.txt",
                on_activity=activity, on_activity_delta=None, image_paths=references,
                image_generation=True, on_image=receive_image, on_thread=thread.update,
            )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError("Codex storyboard image generation timed out. Saved work is preserved; Resume to retry.") from exc
    else:
        activity("Reusing the completed Codex image-generation checkpoint")
    completed = [item for item in images.values() if item.get("status") == "completed"]
    if not completed and images:
        _image_bytes(next(reversed(images.values())))
    if len(completed) != 1:
        raise RuntimeError(f"Codex must return exactly one generated image; received {len(completed)}. "
                           f"See the logs in {workspace}. {_SETUP}")
    item = completed[0]
    image_bytes = _image_bytes(item)
    observed_inputs = {str(Path(row["path"]).resolve()): row["sha256"]
                       for row in item.get("inputImages", [])}
    if expected_inputs != observed_inputs:
        raise RuntimeError("Codex did not confirm use of exactly the supplied visual references. "
                           f"The generated image was not accepted; see {checkpoint}. {_SETUP}")
    for path in references:
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected_inputs[str(path)]:
            raise ValueError("A storyboard reference changed during Codex image generation.")
    with Image.open(io.BytesIO(image_bytes)) as decoded:
        original = ImageOps.exif_transpose(decoded).convert("RGB")
        source_size = original.size
        normalized = ImageOps.fit(original, (width, height), method=Image.Resampling.LANCZOS)
        with output.open("xb") as stream:
            normalized.save(stream, format="PNG")
    return {**identity, "pipeline": "reference-image-generation" if references else "text-to-image",
            "prompt": prompt + ("\nComposition: " + composition if composition else ""),
            "generation_directory": str(workspace), "requested_seed": seed,
            "thread_id": thread.get("thread", {}).get("id"),
            "model": thread.get("model", "codex-configured"), "model_provider": thread.get("modelProvider"),
            "image_generation_id": item["id"], "revised_prompt": item.get("revisedPrompt"),
            "input_images": item.get("inputImages", []),
            "original_image": {"file": str(Path(item['savedPath']).relative_to(output.parent)),
                               "width": source_size[0], "height": source_size[1],
                               "sha256": hashlib.sha256(image_bytes).hexdigest()},
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "sha256": hashlib.sha256(output.read_bytes()).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check",))
    parser.parse_args()
    print(json.dumps(check(), indent=2))
