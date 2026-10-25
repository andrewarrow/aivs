# AIVS

A minimal Rust desktop app adapted from `/Users/aa/DeployerCoaster/studio`.
It uses the same egui 0.35, winit, and wgpu stack, with a single native window
and an empty main canvas. The **Codex** menu contains ChatGPT sign-in and
**Show chat**, which opens a chat panel within that window.

## Run

Install Rust 1.92 or newer and the Codex CLI, then run:

```sh
cargo run
# Or choose the directory Codex should work in:
cargo run -- /path/to/workspace
```

Codex is resolved from `AIVS_CODEX_PATH`, a binary alongside AIVS (or in a macOS
bundle's `Contents/Resources`), then `PATH`. For example:

```sh
AIVS_CODEX_PATH=/absolute/path/to/codex cargo run
```

Choose **Codex → Sign in with ChatGPT** if needed, finish in your browser, then
enable **Show chat**. An existing Codex ChatGPT login is picked up automatically.
Send messages with **Send** or **⌘/Ctrl+Enter**; **Stop** interrupts the current turn.
On narrow windows chat fills the content area. Closing chat restores the empty canvas.

## Integration

`src/main.rs` owns the one-window native event/render loop. `src/app.rs` owns
the empty canvas and optional chat UI. `src/codex.rs` is the adapted Studio
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

No Studio dashboards, deployment integrations, assets, or application data are included.
The adapted source retains Studio's GPL-3.0-only license; see `LICENSE`.

## Verify

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
# Optional: uses the existing ChatGPT login and a small inference request.
cargo test live_app_server_streams_a_complete_reply -- --ignored
```

Tests cover protocol handshake/order, login completion, streaming, cancellation,
workspace policy, error handling, process shutdown, and GUI layout at narrow,
tablet, laptop, and desktop sizes. Browser OAuth completion requires an interactive
sign-in.
