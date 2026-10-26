# AIVS

https://github.com/user-attachments/assets/4e372e6e-2cd9-4b2f-ac84-19e1252cea96

![AIVS screenplay editor beside its character gallery](docs/images/aivs-codex-scene-heading.png)

AIVS turns a human-readable screenplay into an AI-generated video with speech.
Write a scene, cast its characters, select its heading, and choose **Generate
video**. The app uses **Codex through its app-server protocol to turn the scene
into a shooting script**, then uses **LTX-2.5 to render each shot's video and audio locally** on
an Apple Silicon Mac.

```text
Screenplay + cast
       ↓
Codex app-server → shooting script (timing, staging, dialogue, camera, sound)
       ↓
Python validates the plan → LTX-2.5 renders connected shots
       ↓
Speech and video checks → FFmpeg assembly → video.mp4
```

The desktop app is Rust; the video pipeline is Python using MLX on Apple's GPU.
Codex planning uses your configured Codex service and login. Video inference and
speech verification run locally.

## The core workflow, with a real example

These files come from run `2026-10-26_04-12-06-092171_1035559519`.
Copies live in the repository so you can read them without a local render:
[screenplay.txt](docs/examples/desert-road/screenplay.txt) →
[shooting-script.txt](docs/examples/desert-road/shooting-script.txt).

### 1. Start with a screenplay

The input describes the story in terms a writer understands: a setting, actions,
characters, and dialogue. Here is the original text:

```text
INT. DESERT ROAD - DUSK

A two-lane road and cuts through red rock. An old station wagon sits on the shoulder, its hood raised.

SUE, 30s, turns a battered postcard over in her hands. JOE, late 20s, emerges from behind the engine.

JOE: "Good news. It's definitely the car."

Sue almost smiles. She looks down the empty road.

SUE: "My dad said you could see the ocean from here."

JOE (looking around): "He was an optimistic man."

FADE OUT.
```

### 2. Codex turns the story into directions the video model can use

The app sends the screenplay, cast descriptions, ordered story beats, and
[director instructions](python/shooting-script-instructions.txt) to Codex.
A **beat** is one source action or dialogue line. Codex divides those beats into
timed shots and specifies where people stand, what they do, how the camera sees
them, and what should be heard.

In this example, it produces **two shots totaling 21 seconds**:

| Shot | Duration | Source beats | Planned action |
| --- | --- | --- | --- |
| 1 | 10 seconds | 1–4 | Establish the wagon and postcard; Joe steps into view and delivers his first line; Sue almost smiles and looks down the road. |
| 2 | 11 seconds | 5–6 | Sue delivers her line about the ocean; Joe looks around and replies; the scene ends on a quiet reaction. |

An excerpt from the actual first shot shows the added physical detail:

> SUE stands on the gravel screen left beside the front passenger door, holding a battered postcard in both hands. JOE stands on the ground screen right alongside the engine bay, empty hands clear of the engine, upper body partly screened by the raised hood. His feet and route along the near fender remain visible.

The plan also directs sound and the end of the action:

> Finish on this quiet reaction with environmental sound only: dry wind, faint cooling-engine ticks, and gravel crunch synchronized with JOE's step.

The [full shooting script](docs/examples/desert-road/shooting-script.txt) includes
exact dialogue, appearance and voice descriptions, and start/end continuity notes
for each shot. This translation is the core of AIVS: turning a short human scene
into explicit, timed instructions for a video model.

This saved example is a historical planning artifact, not a guarantee of visual
quality or a template for every current rule. For example, its second shot calls
for a tighter framing; the current renderer carries forward the previous frame,
and current director instructions require reframing to develop through camera
movement within a scene.

### 3. Validate, render, and assemble

Python checks that the plan covers the source beats in order, preserves dialogue,
and fits duration and model token limits. It fills dialogue placeholders with the
source lines and saves both `shooting-script.json` for the renderer and
`shooting-script.txt` for people to inspect. Invalid plans get one correction
attempt through Codex before the run fails.

LTX-2.5 generates video and speech together in **4–12 second shots at 24 fps**.
Choose **File → Settings…** for **1280×720 (16:9)** or **1080×1920 (9:16)** delivery.
The model requires dimensions divisible by 64, so it generates at 1280×768 or
1088×1920 respectively. Final assembly crops 24 pixels from the top and bottom,
or 4 pixels from each side; it does not stretch the picture. Within a scene,
each shot after the first starts from the previous
shot's final decoded frame to carry forward faces, clothing, positions, and props.
A handoff uses the full model frame, before the delivery crop. Portrait planning
uses the narrow frame from the outset: people staged in depth, visible speaker
faces and handoffs, and continuous reframing rather than instant reverse angles.
A new scene resets that frame reference. The model receives each shot's complete
prompt; the separate start/end notes support continuity review.

Whisper transcribes each take locally and the app compares the recognized words
with the source dialogue. FFmpeg assembles the shots and applies the final fade
when requested. The assembled result must pass speech, decoded-frame, and
audio/video timing checks before it is published as `video.mp4`.

A shot with mismatched dialogue gets up to two retakes with new seeds. Rejected
takes and their transcripts are preserved in `takes/<shot>/<take>/`. Recognizer
or file errors stop the run immediately. If the retakes also fail, the error
shows the differing words and keeps the latest take available for review.

Speech matching ignores case and punctuation and folds Latin diacritics (so
“Allô” and “Allo” compare equally); it still checks word order, repetition, and
lexical changes. Standalone repeated “ha” syllables are ignored only when an
action beat in the source explicitly calls for laughter and the segment falls
inside that shot's time range. Raw Whisper segments remain in the report. This
small rule does not classify mixed dialogue or other forms of vocalization.

## Run your first scene

### Prerequisites

- An **Apple Silicon Mac with Metal support** and **Rust 1.92+**.
- **Homebrew**, so setup can install `uv`, FFmpeg, and `whisper-cpp` if missing.
- A **Hugging Face access token** for downloading models.
- The **Codex CLI**, signed in and available on `PATH` (or set `AIVS_CODEX_PATH`).
  Codex is required for screenplay planning; the chat panel is optional.
- **50 GiB of free disk space** for first setup. Video model downloads are about
  **43 GB**; speech verification also caches a roughly **488 MB** Whisper model.

From this checkout:

1. Create `.env` if it does not already exist, using [.env.example](.env.example)
   as a template. Set `HF_API` to your Hugging Face token.
2. Start the app:

   ```sh
   cargo run
   ```

3. Let setup prepare Python 3.12 in `.venv`, install the pinned packages, download
   models, and verify the GPU. Downloads reuse the cache. Use **Check again**
   after adding a token, or **Retry setup** after fixing a setup error.
4. If needed, choose **Codex → Sign in with ChatGPT** and finish in your browser.
   An existing Codex login is reused.
5. Edit the screenplay and cast each role in the scene. Drag a gallery character
   onto a role, or use its **Uncast / played by** control. For the sample, try **Ava as Sue**
   and **Eli as Joe**. Casting keeps the script's role names and dialogue intact.
6. Choose **File → Settings…** to select landscape or portrait. Landscape is the
   default; the selection saves to `settings.json` in the workspace.
7. Select a scene by its heading above **Generate video**, then choose
   **Generate video**. Only that scene and its cast are sent for generation.
   **Activity** shows planning and rendering progress.
   When complete, play the result in the app or choose **Show files**.

**Stop** interrupts the render and its child processes; closing AIVS also stops
active work. Partial outputs remain available for inspection.

If generation or Resume fails, AIVS sounds the system alert, bounces its Dock
icon, adds a red **!** badge, and posts a notification with the error summary.
Failure detection continues while the window is minimized or hidden. Returning
to AIVS or starting another generation clears the Dock alert; the full error
remains in the app. Stop and recoverable take retries do not trigger alerts.
Notifications use macOS AppleScript so they also work with `cargo run`; allow
Script Editor notifications in System Settings → Notifications if banners do
not appear. Focus settings and system sound volume still apply.

Choose **File → Resume…** and select an existing `outputs/<timestamp>_<seed>/`
directory to continue that run in place. Resume uses its saved screenplay, cast,
seed, render format, and shooting script; Codex runs only if planning never finished.
Current settings do not change a resumed run, including older 1280×704 runs. Existing
shots are checked and accepted takes are reused without video inference. A take
saved before speech verification failed is rechecked, then retaken if its words
still mismatch. Each explicit Resume allows up to two further retakes per shot;
take numbers and seeds continue from the saved history. Missing or invalid shots
are rendered before assembly. Continuation shots are reused only when their
recorded input matches the preceding take, including across interruptions.

By default, the current directory is the workspace. To keep scripts, caches,
and outputs elsewhere, pass an existing directory:

```sh
cargo run -- /path/to/workspace
# To select a specific Codex executable:
AIVS_CODEX_PATH=/absolute/path/to/codex cargo run
```

Put `.env` in the selected workspace. Keep this checkout's `python/` directory
alongside the source when building; it contains the runtime pipeline.

### Editing and casting

Scripts and cast assignments autosave to `screenplay.json` in the workspace.
**⌘/Ctrl+S** saves immediately. Click text to edit; use the element selector,
**+ Insert**, and **Edit** for formatting, insertion, deletion, and role renaming.

Use **File → Import…** to choose a `.fountain` screenplay. Import replaces the
current screenplay and clears its cast; **Undo** restores both. Scene headings,
action, character cues, parentheticals, dialogue, and transitions become editable
elements. Centered text and lyrics retain their text in action/dialogue elements;
simultaneous dialogue is imported in source order. Title-page metadata, outline
sections, notes, omitted passages, page breaks, and emphasis formatting are omitted.
The original Fountain file is preserved, and the imported screenplay autosaves to
`screenplay.json`. Full-length scripts can be imported; video generation uses
one selected scene at a time, within a 100,000-character limit per scene.

Fountain is a plain-text screenplay format; [nyousefi/Fountain](https://github.com/nyousefi/Fountain)
provides a reference implementation and sample scripts. Try the [Big Fish
sample](https://github.com/nyousefi/Fountain/blob/master/FountainTests/Big%20Fish.fountain):
[download the raw `.fountain` file](https://raw.githubusercontent.com/nyousefi/Fountain/master/FountainTests/Big%20Fish.fountain),
then choose **File → Import…** in AIVS. Select a scene heading from the **Scene**
menu, cast its characters, then choose **Generate video**. AIVS plans and renders
only that scene; the rest of the imported screenplay remains available to edit
and render scene by scene.

- **Enter** creates the next screenplay element; **Shift+Enter** inserts a line break.
- **Tab** in dialogue adds a parenthetical instruction. On an empty action, it
  changes the element to a character cue; **Shift+Tab** selects the previous type.
- **Backspace** on an empty element steps backward through its formats, then
  removes it. One blank scene heading remains when the script is cleared.
- **⌘/Ctrl+Z** and **⌘/Ctrl+Shift+Z** undo and redo edits, including casting.

The gallery supplies saved appearance and voice descriptions. The GUI uses these
for a scene's first shot, then carries frames forward. Solo gallery portraits
are not supplied as separate identity references to an ensemble render.

## Where to start in the code

Follow the main path: **UI → Python entry point → Codex planner → shot renderer**.

| File | Responsibility |
| --- | --- |
| [src/app.rs](src/app.rs) | Desktop workspace, setup gate, and optional chat UI. |
| [src/screenplay.rs](src/screenplay.rs) | Screenplay editor, casting, and document persistence. |
| [src/video.rs](src/video.rs) | Starts setup/render workers, handles cancellation, and reads Python progress events. |
| [python/video.py](python/video.py) | Pipeline entry point; `generate_screenplay()` coordinates planning and rendering. |
| [python/shooting_script.py](python/shooting_script.py) | Parses story beats, calls Codex, validates the plan, and saves shooting scripts. |
| [python/shooting-script-instructions.txt](python/shooting-script-instructions.txt) | Director guidance for staging, timing, continuity, and sound. Start here to improve planning. |
| [python/shot_renderer.py](python/shot_renderer.py) | Renders sequential shots, carries frames forward, checks speech, and assembles the result. |
| [python/quality_backend.py](python/quality_backend.py) | Local LTX-2.5/MLX generation and model setup. |
| [python/speech_validation.py](python/speech_validation.py), [python/validation.py](python/validation.py) | Speech matching and media checks. |
| [src/codex.rs](src/codex.rs) | Codex account login and optional chat integration. |
| [src/main.rs](src/main.rs), [src/playback.rs](src/playback.rs) | Native egui/winit/wgpu window and macOS AVKit playback. |

### How the Codex integration works

AIVS launches `codex app-server` and exchanges newline-delimited JSON over
stdin/stdout using the [official Codex app-server protocol](https://learn.chatgpt.com/docs/app-server).
The planner in `python/shooting_script.py` initializes a connection, creates an
ephemeral read-only thread, and starts a turn with the screenplay and a JSON
output schema. It streams activity into the UI, then validates the returned plan.

The Rust client in `src/codex.rs` handles login and optional chat separately.
**Show chat** opens that panel; chat threads use `workspace-write` in the selected
workspace, with network access disabled and `approvalPolicy: never`. Reconnecting
starts a fresh chat. The Codex CLI owns credentials, and AIVS inherits its model
and reasoning defaults.

Change story direction in the director instructions. Keep Python responsible for
source fidelity, timing bounds, and rendering contracts rather than rules tied
to particular story props.

## Inspect and debug a run

Each run writes to `outputs/<timestamp>_<seed>/` in the workspace:

| Artifact | What to inspect |
| --- | --- |
| `screenplay.txt`, `screenplay-request.json` | Original script and cast bindings. |
| `shooting-script.txt`, `shooting-script.json` | Readable directions and the validated machine plan. |
| `shooting-script-request-*`, `shooting-script-response-*` | What the planner sent to Codex and received back. |
| `shooting-script-events-*.jsonl`, `shooting-script-errors-*.txt` | Live protocol events and diagnostics, including failed or stopped planning. |
| `shots/001/`, `shots/002/`, … | Individual takes and their reports. |
| `takes/<shot>/<take>/` | Archived takes, seeds, and diagnostics from replacements. |
| `speech-recognition.json`, `speech-transcript.txt`, `speech-validation.json` | Speech evidence for individual shots and the final assembly. |
| `render.json` | Seed, status, prompts, timing, runtime details, and errors. |
| `video.mp4` | Final validated assembly, present after a successful render. |

For planning work, you can skip video inference after completing setup. From the
checkout using its default workspace:

```sh
.venv/bin/python python/video.py render \
  --script-file docs/examples/desert-road/screenplay.txt \
  --cast SUE=ava --cast JOE=eli \
  --plan-only
```

This still calls Codex and uses the cached tokenizer. It generates a new plan,
which may differ from the saved example. Remove `--plan-only` to render as well.
Add `--render-format portrait` for 1080×1920; `--script-file` defaults to landscape.
`--shooting-script /path/to/shooting-script.json` reuses a plan after revalidating
it against the current script, cast, and render format. Changing format requires
a new shooting script.

To resume an existing run from the command line:

```sh
.venv/bin/python python/video.py resume --output-dir /path/to/output-directory
```

## Verify changes

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 -m unittest discover -s python -v
```

The suites cover editing, casting, setup, planning, cancellation, media validation,
Codex protocol handling, and responsive UI layout. Optional integration checks:

```sh
# Reuse an existing model cache while testing setup in a temporary workspace.
AIVS_TEST_CACHE=/absolute/path/to/.cache/huggingface \
  cargo test live_setup_installs_python_then_skips_installation_on_the_next_launch -- --ignored
# Use an existing ChatGPT login for a small live Codex request.
cargo test live_app_server_streams_a_complete_reply -- --ignored
```

## Current limits

Timing is estimated, and generated actions, faces, voices, and lip sync still
need human review. Frame continuity helps within a scene but does not lock
identity across new locations. Whisper can mishear dialogue; a matching transcript
does not prove the correct speaker or a convincing performance. Persistent speech
mismatches stop the run after bounded retakes and leave every take available for
review. An assembled-video speech mismatch stops for review without automatically
regenerating accepted shots.

The editor has no print pagination or Final Draft import/export. Other renderers
and separate speech synthesis are outside the main workflow. An opt-in
[character consistency experiment](experiments/character-consistency/README.md)
explores portrait-guided starting images and fixed dialogue audio; it has separate
setup and limitations.

`.env`, `.venv`, `.cache`, `screenplay.json`, `settings.json`, and generated `outputs/` stay out of Git.
The adapted Studio source retains its **GPL-3.0-only** license; see [LICENSE](LICENSE).
