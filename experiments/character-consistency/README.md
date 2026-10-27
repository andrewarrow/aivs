# Character consistency evidence and production voice worker

The character consistency, native-reference, fixed-audio, lip-sync, and voice
conversion experiments are complete. Their one-off render, comparison, and review
scripts have been removed. The JSON records, reference images, and generated runs
remain historical evidence; they are not current reproduction instructions.

The saved results include:

- [Original character acceptance](acceptance.json) and [human review](human-review.json).
- [Native Lena control](native-reference-control.json) and [native Owen control](native-reference-owen.json).
- [Lena voice conversion](voice-conversion-lena.json) and [Owen voice conversion](voice-conversion-owen.json).
- [Lip-sync trial](lip-sync-trial.json) and [reference provenance](reference-provenance.json).

Acceptance applies to the recorded takes. It does not establish quality for new
scenes or dialogue. Historical records retain the commands and source hashes used
at the time, including paths to scripts that have since been retired.

The following tools remain active because Generate Video uses the pinned Seed-VC
converter to apply saved voices to native LTX soundtracks:

- `setup_voice_conversion.py` prepares its separate Python environment and source checkout.
- `download_voice_models.py` downloads the pinned weights in `seed-vc-models.json`.
- `run_voice_conversion.py` checks the converter or converts one soundtrack. Rust
  owns scheduling, the workspace lock, take recovery, and video publication.
- `requirements-seed-vc.txt` pins the converter's Python dependencies.

Desktop setup installs these automatically. To prepare them from the CLI:

```sh
.venv/bin/python experiments/character-consistency/setup_voice_conversion.py
```

New filmmaking runs use the [main screenplay workflow](../../README.md#run-your-first-scene).
The optional Kokoro environment and the completed A/B/C production-comparison
harness are no longer supported entry points.
