"""Compatibility names for Rust visual observations and staging adaptation."""
from pathlib import Path
from production_adapter import call

POLICY_VERSION = "visual-continuity-v2"


def review_shot(plan, shot_index, clip, review_dir, **options):
    return call("review_shot", plan=plan, shot_index=shot_index, clip=Path(clip), directory=Path(review_dir), **options)


def review_scene(plan, shot_reviews, review_dir, **options):
    return call("review_scene", plan=plan, shot_reviews=shot_reviews, directory=Path(review_dir), **options)


def adapt_shot(plan, shot_index, previous_review, review_dir, **options):
    return call("adapt", plan=plan, shot_index=shot_index, previous_review=previous_review, directory=Path(review_dir), **options)
