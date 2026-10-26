# Character consistency prototype

An opt-in four-shot test of Lena as Sue and Owen as Joe, on a desert road and in a
cafe. Each shot has one visible speaker. The starting images were generated from
the original portraits; `reference-provenance.json` records their exact prompts.
`request.json` pins portrait and starting-image checksums. Existing GUI screenplay
rendering continues to use its current path while this experiment is evaluated.

## Prepare speech and reference assets

The optional CPU voice environment avoids changing the pinned MLX dependencies.
The model download is about 354 MB. It uses no paid service or access token.

```sh
uv venv .venv-voice --python .venv/bin/python
uv pip install --python .venv-voice/bin/python -r python/requirements-voice.txt
.venv-voice/bin/python python/speech.py setup
.venv/bin/python python/character_milestone.py prepare
```

Preparation prints a new directory under `outputs/`. It snapshots the characters,
synthesizes each line once, saves model/voice provenance and actual durations,
copies the shot references, and pads each shot track with 350 ms of opening
silence and at least 650 ms after the line. Both locations reuse identical line
audio. No video inference occurs during preparation.

## Verify and render

Use the directory printed by preparation:

```sh
.venv/bin/python python/character_milestone.py verify --run-dir outputs/<run>
.venv/bin/python python/character_milestone.py render --run-dir outputs/<run> --shot 1
.venv/bin/python python/character_milestone.py render --run-dir outputs/<run>
```

Rendering takes the existing workspace render lock. It fails clearly if the GUI
is using the GPU; it does not stop that operation. It validates asset checksums
and the installed video runtime before inference. The baseline uses 768x448 at
24 fps, with four-second shots for these short lines. Both stages receive frozen
audio conditioning. The final mux selects the original shot soundtrack, avoiding
generated voices and audio-VAE reconstruction. The extra causal-grid inference
frames receive silence padding and are trimmed from the finished take.

Completed takes can be resumed with the same seed. Changed inputs, assets, model
versions, or seeds cannot silently reuse a previous take. All four completed
shots are assembled into `video.mp4`; individual clips and reports remain under
`shots/`. A failed later shot preserves earlier takes.

## Review criteria

`rendered-needs-review` means media validation passed, not that identity is proven.
Compare the original portraits with the beginning, middle, and end of each take;
listen to the original `lines/*.wav` beside the videos. Check both locations,
speaking mouth shapes, eye and face geometry, voice identity, spoken words, and
silence after dialogue. Speaker intervals are recorded in `manifest.json`.

Automated checks currently cover checksums, role/voice binding during preparation,
soundtrack duration, exact PCM reuse between scenes, pipeline selection, causal
padding, final audio selection, decoded video integrity, timing, and correlation
of the decoded soundtrack with the supplied PCM performance. Face
similarity, speaker similarity, transcription, and lip-sync still need review.
This experiment does not establish multi-person dialogue or arbitrary-angle
identity preservation. See [PLAN.md](../../PLAN.md) for later milestones.

Initial visual finding: the model sometimes adds subtitles despite the prompt.
Explicit transcript prompting changed the staging-text subtitle into the actual
line, but did not remove it. These are comparison takes pending review, not a
production-quality acceptance result.
