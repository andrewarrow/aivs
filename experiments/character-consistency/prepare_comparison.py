"""Prepare a controlled milestone comparison while reusing verified source audio."""

import argparse
from datetime import datetime
import json
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parents[2]
PYTHON = ROOT / "python"
if str(PYTHON) not in sys.path:
    sys.path.insert(0, str(PYTHON))

import character_milestone as milestone


def _repo_path(path: Path) -> Path:
    path = Path(path).expanduser()
    return path.resolve() if path.is_absolute() else (ROOT / path).resolve()


def _validate_source_cast_and_lines(source_manifest: dict, request: dict) -> dict:
    if source_manifest.get("cast") != request.get("cast"):
        raise ValueError("Source run cast does not match the current fixed milestone request.")

    role_to_character = {item["role"]: item["character"] for item in request["cast"]}
    expected_role = {}
    for line in request.get("lines", []):
        character = role_to_character.get(line.get("role"))
        if character is None:
            raise ValueError(f"Requested line has an unbound speaker role: {line.get('role')!r}.")
        key = (character, line.get("text"))
        if key in expected_role:
            raise ValueError("Requested lines do not uniquely identify source performances by speaker and text.")
        expected_role[key] = line["role"]

    matched = {}
    for key, role in expected_role.items():
        candidates = [(line_key, record) for line_key, record in source_manifest.get("lines", {}).items()
                      if (record.get("character"), record.get("text")) == key]
        if len(candidates) != 1:
            raise ValueError(f"Source run needs exactly one saved performance for {role} / {key[0]}.")
        line_key, record = candidates[0]
        if record.get("role") != role or record.get("character") != key[0]:
            raise ValueError(f"Source line {line_key!r} has a different speaker binding.")
        matched[key] = (line_key, record)
    return matched


def _validate_shot_inputs(source_manifest: dict, request: dict) -> None:
    source_shots = source_manifest.get("shots", [])
    requested_shots = request.get("shots", [])
    if len(source_shots) != len(requested_shots):
        raise ValueError(f"Comparison shot count changed: source has {len(source_shots)}, request has {len(requested_shots)}.")
    role_to_character = {item["role"]: item["character"] for item in request["cast"]}
    # The prompt is the only shot input allowed to change. Check all other
    # source-linked inputs before any audio is copied or any run is created.
    for index, (source_shot, shot) in enumerate(zip(source_shots, requested_shots, strict=True), 1):
        line_id = shot.get("line")
        line = next((item for item in request["lines"] if item.get("id") == line_id), None)
        if line is None:
            raise ValueError(f"Comparison shot {index} references an unknown dialogue line.")
        character = role_to_character.get(line.get("role"))
        source_line_key = source_shot.get("line")
        source_line = source_manifest.get("lines", {}).get(source_line_key, {})
        if (source_shot.get("index") != index
                or source_shot.get("scene") != shot.get("scene")
                or source_line.get("role") != line.get("role")
                or source_line.get("character") != character
                or source_line.get("text") != line.get("text")):
            raise ValueError(f"Comparison shot {index} order, scene, or dialogue differs from the source run.")
        if shot.get("image", {}).get("sha256") != source_shot.get("image", {}).get("sha256"):
            raise ValueError(f"Comparison shot {index} image hash differs from the source run.")


def _validate_prompt_only(source_run: Path, request: dict) -> None:
    source_request = json.loads((source_run / "request.json").read_text(encoding="utf-8"))
    original = json.loads(json.dumps(source_request))
    proposed = json.loads(json.dumps(request))
    for version in (original, proposed):
        for shot in version.get("shots", []):
            shot.pop("prompt", None)
    if original != proposed:
        raise ValueError("Comparison request may change shot prompts only.")


def _invalidate_prepared_run(run_dir: Path, error: Exception) -> None:
    manifest_path = run_dir / "manifest.json"
    if manifest_path.exists():
        failed_manifest = run_dir / "manifest.failed.json"
        if failed_manifest.exists():
            failed_manifest.unlink()
        manifest_path.replace(failed_manifest)
    report_path = run_dir / "render.json"
    report = json.loads(report_path.read_text(encoding="utf-8")) if report_path.exists() else {}
    report.update(status="failed", error=str(error))
    milestone._atomic_json(report_path, report)


def prepare_comparison(source_run: Path, run_dir: Path, *, request_file: Path | None = None) -> dict:
    source_run = _repo_path(source_run)
    run_dir = _repo_path(run_dir)
    source_manifest = milestone.verify(source_run)
    request_path = _repo_path(request_file) if request_file is not None else milestone.DEFAULT_REQUEST
    request = json.loads(request_path.read_text(encoding="utf-8"))
    matching_lines = _validate_source_cast_and_lines(source_manifest, request)
    _validate_prompt_only(source_run, request)
    _validate_shot_inputs(source_manifest, request)

    def reuse_performance(profile, text, output):
        key = (profile["id"], text)
        if key not in matching_lines:
            raise ValueError(f"No verified source performance for {profile['id']} / requested dialogue.")
        line_key, source_line = matching_lines[key]
        if source_line.get("voice", {}).get("voice_id") != profile.get("voice_id"):
            raise ValueError(f"Source performance {line_key!r} uses a different fixed voice.")
        source_audio = source_run / source_line["file"]
        shutil.copyfile(source_audio, output)
        if milestone.characters._digest(output) != source_line.get("sha256"):
            output.unlink(missing_ok=True)
            raise ValueError(f"Copied source performance {line_key!r} failed its checksum.")
        record = {**source_line,
                  "reused_from": {"source_sha256": source_manifest["source_sha256"], "line": line_key}}
        output.with_suffix(".json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return record

    prepared = False
    try:
        comparison = milestone.prepare(request_path, run_dir, speech_fn=reuse_performance)
        prepared = True
        before = source_manifest["shots"]
        after = comparison["shots"]
        if len(before) != len(after):
            raise ValueError(f"Comparison shot count changed: source has {len(before)}, new run has {len(after)}.")
        for index, (source_shot, comparison_shot) in enumerate(zip(before, after, strict=True), 1):
            if source_shot.get("index") != comparison_shot.get("index"):
                raise ValueError(f"Comparison shot ordering differs at position {index}.")
            for asset_name in ("image", "audio"):
                source_hash = source_shot.get(asset_name, {}).get("sha256")
                comparison_hash = comparison_shot.get(asset_name, {}).get("sha256")
                if source_hash != comparison_hash:
                    raise ValueError(f"Comparison shot {index} {asset_name} hash differs from the source run.")
        return comparison
    except Exception as exc:
        if prepared:
            _invalidate_prepared_run(run_dir, exc)
        raise


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_run", type=Path, help="Verified prepared/rendered source run")
    parser.add_argument("--request", type=Path, help="Comparison request JSON (defaults to the milestone request)")
    parser.add_argument(
        "--run-dir", type=Path,
        default=ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_character-comparison",
        help="New directory for the prepared comparison run",
    )
    args = parser.parse_args(argv)
    try:
        manifest = prepare_comparison(args.source_run, args.run_dir, request_file=args.request)
    except Exception as exc:
        print(f"Comparison preparation failed: {exc}", file=sys.stderr, flush=True)
        return 1
    print(f"Prepared comparison: {_repo_path(args.run_dir)} ({manifest['source_sha256']})", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
