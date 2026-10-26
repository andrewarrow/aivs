# AIVS

A native Rust desktop workspace for local AI video generation, using egui 0.35,
winit, and wgpu. The first migrated workflow is Hunter's **option 8**: LTX-2.5
generates speech and video together from the screenplay shown in the workspace.
Python still owns the MLX pipeline; Rust owns setup, controls, and process lifetime.
The existing **Codex** menu and optional chat remain available after setup.

## Run

Use an Apple Silicon Mac with Metal support and Rust 1.92 or newer, then run:

```sh
cargo run
# Or choose a directory for local setup, video outputs, and Codex:
cargo run -- /path/to/workspace
```

On startup AIVS checks the selected workspace before showing its controls:

- `.env` must contain a nonempty `HF_API`. If missing, the blocking setup modal
  links to Hugging Face access tokens and shows where to create the file. AIVS
  does not create or copy credentials. See `.env.example`, then use **Check again**.
- `.venv/bin/python` must be Python 3.12 with the tested packages and pinned MLX
  runtime. Setup uses `uv` directly, without the old shell menus.
- `.cache/huggingface` must contain the pinned LTX-2.5 q8 checkpoint, text encoder,
  configuration, audio VAE, and vocoder. Startup checks every required file,
  safetensors headers and lengths, and an actual Apple GPU calculation.

Missing Python packages and model files are installed automatically after the
token is present. If `uv` or FFmpeg is missing, setup installs them through an
existing Homebrew installation. Without Homebrew, it shows an installation error
and **Retry setup**. Python installation shows activity; model downloads report
file/byte progress. Initial model setup downloads about **43 GB** and requires
**50 GiB free**. Cached files are reused, and interrupted downloads can be retried.
Errors keep setup locked. A ready installation skips the setup modal.

Choose **Generate video** to perform the displayed screenplay as one continuous
shot with native jointly generated speech at **1280×704 and 24 fps**. All spoken
lines determine speech time; screenplay action paragraphs each reserve another
three seconds for physical action, including the final walk. This is a timing
heuristic, not a guarantee that the model completes every action. **Stop**
interrupts the Python process group, including FFmpeg. Closing AIVS also stops
its active work.
Each run saves its prompt, random seed, dialogue timing, runtime details, and
status in `outputs/<timestamp>_<seed>/render.json`. Only a video that passes
decoded-frame and audio/video timing checks is published as `video.mp4`.
The progress bar estimates overall completion and time remaining using measured
transformer blocks, denoising steps, and decoder tiles, with time reserved for
encoding and validation. Successful runs save phase timings so later estimates
adapt to this Mac. Estimates can change with GPU load and cache performance;
100% is reserved for a validated result.

While Codex prepares the shooting script, an elapsed timer and spinner show that
the job is active. Activity opens automatically and displays Codex messages and
diagnostics as they arrive, with periodic updates during quiet stretches and
visible validation retries. Raw Codex events and diagnostics are saved as they
arrive in the run's output directory, including when planning times out or stops.

The director instructions live in `python/shooting-script-instructions.txt`.
Codex resolves physical staging, prop handoffs, dialogue timing, and quiet
reactions there, then reviews the plan for continuity before returning it.
Each shot prompt must include its essential opening state and sound through the
end of the take; the separate start/end notes are for continuity review and are
not passed to the model. Python checks source dialogue, beat order, duration,
and token limits. Story-specific fixes belong in the director instructions,
rather than Python rules for particular objects or actions. These checks do not
verify the generated performance; picture and speech still need review.

Completed videos appear inside AIVS with native macOS playback controls for
play/pause, seeking, volume, and fullscreen, including the generated audio.
**Show files** opens the output directory. Playback pauses when its preview is
hidden, and previous results remain playable while another render runs.

The left workspace starts with a 50/50 split: a one-page screenplay preview above
the existing video context. Drag the horizontal divider to give the screenplay
30–70% of the height, or double-click it to restore 50%. With the divider focused,
use Up/Down to resize or Home to reset. The page and generation share the same
scene data, with explicit speaker labels for all five lines of dialogue.

Character reference images appear in a scrollable gallery beside the workspace.
Gallery selection browses the library. Drag a character's name or portrait onto
any role cue, parenthetical, or dialogue to cast that role throughout the script.
You can also click or keyboard-activate a dialogue block to choose or clear its
cast. Every occurrence shows the assigned performer. The current script's roles
are **Sue and Joe**; for example, cast **Ava as Sue** and **Eli as Joe**. Script
names and dialogue stay unchanged when casting or recasting. Both roles must be
cast before **Generate video** becomes available. Assignments last for the current
session.
Python resolves the assigned `@name` references and adds appearance and voice
descriptions, with explicit performer-to-role bindings in the render prompt.
GUI renders do not infer cast from bare names in the script, even when a role
shares a saved character's name.
The GUI renders from these saved descriptions, with a fixed wide camera showing
both actors head to toe beside the whole car, with room to move and walk. Camera
instructions hold the composition while allowing actors to change position.
The current scene reserves 32 seconds for its dialogue and action, rather than
the 14-second speech-only estimate. No joint image is required. Cast resolution
has no two-character limit. The renderer's image inputs anchor complete frames, rather than separate
per-person identities, so the GUI does not feed solo portraits into an ensemble
scene. Saved photos remain available for a future identity-conditioning backend;
text guidance does not guarantee exact likeness or voice identity.

For image-guided CLI renders, a character
may include an optional `scene_image` record pointing to `scene-reference.png`;
it supplies the whole first video frame and is anchored again at the end to hold
the composition. The `image` (`reference.png`) remains the gallery portrait and
is the first-frame fallback when there is no scene image. Square portraits may
be cropped or frame the character too closely for landscape video.
All nine imported characters include wide references for the default desert scene
while keeping their original portraits in the gallery. The built-in imagegen
prompts are saved beside the images as `scene-reference.txt`. For a different
setting, provide a matching scene reference or use `--character-text-only`.
Each render saves copies of its character profiles and reference assets beside
the original and expanded prompts. Native
speech follows the voice description; voice samples do not lock voice identity.
The gallery and renderer use the workspace's `characters/` directory, falling back
to this checkout's imported library. CLI renders also accept `--character NAME`
(repeatable) and `--character-text-only` for scenes with multiple character images.
Alternate renderers, speech synthesis, and the remaining
Hunter menu options have not been migrated. Generation uses local cached models;
file checks do not establish speech accuracy or lip synchronization.
The Python sources currently ship in this checkout; keep `python/` alongside the
source when building. `.env`, `.venv`, `.cache`, and generated outputs are ignored
by Git. No runtime path points back to the Hunter checkout.

Codex is resolved from `AIVS_CODEX_PATH`, a binary alongside AIVS (or in a macOS
bundle's `Contents/Resources`), then `PATH`. For example:

```sh
AIVS_CODEX_PATH=/absolute/path/to/codex cargo run
```

Choose **Codex → Sign in with ChatGPT** if needed, finish in your browser, then
enable **Show chat**. Install the Codex CLI to use this optional feature. An
existing Codex ChatGPT login is picked up automatically.
Send messages with **Send** or **⌘/Ctrl+Enter**; **Stop** interrupts the current turn.
On narrow windows chat fills the content area. Closing chat restores the video workspace.

## Integration

`src/main.rs` owns the one-window native event/render loop. `src/app.rs` owns
the setup gate, video workspace, and optional chat UI. `src/video.rs` runs
setup/render processes on a worker thread and reads Python's prefixed JSON events
and activity logs. `src/playback.rs` embeds AVKit in the native window.
`python/video.py` is the entry point; `render_estimate.py` learns phase budgets
from successful reports. `quality_backend.py`,
`quality_progress.py`, `dialogue_helpers.py`, and `validation.py` preserve the
needed Hunter render, progress, timing, and validation behavior.
`src/codex.rs` is the adapted Studio
app-server client: it runs a background child process and exchanges newline-delimited
JSON over stdin/stdout using the [official Codex app-server protocol](https://learn.chatgpt.com/docs/app-server).
It initializes the connection, reads the account, starts browser login, creates
a thread, streams turns, interrupts work, and kills/reaps the child on shutdown.

The Codex CLI manages credentials and login callbacks; AIVS does not copy or
store tokens. It inherits the user's Codex model and reasoning defaults.
Threads use `workspace-write` scoped to the selected directory, with network
access disabled and `approvalPolicy: never`, matching Studio's basic setup.
Requests needing unsupported interactive client methods receive an RPC error.
Conversations are not restored in the GUI after reconnecting or restarting.
**Reconnect** starts a fresh connection and chat while preserving the unsent draft.

No Studio dashboards or deployment integrations are included.
The adapted source retains Studio's GPL-3.0-only license; see `LICENSE`.

## Verify

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 -m unittest discover -s python -v
# Optional: installs Python into a temporary workspace, using an existing cache.
AIVS_TEST_CACHE=/absolute/path/to/.cache/huggingface \
  cargo test live_setup_installs_python_then_skips_installation_on_the_next_launch -- --ignored
# Optional: uses the existing ChatGPT login and a small inference request.
cargo test live_app_server_streams_a_complete_reply -- --ignored
```

Tests cover the startup lock, missing credentials, model completeness, failed
renders, process-group cancellation, real FFmpeg validation, and GUI layout at
narrow, tablet, laptop, and desktop sizes, plus the existing Codex protocol tests.
Browser OAuth completion requires an interactive sign-in. The optional setup
test reuses a cache without downloading model files or using real credentials.
