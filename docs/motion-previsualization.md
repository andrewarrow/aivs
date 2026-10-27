# Motion Previsualization v1

Motion previsualization adds timed images to a **completed, accepted** AIVS shooting plan. It does not call, replace, or modify the five-pass planner. The original plan, storyboards, reference library, and storyboard acceptance stay in place.

In the app, choose **Motion → Previsualization…** and select a run with completed storyboards. This produces independently generated and inspected keyframes, then a silent held-frame animatic. Choose **Motion → Final Render…** with that directory to render using the accepted anchors. Repeating Previsualization resumes saved work. Ordinary Resume retains the existing single-image method until Final Render is selected; afterward it resumes that run's saved motion rendering mode.

## Telephone demonstration to run

This implementation has not been executed or visually validated. No generated frames, animatic, comparison videos, or measured improvements are claimed. Per the repository instruction, testing and generation are left to the operator.

Start with an existing five-second telephone shot if available. Otherwise prepare the example once, using a saved `ava` character with portrait references (change the cast binding if necessary):

```sh
.venv/bin/python python/video.py render \
  --screenplay-file docs/examples/telephone-previs.json \
  --storyboards-only --profile distilled --seed 83
```

The existing planner still controls duration and coverage. Verify the finalized plan has exactly one five-second shot before proceeding with the initial experiment; the comparison command rejects other durations. Do not regenerate an existing suitable plan.

Use the emitted output directory for `RUN`:

```sh
RUN=outputs/YOUR_EXISTING_RUN
.venv/bin/python python/motion_previs.py prepare "$RUN"
```

Inspect `motion/animatic.mp4`, all `motion/001/*/frame.json` images, and their `review.json` observations. A moving shot must have three or four states: opening, one or two intermediate poses, and ending. Suggested five-second telephone timing is frames 0, 24, 48, and 119 at 24 fps: ringing, reaching, lifting, and speaking. Frame 119 is the last **output** frame; the backend pads inference to 121 frames and trims afterward. It does not silently move the endpoint guide into discarded padding.

Confirm that each image shows the same character, clothing, environment, base, handset, cradle, and cord. Check that the cradle becomes empty, the handset exists exactly once, the cord stays attached at both ends, and the same hand holds the handset. Hidden details must remain uncertain rather than being certified from a prompt.

Then run the controlled comparison:

```sh
.venv/bin/python python/motion_previs.py compare "$RUN" --shot 1 --profile distilled
```

This explicitly invokes two LTX renders: one original opening-image render and one timed-anchor render. Prompt, seed, profile, duration, and dimensions match. Both retain native LTX audio and model-resolution video, without voice conversion or delivery cropping. HQ can be selected with `--profile hq`; fixed audio is excluded from this experiment.

Outputs are preserved under `motion/comparison/001-distilled/{single,multiple}/video.mp4`, with separate render provenance and visual reviews. `comparison.json` records differences from actual timestamped samples, including geometry, character appearance, and motion limitations. Sampling does not prove correctness between sampled frames. Repeating the command reuses exact completed outputs; an interrupted arm resumes independently.

To render the whole movie with the approved keyframes:

```sh
.venv/bin/python python/video.py resume --output-dir "$RUN" --motion-previs
```

Existing takes are archived through the normal renderer before replacement. Use the comparison command for a matched-seed A/B experiment: normal Final Render retains existing take numbering and seed behavior.

## Artifacts and acceptance

- `motion/NNN.plan.json`: independently cached timed direction for each shot.
- `motion/plan.json`: complete temporal contract bound to the unchanged finalized plan.
- `motion/NNN/FFFFF/frame.json`: generated image, input reference hashes, physical state, timestamp, strength, and generation provenance. Frame zero reuses the approved opening image.
- `motion/NNN/FFFFF/review.json`: independently cached pixel observations, component counts, and targeted correction advice.
- `motion/NNN/FFFFF/repair-{1,2,3}/`: immutable repair image/review attempts, each bound to its rejected predecessor and findings.
- `motion/keyframes.json`: accepted complete sequence with hashes of every image and review.
- `motion/animatic.{mp4,json}`: silent held-keyframe preview and its input/output fingerprints. No LTX weights are loaded.

The planner retains every tracked entity and fact key through all states, including offscreen components. Each interval is validated using the existing source-backed transition validator. New physical identity transformations and canonical attachment changes require authorization in the original shot contract. A separate `hands` array records each character's left/right hand and held component ID (or none/unknown/not-visible), checked against independently observed hand states. Every held component must have the matching owner and exact `ROLE:left-hand` / `ROLE:right-hand` support, even without an optional relationship record. Existing explicit `holds-left` / `holds-right` relationships must agree, including negative claims; those relationships use character roles as subjects, component IDs as objects, and yes/no values. One hand cannot hold two separately tracked components, and a held component cannot remain docked.

The addon preserves the original endpoint contracts exactly and adds hand detail alongside them. If those contracts contradict the intended movement, it rejects the refinement instead of silently rewriting the approved plan. Correct that contract deliberately before generating motion assets. This is a state/transition validator with visual inspection, not a geometric simulator or collision solver.

All accepted pixels, reference bytes, contracts, review results, dimensions, and implementation identity are checked again before rendering. Changed or rejected artifacts cannot reuse an old acceptance. Completed frame generation survives an interruption during review; completed image-tool output also uses the existing backend's recovery checkpoint. Partial/orphan images are archived rather than accepted.

Rejected generated frames receive up to three automatic repair attempts. Local defects use a targeted edit of the rejected pixels, with the preceding accepted pose and canonical shot references, and explicit preservation constraints. Each attempt is independently reinspected. A fundamentally broken composition is regenerated immediately; the last retry also uses a fresh generation if edits have not resolved the defect. All rejected pixels, requests and reviews remain in place. Repeating Previsualization reuses completed attempts and does not reset the retry allowance. Exhausting the allowance stops before LTX. Frame zero must retain the approved storyboard pixels; a rejected opening requires correcting that storyboard rather than editing it inside motion previsualization.

Motion images use references for the current shot's environment, visible cast and visible prop/component designs. Preceding accepted pose and opening pixels receive priority; repairs also prioritize the rejected edit target. At most five image-tool inputs are submitted. Extra canonical references are combined into labeled sheets with at most four panels per sheet and source hashes. A request that cannot retain every essential reference in readable sheets fails instead of dropping identities or props. Reference verification still requires exact image-tool input hashes, including the sheets. The first previsualization run performs the same cached generation/edit preflight as storyboard production before invoking motion planning.

Static shots can retain one image if their complete endpoint states match. Moving shots use three or four distinct images. Frame zero uses `ImageConditioningInput.replace()`, later output-frame indices use `.guide()`. Strengths (1.0 opening, .7 intermediate, .9 ending) are experimental defaults, not tuned recommendations. Existing single-image and held-image paths remain available. Multi-anchor fixed-audio generation is intentionally rejected; HQ's existing fixed-audio restriction remains in place.

Keyframes guide LTX generation. They do not guarantee exact poses, exact intermediate motion, perfect anatomy, or continuity. This version does not add Blender rigs, trajectory/depth/pose control, Prompt Relay, dialogue audio, or interpolated human animation.

Pinned runtime image-binding reference: [ImageConditioningInput helpers at 14a3ddd6](https://github.com/dgrauet/ltx-2-mlx/blob/14a3ddd6054736beb45b4d836135fa4d44490531/packages/ltx-pipelines-mlx/src/ltx_pipelines_mlx/utils/args.py).
