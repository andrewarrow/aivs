"""Compatibility names for the Rust image-generation controller."""
from pathlib import Path
from production_adapter import call

POLICY = "codex-imagegen-v2"
MAX_REFERENCE_IMAGES = 5


def configuration():
    return call("image_config")


def check_local():
    return call("image_local")


def check(**options):
    return call("image_check", **options)


def generate(prompt, references, output, seed, **options):
    return call("image_generate", prompt=prompt, references=[Path(p) for p in references],
                output=Path(output), seed=seed, **options)


def edit(edit_target, correction, preserve, references, output, seed, **options):
    return call("image_generate", prompt="Apply only the requested correction.",
        edit_target=Path(edit_target), correction=correction, preserve=preserve,
        references=[Path(p) for p in references], output=Path(output), seed=seed, **options)


if __name__ == "__main__":
    import argparse
    import json
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check",))
    parser.add_argument("--refresh", action="store_true")
    args = parser.parse_args()
    print(json.dumps(check(refresh=args.refresh), indent=2))
