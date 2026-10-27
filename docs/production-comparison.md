# Compare production paths

The comparison CLI renders complete scenes through three paths, keeping the
screenplay, dramatic requirements, reference images, and candidate schedule in a
saved experiment. It is separate from **Generate video** in the desktop app.

Normal **Generate video** now uses B's production approach: independent
storyboard first frames and local HQ inference. It adds automatic still-image
generation between two Codex planning passes. **Preview (distilled)** keeps the
same storyboard workflow. The experiment below remains useful for frozen
comparisons; running the optional paid C arm is unnecessary.

| Arm | Plan and visual input | Renderer |
| --- | --- | --- |
| A | Baseline plan, with the existing previous-frame handoffs | Local LTX distilled |
| B | Controlled plan, with an explicit starting image for every shot | Local LTX HQ, or distilled when selected |
| C | The same controlled plan and starting images as B | LTX-2.5 Pro API |

All three use generated native audio. The comparison omits saved-character voice
conversion and automatic prompt adaptation so those processes cannot change the
experiment between candidates. A represents the baseline plan and local renderer;
it does not reproduce every operation in the desktop workflow.

Storyboard frames override previous-shot frames. Each frame is bound to its shot
and recorded by checksum; changing a frame cannot silently reuse an old take.
This experimental CLI consumes prepared images. The desktop production workflow
generates them automatically, but neither workflow proves their artistic quality.
See the [production workflow and setup](../README.md#3-validate-render-and-assemble).

## Start with the included scene

The [example request](examples/production-comparison/request.json) describes an
18-second scene: Sue speaks, Joe replies, then Sue decides to keep going. It uses
existing desert reference frames for Lena and Owen from the
[character-consistency experiment](../experiments/character-consistency/README.md).
Its plans are hand-authored examples, not previously approved Codex plans or
evidence of successful footage.

From the repository root, freeze the example into a new directory:

```sh
.venv/bin/python python/production_comparison.py prepare \
  --request docs/examples/production-comparison/request.json \
  --output-dir outputs/production-comparison
```

Preparation copies inputs and records their hashes. It does not render video or
submit cloud jobs. Use a different output directory for a changed experiment.

App setup downloads the required HQ full-model transformer alongside the distilled
weights. To download or repair these weights manually before rendering:

```sh
.venv/bin/python python/quality_backend.py --download-hq
```

This reuses cached files, adds roughly 19 GB if only the distilled weights are
present, and uses the Hugging Face credentials from normal setup. HQ uses the full
transformer for guided stage 1 and the distilled transformer for stage 2, using
15+3 inference steps. Distilled uses 8+3 steps. HQ is now the production default;
the comparison still matters because coverage, storyboard conditioning, and
inference can each affect quality.
To compare storyboard conditioning on the existing local configuration first,
set `"local_profile": "distilled"` in a copy of the request before preparation.

Render one complete candidate at a time:

```sh
.venv/bin/python python/production_comparison.py render \
  outputs/production-comparison --arm A --candidate 1
.venv/bin/python python/production_comparison.py render \
  outputs/production-comparison --arm B --candidate 1
```

Stop at A and B for an entirely local video comparison. C remains optional. To
explicitly compare the paid service, set `LTX_API_KEY` in the shell environment,
then run:

```sh
.venv/bin/python python/production_comparison.py render \
  outputs/production-comparison --arm C --candidate 1
```

Running C sends the controlled prompts and storyboard images to LTX and submits
paid video jobs. The adapter uses the
[LTX v2 image-to-video API](https://docs.ltx.io/quickstart/), records job IDs, and
resumes polling after interruption. If submission has an uncertain outcome, it
stops instead of automatically submitting another paid generation.

Repeat the selected arms with `--candidate 2` and `--candidate 3`. Rerunning the same command
resumes that candidate and reuses matching completed shots. The local seeds are
fixed in the request. Cloud candidate numbers identify separate takes; the API
does not provide the same seed control, so they are not deterministic matches to
local takes.

## Use your own scene

Start from a saved baseline shooting script. Use a historical continuation-based
plan if A should reproduce the earlier renderer; a new production plan already
uses independent storyboard shots.

```sh
.venv/bin/python python/production_comparison.py init \
  --baseline outputs/YOUR_RUN/shooting-script.json \
  --controlled path/to/controlled-plan.json \
  --output path/to/comparison-request.json
```

Edit the request before preparation. File paths are relative to the request file.
The required inputs are:

- `screenplay`: the original scene text file.
- `baseline_plan` and `controlled_plan`: version 1 shooting-script JSON files.
- `storyboards`: one `{ "shot": 1, "image": "frames/001.png", "description": "..." }`
  entry for every shot in the controlled plan, numbered from one.
- `requirements`: the events and emotional changes the audience must understand.
- `candidate_seeds`: distinct integer seeds, one for each candidate per arm.
- `local_profile`: `hq` or `distilled` for B; A always uses distilled inference.
- `premium_model`: `ltx-2-5-pro`.

Both plans must use `"render_format": "landscape"`, delivering 1280×720 at 24 fps.
Controlled shots must last 6, 8, or 10 seconds, matching the
[LTX-2.5 Pro duration options](https://docs.ltx.io/models/ltx-2-5).
The controlled plan can change framing and coverage while preserving the ordered
source beats. Its prompts must describe action from the supplied starting image;
do not retain an instruction to begin on an unrelated inherited frame or portrait.
These are experiment execution plans. The comparison checks their structure and
source binding; it does not run the production planner or treat copied planning
metadata as validation of an edited plan. Source fidelity remains part of the
finished-scene review.

Make each storyboard a single shot composition using the intended cast, costume,
location, light, and visible props. Keep essential details inside the delivery
crop. A character portrait alone rarely establishes the intended scene. Check
reveals in the image itself: if pregnancy must be visible, the starting
composition must actually show it. Use reactions and offscreen action when the
audience can infer a difficult physical transition.

Prepared inputs are frozen for rendering. To revise a prompt, frame, duration, or
inference profile, edit the source request or its files and prepare a new
experiment directory. This keeps results attributable to the inputs that
produced them.

## Watch and record the result

```sh
.venv/bin/python python/production_comparison.py report outputs/production-comparison
open outputs/production-comparison/review.html
```

Watch the complete candidates with sound. The report links their assembled videos
and provides `review.json` for seven assessments: physical plausibility, acting,
cinematography, dramatic storytelling, editing, continuity, and audio performance.
Scores begin unassigned. Technical completion is not an artistic pass, and all
candidates may be rejected. The comparison does not publish a winning film or
claim an automatic audiovisual judgment.

Judge whether each required story event is clear, then record concrete evidence
and timestamps for weaknesses. Compare the same number of candidates per arm;
retain failures instead of presenting only the strongest individual shot.

A versus B changes coverage, conditioning, and potentially inference together.
It compares production paths rather than isolating a single cause. To measure
the benefit of HQ alone, prepare another experiment with the same controlled plan
and frames but B set to `distilled`. B versus C keeps the controlled inputs fixed,
while comparing local and cloud generation. Strong shots with a weak assembled
scene point toward further direction and editing work rather than another
increase in prompt detail.

For an HQ/distilled comparison through the normal renderer, generate the scene
once, then select the other Quality option and generate it again. Matching
screenplay, cast assets, format, and planning code reuse completed preproduction
from `.cache/preproduction/`; video profile and seed do not change that cache key.
The CLI equivalents are `--profile hq` and `--profile distilled`. Keep the same
`--seed` when comparing inference profiles. `--fresh-preproduction` deliberately
bypasses this reuse, so omit it when holding storyboard conditioning fixed.

Record motion and acting separately from faces, sharpness, and composition.
An overall preference for B cannot identify how much each change contributed.
The more difficult Will/Josephine scene still needs its own rendered review;
the shorter Sue/Joe example does not establish its physical-action or continuity
quality.
