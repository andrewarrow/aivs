# AIVS

A native Rust desktop workspace for local AI video generation, using egui 0.35,
winit, and wgpu. The first migrated workflow is Hunter's **option 8**: LTX-2.5
generates speech and video together using the original Nancy default prompt.
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

Choose **Generate video** to run the unchanged prompt as one continuous shot
with native jointly generated speech. Dialogue determines the duration: the
default produces **8 seconds at 1280×704 and 24 fps**. **Stop** interrupts the
Python process group, including FFmpeg. Closing AIVS also stops its active work.
Each run saves its prompt, random seed, dialogue timing, runtime details, and
status in `outputs/<timestamp>_<seed>/render.json`. Only a video that passes
decoded-frame and audio/video timing checks is published as `video.mp4`.
**Open video** and **Show files** open the completed result and its directory.

Character reference images appear in a scrollable gallery beside the workspace.
The gallery reads the imported `characters/` data; generation does not yet use
those references. Alternate renderers, speech synthesis, and the remaining
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
and activity logs. `python/video.py` is the entry point; `quality_backend.py`,
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
