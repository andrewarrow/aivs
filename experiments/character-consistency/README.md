# Character consistency prototype

An opt-in four-shot test of Lena as Sue and Owen as Joe, on a desert road and in a
cafe. Each shot has one visible speaker. The starting images were generated from
the original portraits; `reference-provenance.json` records their exact prompts.
`request.json` pins portrait and starting-image checksums. Existing GUI screenplay
rendering continues to use its current path while this experiment is evaluated.

## Native-reference control (PLAN2 Milestone A)

The next experiment in [PLAN2.md](../../PLAN2.md) restores joint audio/video
generation with Lena's existing desert starting image:

```sh
.venv/bin/python python/native_reference.py
```

This renders one four-second, 1280×704 shot at 24 fps with seed 83 and the exact
line “We still have some time.” It uses the pinned `DistilledPipeline`, the
workspace GPU lock, and first-frame image conditioning. It does not change GUI
defaults. Every invocation requires a new output directory; an existing take
cannot be overwritten. An optional `--run-dir outputs/<new-run>` sets its location.

The `diagnostic-native-voice` run saves the complete prompt, source request,
original starting image, canonical character snapshots, code and dependency
snapshots, hashes, model/runtime revisions, raw `video.native.mp4`, and final
`video.mp4`. `native-audio.wav` is 48 kHz stereo PCM decoded from the final video's
AAC soundtrack, so it retains the reviewed take's performance; it is not the
pre-encoding waveform. The raw native output also remains available.

Run independent transcription, all-frame OCR, and face-sheet preparation with:

```sh
.venv/bin/python experiments/character-consistency/review_native.py outputs/<new-run>
```

This uses the existing cached Whisper model and Apple Vision, records their
provenance, and writes evidence under the run's `review/` directory. Review the
images against the canonical portrait and play the video with sound. Technical
checks alone leave human lip-sync acceptance pending. For the
[native-reference control](native-reference-control.json), the user accepted lip
sync, likeness, and spoken words; all-frame OCR and visual review found no captions.
The generated voice is
temporary diagnostic audio and does not satisfy fixed-voice acceptance. Lena's
canonical reference has since been prepared and used in the Milestone B trial
below.

## Voice-conversion experiment (PLAN2 Milestone B)

The experiment uses Seed-VC V1 with the accepted native take and Lena's original
`af_sarah` reference. Preparation verifies the acceptance record, media, review
evidence, and canonical character hashes before copying them to a new run.
It preserves the original reference and records PCM statistics; those statistics
do not establish perceptual quality or speaker identity.

Install the converter separately from both the MLX and Kokoro environments:

```sh
uv venv .cache/seed-vc-env --python 3.10.21
uv pip install --python .cache/seed-vc-env/bin/python \
  -r experiments/character-consistency/requirements-seed-vc.txt
git clone https://github.com/Plachtaa/seed-vc.git .cache/seed-vc
git -C .cache/seed-vc checkout --detach 51383efd921027683c89e5348211d93ff12ac2a8
.venv/bin/python experiments/character-consistency/download_voice_models.py
.venv/bin/python experiments/character-consistency/prepare_voice_reference.py \
  --run-dir outputs/<new-run>
cp outputs/2026-10-26_voice-conversion-lena/source-quality.json \
  outputs/<new-run>/source-quality.json
```

The copied source-quality confirmation is valid only when the new prepared
run's source video and audio hashes match that confirmation. The runner checks
both hashes before conversion and again afterward. For a different native take,
listen to that source first and record a new user confirmation in its run-local
`source-quality.json` with status `clean-speech-confirmed`, reviewer `user`, and
the exact source video/audio SHA-256 values; do not reuse this confirmation.

The [model manifest](seed-vc-models.json) pins each repository revision and file
hash. Downloads total about 1.9 GB. The runner requires the pinned source checkout
to be clean and runs offline under the workspace GPU lock. It saves the source
archive, dependency versions, model hashes, configuration, seed, and input hashes.

Before conversion, listen to the source with sound and record whether it is clean
speech. A run-local `source-quality.json` binds that human assessment to both
source media hashes. The current Lena run records the user's confirmation,
“Clean speech with quiet around it.” A source with interfering ambience needs
separate evaluation before conversion.

```sh
.cache/seed-vc-env/bin/python experiments/character-consistency/run_voice_conversion.py \
  --run-dir outputs/<new-run>
.cache/seed-vc-env/bin/python experiments/character-consistency/review_voice_conversion.py \
  outputs/<new-run>
```

The first configuration uses seed 83, 30 diffusion steps, CFG 0.7, length adjustment
1.0, and no F0/pitch adjustment. V1 does not run V2's optional accent/emotion
conversion stage. CFM and the vocoder use float32; the upstream Whisper encoder
uses float16. These settings are a timing-preservation hypothesis to evaluate.
Every conversion attempt requires a new prepared run, preserving failed attempts.

Review remuxes the converted audio with unchanged video frames and checks their
hashes. It preserves raw converter output, records any PCM conversion and tail
silence padding, independently transcribes source and converted speech, and saves
approximate word alignment evidence. Duration and transcript agreement do not
establish lip sync. Human comparison against the original video and canonical
voice remains required for each take. For the first Lena conversion, the user
accepted voice match, lip sync, and audio quality. The reviewed [converted clip](../../outputs/2026-10-26_voice-conversion-lena/voice-review/converted-video.mp4)
uses the accepted native take and the original `af_sarah` sample. The raw output
was preserved as float and measured 3.993832 seconds. Conversion took 23.70
seconds. The 88,064-sample, 22,050 Hz PCM16 derivative retains its level without
normalization or clipping, with only 136 zero samples (6.17 ms) appended to reach four seconds.
All 96 video frame hashes and their timing match the source. The remuxed AAC has
0.999760 correlation at zero offset; ALAC decoding is exact. Both transcripts
match the source line. DTW reports approximate token alignment differences of
0–40 ms, which are estimates rather than proof of synchronization. Full settings,
hashes, and review are in the [Lena conversion record](voice-conversion-lena.json).
Fixed-voice acceptance is true for this Lena performance only. Owen remains
pending, so Milestone B is incomplete and the GUI/default render path is unchanged.

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

To compare a prompt change using the exact original performances, prepare from
an existing run instead of synthesizing again:

```sh
.venv/bin/python experiments/character-consistency/prepare_comparison.py outputs/<original-run>
```

The helper prints a new run directory for the same verify/render commands. It
checks the source snapshots, role/voice/text bindings, and all four image and
padded-audio hashes. It copies the saved PCM and full speech provenance; a missing
or mismatched performance fails preparation. A failed comparison cannot be
verified or rendered; a late validation failure preserves its manifest as
`manifest.failed.json`. Existing runs remain immutable.

For a controlled prompt-only experiment, pass `--request <request.json>` to the
helper. Other request fields must match the source request. The current lip-sync
pilot changes only shot 1's performance directions:

```sh
.venv/bin/python experiments/character-consistency/prepare_comparison.py \
  outputs/2026-10-26_02-38-01-820168_clean-prompt \
  --request experiments/character-consistency/request-lip-sync.json
.venv/bin/python python/character_milestone.py render --run-dir outputs/<new-run> --shot 1
```

The default seed keeps shot 1 at 83. Evaluate this pilot before commissioning
more retakes; the changed directions are a hypothesis, not an accepted fix.

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
similarity, speaker similarity, and lip-sync still need review.
This experiment does not establish multi-person dialogue or arbitrary-angle
identity preservation. See [PLAN.md](../../PLAN.md) for later milestones.

The [16-second baseline comparison](../../outputs/2026-10-26_02-38-01-820168_clean-prompt/video.mp4)
uses visual performance directions without quoted dialogue or overlay prohibitions.
Words remain in the supplied audio and line manifest. All images, PCM tracks,
and seeds match the original comparison. OCR found zero text-bearing frames out
of 384, versus 159 in the original takes. Independent Whisper transcription
recovered every line exactly; all decoded soundtracks correlate above 0.9999
with their supplied PCM.

[acceptance.json](acceptance.json) records these run-specific checks and detailed
visual observations. The run also holds all-frame face sheets, 6fps whole-frame
samples, OCR/landmark output, review scripts, and transcription/model provenance.
Likeness is visually consistent in these views. Subsequent human audiovisual
review rejected lip synchronization in **all four shots**. The exact feedback
and reviewed video hashes are saved in [human-review.json](human-review.json).
The direction of the perceived timing error was not specified. The earlier
run-local review report remains historical evidence; `acceptance.json` records
the failed human review. Milestone 1 has not passed.

The subsequent [shot 1 prompt pilot clip](../../outputs/2026-10-26_02-57-28-419326_character-comparison/shots/001/video.mp4)
added performance directions while preserving the baseline's generic lip-sync
suffix and reusing the same image, PCM, conditioning audio, seed, dimensions,
frame rate, and model/runtime revisions. It rendered shot 1 only. The user's
response was “Improved, but still unacceptable,” so this remains a failed human
lip-sync review and milestone 1 remains false. The pilot is not an accepted fix;
the default prompt is unchanged. Hashes, prompt text, run provenance, OCR, and
independent transcription are recorded in [lip-sync-trial.json](lip-sync-trial.json).
The available [before/after review video](../../outputs/2026-10-26_02-57-28-419326_character-comparison/review/before-after.mp4)
contains the comparison. Automated OCR and Whisper checks do not replace human
audiovisual review; the assistant did not claim to listen to or judge the audio.
