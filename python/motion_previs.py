"""Compatibility entry points for Rust motion planning and keyframe production."""
from pathlib import Path
from production_adapter import call

POLICY = "motion-previsualization-v1"


def _run(command, run_dir, **options):
    return call("motion", command=command, run_dir=Path(run_dir), **options)


def identity():
    return call("motion_identity")


def plan_motion(run_dir, **options):
    return _run("plan", run_dir, **options)


def generate(run_dir, **options):
    return _run("generate", run_dir, **options)


def require_accepted(plan, boards, manifest, run_dir):
    return _run("accepted", run_dir, plan=plan, boards=boards, manifest=manifest)


def anchors(row, run_dir):
    return call("anchors", row=row, run_dir=Path(run_dir))


def animatic(run_dir, **options):
    return Path(_run("animatic", run_dir, **options))


def compare(run_dir, **options):
    return Path(_run("compare", run_dir, **options))


def main():
    import argparse
    from video import project_lock
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("plan", "generate", "animatic", "prepare", "compare"))
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("--shot", type=int, default=1)
    parser.add_argument("--profile", choices=("hq", "distilled"), default="distilled")
    args = parser.parse_args()
    with project_lock(".render.lock"):
        result = _run(args.command, args.run_dir, shot_number=args.shot, profile=args.profile, on_activity=print)
        if isinstance(result, str):
            print(result)


if __name__ == "__main__":
    main()
