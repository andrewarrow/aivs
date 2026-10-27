# Reusable characters without sacrificing lip sync

Historical investigation plan. The experiments below are complete; their one-off
render, comparison, and review tools, including Kokoro, have been retired. Saved
evidence and media remain. This supersedes the original instruction to retain
the comparison code. See the [archive](experiments/character-consistency/README.md)
for preserved records and the [README](README.md) for the current production path.

## Decision

Keep the goal of reusable performers with consistent appearance and voice. Change
the order in which we prove the rendering architecture.

Start from the joint audio/video generation path whose synchronization the user
liked at commit `084d048887f47a54eb959196d19d4944903a9615`. Add canonical visual
references, then test whether voice conversion can give the resulting performance
the saved performer's voice **without changing its words or local timing**.

Voice conversion has passed human review for one Lena performance and one Owen
performance. Milestone B is complete for those takes; this does not establish new
dialogue or scene-to-scene consistency. Neither the frozen-audio prototype nor
conversion becomes the default until both performers pass the planned scene and
dialogue review.

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
- The user accepted the native-reference control's lip sync, likeness, and spoken
  words after watching it with sound. Existing visual review and all-frame OCR
  found no unwanted captions. Its generated voice remains diagnostic.
  [Run evidence](experiments/character-consistency/native-reference-control.json)
  records the result. Milestone A passed. A pinned voice conversion has now
  passed human review for Lena on this line and Owen on “Then let's keep going.”
  Broader scene consistency and new dialogue remain untested.
  The preserved native output from the earlier period lacks a Git SHA, so it
  cannot be identified conclusively as the exact take the user remembers.

Evidence: [four-shot review](experiments/character-consistency/acceptance.json),
[human feedback and clip hashes](experiments/character-consistency/human-review.json),
and [prompt-only trial](experiments/character-consistency/lip-sync-trial.json).

The user confirmed that both accepted native sources contain clean speech with
quiet around it. Both converted takes retained their source video frames and were
accepted for voice match, lip sync, words, and audio quality. Evidence is in the
[Lena conversion record](experiments/character-consistency/voice-conversion-lena.json)
and [Owen conversion record](experiments/character-consistency/voice-conversion-owen.json).

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

Milestone A passed. Lena's voice-reference preparation and first conversion
review are also complete; see Milestone B below.

The initial diagnostic renders one native joint-audio shot of Lena using the
existing desert starting image and the line “We still have some time.”

Use the installed backend, seed 83, 24 fps, a four-second output, and the old
production resolution of 1280×704. Preserve the model/runtime pins and use the
existing workspace lock. Save the complete prompt, original image, canonical
portrait, source hashes, native audio, and resulting video. Label it
`diagnostic-native-voice`; it does not satisfy fixed-voice acceptance.

This is a production-quality checkpoint. It changes both audio mode and resolution
relative to the rejected prototype, so an improvement does not isolate their
individual contributions. Optimize resolution only after recovering quality.

- [x] Produce the native-reference control without modifying default GUI behavior.
- [x] Verify the exact words independently and inspect for generated captions.
- [x] Compare face identity against the canonical portrait throughout the clip.
- [x] Obtain human audiovisual acceptance of this take's lip sync, likeness, and
      spoken words.

The first control rendered successfully in 237.81 seconds. Whisper independently
recovered the exact line, and all-frame OCR found no text in 96 frames. Visual
comparison of every face frame with the canonical portrait found no obvious
identity drift. The user watched with sound and accepted the lip sync, likeness,
and spoken words in the [native-reference clip](outputs/2026-10-26_native-reference-control/video.mp4).
The run preserves its prompt, references, code snapshots,
hashes, raw native media, and a PCM WAV decoded from the final AAC soundtrack.
See the [recorded result](experiments/character-consistency/native-reference-control.json)
and [archived investigation](experiments/character-consistency/README.md).
Milestone A has passed. Its generated voice remains temporary diagnostic audio;
the accepted converted Lena take is recorded separately under Milestone B. This
native control itself remains diagnostic and does not satisfy fixed-voice
acceptance.

If it fails, use at most two additional diagnostic renders, changing one factor
at a time. One useful control removes the image anchor at the same resolution,
prompt, and seed to test whether image conditioning is implicated. Such a control
has no claim to character consistency. Record the outcome before selecting any
further approach; do not start another four-shot batch of speculative prompts.

**Exit condition:** a saved-face native take has correct dialogue, acceptable
likeness, no unwanted captions, and accepted synchronization. Its generated voice
remains temporary diagnostic audio.

## Milestone B: prove voice conversion on the accepted take

Milestone B passed for one short desert performance per performer. Owen's
[native control](experiments/character-consistency/native-reference-owen.json) was
accepted for lip sync, likeness, words, captions, and clean source speech. His
conversion using the canonical `am_michael` sample was then accepted for voice
match, lip sync, words, and audio quality.

**Immediate next step:** Milestone C, completing the two-location matrix with new
cafe counterparts to the accepted desert takes, using the same canonical
identities and initial dialogue, then testing new lines.

Reuse the accepted native video and speech. Lena's first conversion was accepted
for the exact line “We still have some time.” No new video inference was needed.
That acceptance applies to this Lena performance. Owen's separate accepted
performance is recorded below; neither proves new dialogue or scene consistency.

- [x] Lena: snapshot the canonical voice reference and record PCM quality
      measurements; the measurements are descriptive, not a perceptual pass.
- [x] Lena: run the pinned V1 converter in isolation with seed 83, 30 steps,
      CFG 0.7, length adjustment 1.0, and no F0 or pitch adjustment.
- [x] Lena: save source and converted audio, model/reference hashes, settings,
      durations, and approximate word-alignment evidence.
- [x] Lena: remux the converted speech with unchanged video frames. All 96 frame
      hashes and timing match the source; no offset or stretch was applied.
- [x] Lena: independently check words, clipping, added speech, silence, and
      conversion artifacts, and compare against the canonical voice.
- [x] Lena: the user accepted voice match, lip sync, and audio quality for this
      converted performance.
- [x] Repeat with Owen, starting with an accepted native Owen take.

The Lena conversion took 23.70 seconds. Its PCM16 derivative is 88,064 samples
at 22,050 Hz (3.993832 seconds); only 136 zero samples (6.17 ms) were appended
to reach four seconds. The raw float output is preserved, no normalization,
clipping, time stretch, or offset was used. The remuxed AAC correlates 0.999760
with the converted PCM at zero offset; ALAC decoding is exact. Both independent
transcripts match the source line. Whisper DTW estimates token alignment
differences of 0–40 ms; these estimates are not word onsets or proof of sync.
The user separately accepted the audible voice match and lip sync. See the
[run record](experiments/character-consistency/voice-conversion-lena.json) and
[reviewed converted clip](outputs/2026-10-26_voice-conversion-lena/voice-review/converted-video.mp4).

Owen's native control took 239.98 seconds at the same seed 83, 1280×704, 24 fps,
and four-second duration. Independent transcription recovered “Then let's keep
going.” and all-frame OCR found no text in 96 frames. The user accepted the
performance and confirmed clean speech with quiet around it.

Owen's conversion took 32.91 seconds using the same pinned V1 configuration and
his original 4.700-second `am_michael` reference. The raw float output is retained;
the PCM16 derivative has 88,064 samples at 22,050 Hz with no clipping. Only 136
zero samples (6.17 ms) were appended at the end. All 96 decoded video frame hashes
and their timing match the native take. AAC correlation is 0.999922 at zero offset;
ALAC decoding is exact. Both transcripts match. Corrected Whisper token alignment
differences are 0–20 ms; a preserved addendum fixes contraction matching for
“let's,” without changing any media. These estimates are not proof of sync.
The user separately accepted the [converted Owen clip](outputs/2026-10-26_voice-conversion-owen/voice-review/converted-video.mp4)
for voice match, lip sync, words, and audio quality. Full provenance and review are
in the [Owen conversion record](experiments/character-consistency/voice-conversion-owen.json).

Fixed-voice acceptance is true for these two reviewed takes only. Milestone B is
complete; it does not enable a default GUI path or establish general conversion
or scene-consistency capability.

Try one conservative configuration per performer first; allow at most two
documented adjustments before evaluating another method. A conversion failure
must leave the take failed or awaiting review. Never silently publish the native
generated voice as the saved performer.

**Exit condition met:** both performers have accepted target voices and retain
their native take's accepted sync for one performance each. Human audiovisual
review established this; matching duration, transcript, approximate alignment,
or waveform checks alone cannot satisfy the condition.

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
