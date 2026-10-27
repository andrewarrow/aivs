"""Compatibility adapter for Rust dialogue and screenplay action timing."""
import rust_engine


def duration_estimate(prompt: str) -> dict:
    return rust_engine.call("duration_estimate", prompt=prompt)
