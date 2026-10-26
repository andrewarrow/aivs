# Reusable characters without sacrificing lip sync

## Decision

Keep the goal of reusable performers with consistent appearance and voice. Change
the order in which we prove the rendering architecture.

Start from the joint audio/video generation path whose synchronization the user
liked at commit `084d048887f47a54eb959196d19d4944903a9615`. Add canonical visual
references, then test whether voice conversion can give the resulting performance
the saved performer's voice **without changing its words or local timing**.

This is a hypothesis to test, not a claim that voice conversion already works.
Neither the frozen-audio prototype nor the proposed conversion path becomes the
default until a small end-to-end comparison passes human audiovisual review.

This plan revises the architecture and execution order in [PLAN.md](PLAN.md).
Retain that document and the existing takes as the history of the first approach.

## What the review established

- The older renderer generated picture and sound together with `DistilledPipeline`
  at 1280×704 and 24 fps. The current GUI still uses that path. Its model/runtime
  revisions and 8+3 denoising steps have not changed; restoring it does not require
  reverting the newer character, provenance, or validation work.
- The character prototype uses `A2VidDistilledPipeline`, fixed Kokoro speech,
  reference images, and 768×448 four-second shots. Several factors changed, so the
  comparison alone does not establish which factor causes the poorer sync.
- Fixed-audio preservation works: decoded soundtracks correlate above 0.9999 with
  supplied audio. The conditioning track preserves the original PCM and adds
  silence only at the end. The frame-grid trim also removes only the tail.
  Neither result proves that mouths follow the speech.
- The user rejected lip synchronization in all four caption-free prototype shots.
  A prompt-only shot-1 retake was judged **“Improved, but still unacceptable.”**
  These are failed acceptance results, not pending approvals or successful fixes.
- Source inspection found no demonstrated timeline error or disabled audio
  conditioning. `generate_audio=False` skips audio decoding; the A2V transformer
  still receives frozen audio. Arbitrary stream offsets are not an established fix.
- The native-reference control and voice-conversion experiment described below
  have **not been run**. The preserved native output from the earlier period lacks
  a Git SHA, so it cannot be identified conclusively as the exact take the user
  remembers.

Evidence: [four-shot review](experiments/character-consistency/acceptance.json),
[human feedback and clip hashes](experiments/character-consistency/human-review.json),
and [prompt-only trial](experiments/character-consistency/lip-sync-trial.json).

## What stays

- Explicit role-to-performer binding, such as `SUE -> lena` and `JOE -> owen`.
- Canonical portraits and voice references, versioned separately from scene
  wardrobe, expression, blocking, and camera direction.
- Scene starting images grounded in those portraits. A location or camera change
  must not silently invent a new identity.
- Immutable run assets, checksums, pinned models/runtime, reproducible commands,
  honest render/review status, process cancellation, and the workspace GPU lock.
- Exact source dialogue, transcript checks, caption checks, face/voice review, and
  explicit human lip-sync acceptance.
- Existing A2V and Kokoro code as an opt-in comparison path. Its failure is useful
  evidence; it should not be deleted or silently substituted into another mode.
- The recovery fixes from the review: clear unpublished partial video before a
  retry, and invalidate comparison runs that fail their input checks.

## What changes

### Generate the performance together

Use native joint audio/video generation to produce the mouth movements and spoken
performance together, anchored by the scene reference. Include the exact dialogue
and the intended delivery in the shooting prompt. Check the resulting words,
silences, captions, likeness, and synchronization before processing its voice.

Native generation is not guaranteed to say the script correctly. A line with
missing words, extra speech, or bad sync is a failed take. Voice conversion cannot
repair that performance by replacing it with independently timed TTS.

For the first experiments, use one clearly visible speaker and clean speech with
minimal background sound. Keep music and designed ambience separate. If generated
sound contaminates the speech, evaluate separation and its artifacts explicitly;
do not run the entire mixed soundtrack through a voice converter by assumption.

### Convert speaker identity while retaining performance

For an accepted native take, use speech-to-speech voice conversion with the saved
performer's canonical voice sample as its target. The desired result changes
speaker identity while retaining phonemes, pauses, emphasis, and syllable timing.
The native video frames remain untouched.

The target is the character's saved voice, not merely a similar age, accent, or
gender. Check that the imported voice sample is sufficiently clean and useful.
If a longer reference is needed, prepare and version one from the same pinned
Kokoro voice ID; keep the earlier reference and provenance. Never use successive
converted takes as new canonical references.

Start with an offline converter that supports this Apple Silicon environment.
Seed-VC is one candidate: its project documents Mac installation, reference-based
conversion, and length/style controls. Those controls do not guarantee local
phoneme alignment. The repository is archived, so evaluate its pinned dependencies
and actual local operation before adopting it. Use an isolated environment and
pin code, models, configuration, and reference hashes. Avoid style/accent rewriting
in the first experiment. [Seed-VC documentation](https://github.com/Plachtaa/seed-vc)

Do not infer sync from equal file durations. Compare the starts and ends of words,
internal pauses, and conspicuous mouth closures before and after conversion.
Record any measured codec/converter delay and its correction. Reject variable
timing drift rather than masking it with a guessed global offset or stretching
the finished performance.

### Preserve identity rather than requiring identical speech waveforms

The user needs the same performer across scenes. Separate performances may have
different timing and emotion while sharing a canonical voice identity.

Accordingly, byte-identical pre-synthesized dialogue is no longer mandatory for
every camera angle. Cache accepted native picture/audio pairs and their converted
soundtracks by source-audio and video hashes, canonical voice-reference hashes,
converter/model revisions, and settings; dialogue text alone is not a cache key.
If an edit requires the exact same performance
across different generated shots, that remains a separate capability to prove
with stronger A2V or validated lip correction; do not claim it from voice conversion.

Estimate scene lengths for generation, then measure the accepted native performance
and finalize edit timing. Preserve source dialogue order and complete action beats.

## Milestone A: recover sync with a saved face

**Immediate next step:** render one native joint-audio shot of Lena using the
existing desert starting image and the line “We still have some time.”

Use the installed backend, seed 83, 24 fps, a four-second output, and the old
production resolution of 1280×704. Preserve the model/runtime pins and use the
existing workspace lock. Save the complete prompt, original image, canonical
portrait, source hashes, native audio, and resulting video. Label it
`diagnostic-native-voice`; it does not satisfy fixed-voice acceptance.

This is a production-quality checkpoint. It changes both audio mode and resolution
relative to the rejected prototype, so an improvement does not isolate their
individual contributions. Optimize resolution only after recovering quality.

- [ ] Produce the native-reference control without modifying default GUI behavior.
- [ ] Verify the exact words independently and inspect for generated captions.
- [ ] Compare face identity against the canonical portrait throughout the clip.
- [ ] Obtain human audiovisual acceptance of this take's lip sync.

If it fails, use at most two additional diagnostic renders, changing one factor
at a time. One useful control removes the image anchor at the same resolution,
prompt, and seed to test whether image conditioning is implicated. Such a control
has no claim to character consistency. Record the outcome before selecting any
further approach; do not start another four-shot batch of speculative prompts.

**Exit condition:** a saved-face native take has correct dialogue, acceptable
likeness, no unwanted captions, and accepted synchronization. Its generated voice
remains temporary diagnostic audio.

## Milestone B: prove voice conversion on the accepted take

Reuse Milestone A's exact video and native speech. No new video inference is
needed to test the converter.

- [ ] Snapshot the canonical Lena voice reference and validate its quality.
- [ ] Run a pinned converter in an isolated environment. Initially keep source
      timing and delivery; disable optional style/accent changes where supported.
- [ ] Save source/converted WAVs, model and reference hashes, settings, measured
      durations, word timing evidence, and any explicit delay correction.
- [ ] Remux the converted speech with the unchanged video frames. Verify that
      video frame hashes match and the mux uses the intended converted waveform.
- [ ] Independently check the transcript, clipped words, added speech, silence,
      and conversion artifacts. Listen against the canonical voice sample.
- [ ] Obtain human review of original versus converted video: voice must match
      the target, and lip sync must remain acceptable throughout the sentence.
- [ ] Repeat with Owen to test both voices before expanding the scene matrix.

Try one conservative configuration per performer first; allow at most two
documented adjustments before evaluating another method. A conversion failure
must leave the take failed or awaiting review. Never silently publish the native
generated voice as the saved performer.

**Exit condition:** both performers have accepted target voices and retain the
native take's accepted sync. Matching duration, transcript, or waveform checks
alone cannot satisfy this condition.

## Milestone C: consistency across scenes and new dialogue

Repeat the original two-location matrix with one visible speaker per shot:

| Shot | Setting | Performer |
| --- | --- | --- |
| 1 | Desert road at dusk | Lena / Sue |
| 2 | Desert road at dusk | Owen / Joe |
| 3 | Cafe in daylight | Lena / Sue |
| 4 | Cafe in daylight | Owen / Joe |

Use the same canonical visual and voice identities in both locations. Begin with
the same text to make comparisons easier, allowing native delivery timing to vary.
Then test different lines, including a longer sentence, pauses, and a different
emotion. Check that varying the native generated voice does not leak a different
speaker identity through conversion.

Accept face identity, voice identity, exact words, captions, and sync separately
for every shot. Unknown or obscured evidence is not a pass. Keep rejected takes
and retry only the affected stage: conversion failures should not require another
video render; bad native speech or mouth motion does.

**Exit condition:** both locations and new dialogue pass human audiovisual review.
This still does not establish overlapping speech, tiny faces, profile views, or
multi-person speaker assignment.

## Milestone D: integrate the proven path

Only after the preceding experiments pass:

1. Add the selected voice-conversion stage to the per-shot pipeline with explicit
   modes, provenance, cancellation, bounded retries, and failure status.
2. Separate stable identity from scene styling in the character schema, retaining
   compatibility with existing profiles.
3. Plan dialogue shots around one visible speaker initially; allow silent wider
   interaction shots. Measure accepted performances before final assembly.
4. Add independently tested transcript, speaker, face, and synchronization checks.
   Calibrate them on accepted/rejected takes; retain uncertain and human-review
   states rather than inventing universal confidence thresholds.
5. Surface compact preparation, conversion, and review status in the GUI. Keep the
   existing working render path available and make the chosen mode explicit.

Relevant tests must cover mismatched voice/reference hashes, missing models,
conversion failure, changed words or timing, accidental reuse of the wrong take,
preserved video frames, final audio selection, cancellation, and interrupted retry.
Run the existing Python/Rust checks as appropriate to the integration changes.

## If the main hypothesis fails

Do not relax the voice or synchronization requirements to make a milestone pass.
Choose the next bounded experiment according to the observed failure:

| Failure | Next experiment |
| --- | --- |
| Native reference video loses the old sync quality | Compare image/no-image and resolution controls using the same joint-generation path. |
| Conversion preserves sync but misses the target voice | Improve the canonical reference or evaluate another pinned converter on the same accepted native take. |
| Conversion matches the voice but alters local timing | Evaluate a different timing-preserving converter; if that fails, compare a stronger fixed-audio A2V or face-specific lip-correction path. |
| A fixed recorded performance must be reused exactly across new shots | Evaluate guided A2V or a validated dubbing stage with that exact waveform. |

Check actual model compatibility, local memory, dependencies, and expected render
cost before selecting a fallback. The MLX documentation distinguishes guided A2V
from experimental distilled A2V; its current LipDub path targets 2.3, so it is not
a drop-in fix for the installed 2.5 model. These are candidates to evaluate, not
promised solutions. [MLX pipeline guide](https://github.com/dgrauet/ltx-2-mlx/blob/main/docs/PIPELINES.md)

## Completion standard

A character must look like the saved performer, sound like the saved performer,
say the intended words, and have convincing audiovisual synchronization across
scenes. Preserve the working joint-generation baseline while proving each added
identity constraint. A technically valid file or a stable voice alone is insufficient.
