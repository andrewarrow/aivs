"""Generate reference-aware storyboard images through the signed-in Codex app-server."""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import time

import shooting_script
from quality_backend import ROOT

POLICY = "codex-imagegen-v2"
MAX_REFERENCE_IMAGES = 5
PREFLIGHT_MAX_AGE_SECONDS = 86400
GENERATION_TIMEOUT_SECONDS = 1200
_SETUP = "Update Codex CLI and sign in through Codex → Sign in with ChatGPT, then Resume."


def _backend():
    selected = os.environ.get("AIVS_STORYBOARD_BACKEND", "codex-imagegen")
    if selected not in ("codex-imagegen", "mflux"):
        raise ValueError("AIVS_STORYBOARD_BACKEND must be codex-imagegen or mflux.")
    return selected


def configuration() -> dict:
    if _backend() == "mflux":
        import storyboard_mflux
        return storyboard_mflux.configuration()
    return {"backend": "codex", "policy": POLICY, "transport": "app-server",
            "image_generation": "built-in", "model": "codex-configured",
            "seed_supported": False, "normalization": "lanczos-contain-v1"}


def _cli_identity() -> dict:
    executable = shooting_script._executable(None)
    try:
        result = subprocess.run([executable, "--version"], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RuntimeError(f"Cannot start Codex for storyboard images. {_SETUP}") from exc
    if result.returncode:
        raise RuntimeError(f"Cannot start Codex for storyboard images. {_SETUP}")
    home = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    # Store hashes only: account/config changes invalidate a successful probe.
    files = [home / "auth.json", home / "config.toml", ROOT / ".codex/config.toml",
             _bootstrap_directory() / ".codex/config.toml"]
    return {**configuration(), "codex_version": result.stdout.strip(), "executable": executable,
            "configuration_sha256": hashlib.sha256(json.dumps({str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                if p.is_file() else None for p in files}, sort_keys=True).encode()).hexdigest()}


def check(*, refresh=False, on_activity=None) -> dict:
    """Prove generation and reference editing through the production transport."""
    if _backend() == "mflux":
        import storyboard_mflux
        return storyboard_mflux.check()
    import fcntl

    identity = _cli_identity()
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    directory = ROOT / ".cache" / "imagegen-preflight" / key
    directory.mkdir(parents=True, exist_ok=True)
    receipt = directory / "verified.json"
    activity = on_activity or (lambda message: None)
    with (directory / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if receipt.is_file() and not refresh:
            try:
                saved = json.loads(receipt.read_text())
                if (saved["identity"] == identity
                        and 0 <= time.time() - saved["verified_at"] < PREFLIGHT_MAX_AGE_SECONDS
                        and len(saved["images"]) == 2
                        and all(hashlib.sha256(Path(row["file"]).read_bytes()).hexdigest() == row["sha256"]
                                for row in saved["images"])):
                    return dict(identity, preflight=str(receipt))
            except (OSError, ValueError, KeyError, TypeError):
                pass
        # Isolate a refresh; preserve both failed attempts and past successes.
        pending = directory / "pending.json"
        if pending.is_file() and not refresh:
            attempt = Path(json.loads(pending.read_text())["directory"])
            if not attempt.resolve().is_relative_to(directory.resolve()):
                raise ValueError("Invalid preflight attempt directory.")
        else:
            attempt = directory / f"attempt-{time.time_ns()}"
            attempt.mkdir()
            shooting_script._atomic_json(pending, {"directory": str(attempt)})
        images = []
        for name in ("generate", "edit"):
            output = attempt / f"{name}.png"
            metadata = attempt / f"{name}.json"
            if not metadata.is_file():
                if output.exists():
                    output.rename(attempt / f"{name}.interrupted-{time.time_ns()}.png")
                activity(f"Checking real Codex image {name} capability")
                try:
                    runtime = _generate_codex(
                        "A single ceramic cup centered on a plain white table, "
                        + ("blue." if name == "edit" else "red."), [], output, 0,
                        width=256, height=256, on_activity=activity, identity=identity,
                        **({"edit_target": attempt / "generate.png",
                            "correction": "Change only the cup color from red to blue.",
                            "preserve": ["cup shape", "framing", "table", "lighting"]} if name == "edit" else {}))
                except RuntimeError as exc:
                    raise RuntimeError(f"Codex ImageGen preflight {name} failed with {identity['codex_version']}: "
                                       f"{exc} Saved diagnostics: {attempt}") from exc
                shooting_script._atomic_json(metadata, runtime)
            runtime = json.loads(metadata.read_text())
            if hashlib.sha256(output.read_bytes()).hexdigest() != runtime["sha256"]:
                raise ValueError("Saved preflight image changed; run check --refresh.")
            images.append({"file": str(output), "sha256": runtime["sha256"]})
        if images[0]["sha256"] == images[1]["sha256"]:
            raise RuntimeError("Codex returned unchanged pixels for the preflight edit; inspect the saved attempt "
                               "and run storyboard_backend.py check --refresh.")
        shooting_script._atomic_json(receipt, {"identity": identity, "verified_at": time.time(), "images": images})
        pending.unlink(missing_ok=True)
    return dict(identity, preflight=str(receipt))


def _bootstrap_directory():
    # Match Studio's app-server bootstrap: keep workspace configuration and tools
    # available while the individual thread works in an isolated image folder.
    for start in (ROOT, Path.cwd()):
        for directory in (start, *start.parents):
            if (directory / ".codex-root").is_file():
                return directory
    return ROOT


def _instruction(prompt, references, width, height, composition=None, *, edit_target=None,
                 correction=None, preserve=()):
    description = prompt + ("\nComposition: " + composition if composition else "")
    images = "\n".join(f"Tool input {index}: {path}" for index, path in enumerate(references, 1))
    return (
        "$imagegen\n" + ("Edit the FIRST attached image using your built-in image generation tool. "
            f"Correction: {correction}\nPreserve unchanged: {', '.join(preserve)}. "
            "Keep every correct region intact; other inputs are identity/geometry references. " if edit_target else
            "Create exactly one original raster photograph using your built-in image generation tool. ") +
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


def _prepare_inputs(references, output, *, keep_first=False):
    """Preserve priority inputs; consolidate remaining sources without dropping any."""
    from PIL import Image, ImageDraw, ImageOps

    sources = [{"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in references]
    if len(references) <= MAX_REFERENCE_IMAGES:
        return references, sources, "\nImage numbers in the brief match tool input numbers." if references else ""
    keep = min(3, len(references))
    while keep >= int(keep_first) and len(references) - keep > (MAX_REFERENCE_IMAGES - keep) * 4:
        keep -= 1
    if keep < int(keep_first):
        raise ValueError("Too many essential references for five readable image inputs; simplify this shot.")
    folder = output.parent / "reference-sheets"
    folder.mkdir(parents=True, exist_ok=True)
    inputs = list(references[:keep])
    mapping = [f"Image {n}: tool input {n}." for n in range(1, keep + 1)]
    for start in range(keep, len(references), 4):
        group = sources[start:start + 4]
        digest = hashlib.sha256(json.dumps(group, sort_keys=True).encode()).hexdigest()
        path = folder / f"{digest}.png"
        # Large panels retain face/prop detail; sheets are references, never output frames.
        sheet = Image.new("RGB", (1536, 1600), "white")
        draw = ImageDraw.Draw(sheet)
        for offset, row in enumerate(group):
            x, y = offset % 2 * 768, offset // 2 * 800
            with Image.open(row["path"]) as source:
                tile = ImageOps.contain(ImageOps.exif_transpose(source).convert("RGB"), (768, 768),
                                        method=Image.Resampling.LANCZOS)
                sheet.paste(tile, (x + (768 - tile.width) // 2, y + 32 + (768 - tile.height) // 2))
            draw.text((x + 12, y + 8), f"Image {start + offset + 1}", fill="black")
            mapping.append(f"Image {start + offset + 1}: tool input {len(inputs) + 1}, panel {offset + 1} "
                           "(panels read left to right, top to bottom).")
        buffer = io.BytesIO()
        sheet.save(buffer, format="PNG")
        data = buffer.getvalue()
        if path.exists() and path.read_bytes() != data:
            raise ValueError("A saved reference sheet changed.")
        if not path.exists():
            temporary = path.with_suffix(".partial.png")
            temporary.write_bytes(data)
            temporary.replace(path)
        shooting_script._atomic_json(path.with_suffix(".json"), {"sources": group, "sha256": hashlib.sha256(data).hexdigest()})
        inputs.append(path.resolve())
    return inputs, sources, ("\nReference mapping: all Image numbers in the brief identify source images, "
        "mapped to tool inputs/panels below. Sheets contain separate references; do not reproduce panels or labels.\n"
        + "\n".join(mapping))


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
    if _backend() == "mflux":
        return _generate_mflux(prompt, references, output, seed, width=width, height=height,
                               composition=composition, on_activity=on_activity)
    identity = check(on_activity=on_activity)
    return _generate_codex(prompt, references, output, seed, width=width, height=height,
                           composition=composition, on_activity=on_activity, identity=identity)


def edit(edit_target: Path, correction: str, preserve: list[str], references: list[Path],
         output: Path, seed: int, *, width: int, height: int, composition=None, on_activity=None) -> dict:
    """Target a rejected image while explicitly protecting its correct regions."""
    if not correction.strip() or not preserve:
        raise ValueError("Image edits need a specific correction and preservation constraints.")
    if _backend() == "mflux":
        target = Path(edit_target).resolve(strict=True)
        inputs = [target, *[Path(p).resolve(strict=True) for p in references if Path(p).resolve() != target]]
        return _generate_mflux(
            "Edit the first image. " + correction + " Preserve unchanged: " + ", ".join(preserve),
            inputs, output, seed, width=width, height=height, composition=composition, on_activity=on_activity,
            keep_first=True)
    identity = check(on_activity=on_activity)
    return _generate_codex("Apply only the requested correction.", references, output, seed,
                           width=width, height=height, composition=composition, on_activity=on_activity,
                           identity=identity, edit_target=edit_target, correction=correction, preserve=preserve)


def _generate_mflux(prompt, references, output, seed, *, width, height, composition=None,
                    on_activity=None, keep_first=False):
    import storyboard_mflux

    output = Path(output).resolve()
    if output.exists():
        raise FileExistsError(f"Refusing to overwrite storyboard image: {output}")
    if type(width) is not int or type(height) is not int or min(width, height) <= 0:
        raise ValueError("Storyboard dimensions must be positive integers.")
    paths = [Path(p).resolve(strict=True) for p in references]
    inputs, sources, mapping = _prepare_inputs(paths, output, keep_first=keep_first)
    runtime = storyboard_mflux.generate(prompt + mapping, inputs, output, seed, width=width, height=height,
                                        composition=composition, on_activity=on_activity)
    for row in sources:
        if hashlib.sha256(Path(row["path"]).read_bytes()).hexdigest() != row["sha256"]:
            raise ValueError("A storyboard reference changed during local generation.")
    return dict(runtime, source_inputs=sources, reference_mapping=mapping)


def _generate_codex(prompt, references, output, seed, *, width, height, composition=None,
                    on_activity=None, identity, edit_target=None, correction=None, preserve=()):
    """Accept actual image-tool output, then preserve and normalize its pixels for LTX."""
    from PIL import Image, ImageOps

    output = Path(output).resolve()
    if output.exists():
        raise FileExistsError(f"Refusing to overwrite storyboard image: {output}")
    if type(width) is not int or type(height) is not int or min(width, height) <= 0:
        raise ValueError("Storyboard dimensions must be positive integers.")
    references = [Path(path).resolve(strict=True) for path in references]
    if edit_target is not None:
        edit_target = Path(edit_target).resolve(strict=True)
        references = [edit_target, *[p for p in references if p != edit_target]]
    references, source_inputs, mapping = _prepare_inputs(references, output, keep_first=edit_target is not None)
    if len(references) > MAX_REFERENCE_IMAGES:
        raise ValueError("Image generation accepts at most five actual reference inputs.")
    expected_inputs = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in references}
    instruction = _instruction(prompt + mapping, references, width, height, composition,
                               edit_target=edit_target, correction=correction, preserve=preserve)
    binding = {"backend": configuration(), "instruction": instruction, "inputs": expected_inputs,
               "source_inputs": source_inputs, "identity": identity,
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
        if previous:
            item = dict(previous, **{key: value for key, value in item.items() if value is not None})
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
        if any({str(Path(row["path"]).resolve()): row["sha256"] for row in (item.get("inputImages") or [])}
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
                       for row in (item.get("inputImages") or [])}
    if expected_inputs != observed_inputs:
        raise RuntimeError(f"Codex {identity['codex_version']} did not confirm use of exactly the supplied visual references "
                           "through imageGeneration.inputImages path/SHA-256 metadata. This app-server protocol "
                           "is unsupported for reference generation; update Codex and run storyboard_backend.py check --refresh. "
                           f"The generated image was not accepted; see {checkpoint}. {_SETUP}")
    for source in [*source_inputs, *[{"path": p, "sha256": d} for p, d in expected_inputs.items()]]:
        if hashlib.sha256(Path(source["path"]).read_bytes()).hexdigest() != source["sha256"]:
            raise ValueError("A storyboard reference changed during Codex image generation.")
    with Image.open(io.BytesIO(image_bytes)) as decoded:
        original = ImageOps.exif_transpose(decoded).convert("RGB")
        source_size = original.size
        content = ImageOps.contain(original, (width, height), method=Image.Resampling.LANCZOS)
        left, top = (width - content.width) // 2, (height - content.height) // 2
        normalized = Image.new("RGB", (width, height), "black")
        normalized.paste(content, (left, top))
        with output.open("xb") as stream:
            normalized.save(stream, format="PNG")
    return {**identity, "pipeline": "targeted-edit" if edit_target else "reference-image-generation" if references else "text-to-image",
            "edit_target": str(edit_target) if edit_target else None, "correction": correction, "preserve": list(preserve),
            "source_inputs": source_inputs, "reference_mapping": mapping,
            "normalized_content_box": [left, top, left + content.width, top + content.height],
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
    parser.add_argument("--refresh", action="store_true", help="Repeat the real generation/edit capability probe")
    args = parser.parse_args()
    print(json.dumps(check(refresh=args.refresh), indent=2))
