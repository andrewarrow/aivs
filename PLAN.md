# Reusable characters: consistent appearance and voice

## Goal

Casting a saved performer must bind their visual references and voice to every
shot. A character should remain recognizable across locations, camera angles,
roles, and separate renders. Text descriptions support performance and styling;
they are not the source of facial or vocal identity.

Generative video can drift. The system must distinguish a technically valid
render from a take whose identity, speech, and synchronization have been reviewed.
It must never silently fall back to text-only casting when identity assets fail.

## Current implementation

- `characters/` already stores portraits, scene images, voice samples, descriptions,
  and voice IDs. Each render snapshots profiles and verifies asset hashes.
- Screenplay rendering appends appearance/voice text to prompts but does not use
  the saved photos or voice samples as conditioning.
- Shot continuation can use the previous shot's last frame; this cannot establish
  identity for a new scene or reliably recover an obscured face.
- `quality_backend.py` uses `DistilledPipeline` to invent picture and sound
  together. The installed runtime also provides `A2VidDistilledPipeline`, which
  accepts an input soundtrack and image anchors and retains the original waveform.
- The shooting-script fingerprint currently omits reference-image and audio hashes.
- Validation checks media integrity and timing, not facial identity, spoken words,
  speaker identity, or which visible face is speaking.

## Design

### 1. Persistent performer identity

Keep the role-to-performer binding (`SUE -> lena`) separate from scene state.
The performer owns canonical photos, additional views when available, a fixed
voice definition, and an identity version. A scene owns wardrobe, hair styling,
location, props, expression, blocking, and camera direction. The current imported
descriptions include wardrobe and expression; retain compatibility while migrating
these into separate fields.

Fingerprint all conditioning assets and model/runtime versions. Include them in
cached-plan/render provenance. Continue immutable per-run snapshots. Never replace
canonical identity references with generated frames from later takes.

### 2. Dialogue before video

Synthesize each source line using its performer's saved voice and store the actual
waveform, exact text, speaker, engine/model identity, settings, and checksums.
The existing imported performers use Kokoro voice IDs; use those fixed voices
first. A future recorded-voice backend can bind reference audio plus its transcript.
Emotion and pacing are performance controls and must not select a new voice.

Measure synthesized audio, then finalize shot lengths, pauses, and action timing.
Use clean dialogue to condition video. Preserve that waveform in the final video;
mix ambience, music, and effects separately. Cache line performances by their full
voice/text/settings identity so camera changes do not re-synthesize dialogue.

### 3. Visual references for every shot

For the first prototype, create actual scene starting images from the original
portraits using reference-aware image generation. Explicitly map roles to the
photos, check likeness, then animate each starting image with prepared dialogue.
A camera or location change needs a newly grounded image. Carry previous frames
only where the shot calls for it and re-anchor against the canonical references.

Evaluate LTX-2.5 Ingredients reference-sheet conditioning as a stronger next step.
It accepts character, prop, and location references, but its documented training
bucket is 768x448, 121 frames at 24 fps. Longer/larger clips and combined fixed-audio
conditioning need an experiment; the current app exposes neither this adapter nor
its combination with A2V. A raw portrait montage is not a valid scene first frame.

### 4. Speaker-to-face assignment

Keep a timed speaker manifest alongside the audio. Bind each active speaker to a
visible character/face track. Begin with one clearly visible speaker per dialogue
shot and alternate camera angles. Enable two-person dialogue shots only after
speaker assignment works, or use a validated face-specific lip-sync stage.
Single-subject face/voice personalization research does not establish multi-speaker
support. Preserve wider silent interaction shots where useful.

### 5. Acceptance and recovery

Sample and track faces throughout each clip and compare against original identity
references. Check speaker similarity, the spoken words, mouth/audio synchronization,
and silent listeners. Calibrate thresholds on actual characters and camera angles;
an obscured or tiny face is uncertain, not a pass. Keep review evidence with each
shot. Retry only failed shots within a limit and expose unresolved review findings.
Never claim that media-format validation proves character consistency.

## Milestone 1: Lena and Owen in two locations

Build a small opt-in experiment before changing default GUI screenplay rendering.

Test matrix:

| Shot | Location | Visible speaker | Framing |
| --- | --- | --- | --- |
| 1 | Desert road at dusk | Sue / Lena | Medium close-up, looking screen right |
| 2 | Desert road at dusk | Joe / Owen | Reverse medium close-up, looking screen left |
| 3 | Cafe in daylight | Sue / Lena | Closer three-quarter view |
| 4 | Cafe in daylight | Joe / Owen | Reverse three-quarter view |

Reuse each performer's exact line audio across both locations to isolate visual
consistency first. Use new dialogue in a subsequent check. Start with short clips;
only raise duration/resolution after reviewing the baseline. Keep the original
portraits and voices available next to results for comparison.

Deliverables and acceptance:

- [ ] Include all reference-asset hashes in shooting-script freshness checks.
- [ ] Add fixed-audio conditioning to the existing backend without changing native
      audio defaults, progress reporting, validation, or process lifetime.
- [ ] Prepare real dialogue using Lena's and Owen's saved voice IDs; record model
      hashes, measured durations, line hashes, and speaker timestamps.
- [ ] Create four scene starting images from canonical portraits and save their
      prompts/provenance in the project.
- [ ] Provide a repeatable preparation/render command with immutable run assets,
      clear failure states, and no silent fallback to generated voices.
- [ ] Render and inspect the four shots; compare the original audio and faces.
- [ ] Record which checks are automatic and which still require human review.
- [ ] Exercise failure paths with meaningful offline tests and run the relevant
      existing Python suite.

Do not contend with a GUI render already holding the workspace GPU lock. Asset
preparation and CPU voice synthesis may proceed; video inference waits for the
existing operation to finish.

## Later milestones

1. Integrate measured dialogue timing and shot references into arbitrary screenplay
   planning; separate identity from wardrobe/performance fields in the schema.
2. Validate reference-sheet plus audio conditioning, multi-person speaker binding,
   longer takes, and occlusion recovery on the installed MLX backend.
3. Add calibrated face/speaker/transcript/synchronization checks and bounded retakes.
4. Connect the proven workflow to the GUI with compact preparation/review status.

## Primary references

- [Kokoro voices](https://huggingface.co/hexgrad/Kokoro-82M/blob/main/VOICES.md)
- [MLX A2V pipelines and maturity](https://github.com/dgrauet/ltx-2-mlx/blob/main/docs/PIPELINES.md)
- [LTX-2.5 Ingredients](https://huggingface.co/Lightricks/LTX-2.5-22b-IC-LoRA-Ingredients)
- [ID-LoRA and current multi-speaker limits](https://arxiv.org/html/2603.10256v1)
