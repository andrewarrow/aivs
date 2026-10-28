"""Plain, flushed activity logs for the pinned, disk-streamed MLX renderer."""

from __future__ import annotations

import codecs
from contextlib import contextmanager, ExitStack, suppress
from datetime import datetime
import os
import subprocess
import sys
import tempfile
import threading
import time
from unittest.mock import patch


class RenderProgress:
    def __init__(self, verbose=3, *, stream=None, heartbeat_seconds=15, estimate=None):
        self.verbose = verbose
        self.stream = stream if stream is not None else sys.stderr
        self.heartbeat_seconds = heartbeat_seconds
        self.operation = "Starting quality renderer"
        self._lock = threading.RLock()
        self._stop = threading.Event()
        self._last_message = time.monotonic()
        self.estimate = estimate

    def completed(self, fraction):
        with self._lock:
            if self.estimate is not None:
                self.estimate.update(fraction)

    def log(self, message, *, level=1):
        if self.verbose < level:
            return
        with self._lock:
            print(f"[{datetime.now():%H:%M:%S}] {message}", file=self.stream, flush=True)
            self._last_message = time.monotonic()

    def activity(self, message, *, level=1):
        with self._lock:
            self.operation = message
            self.log(message, level=level)

    @contextmanager
    def phase(self, label, key=None):
        previous = self.operation
        started = time.monotonic()
        self.activity(f"{label}: starting")
        with self._lock:
            if key is not None and self.estimate is not None:
                self.estimate.start(key, label)
        try:
            yield
        except BaseException:
            self.log(f"{label}: stopped after {time.monotonic() - started:.1f}s")
            raise
        else:
            self.log(f"{label}: done in {time.monotonic() - started:.1f}s")
            with self._lock:
                if key is not None and self.estimate is not None:
                    self.estimate.finish(key)
        finally:
            with self._lock:
                self.operation = previous

    def _heartbeat(self):
        while not self._stop.wait(min(self.heartbeat_seconds, 1)):
            with self._lock:
                if self.estimate is not None:
                    self.estimate.tick()
                elapsed = time.monotonic() - self._last_message
                if elapsed >= self.heartbeat_seconds:
                    self.log(f"{self.operation} — waiting; {elapsed:.0f}s since last activity update")

    def __enter__(self):
        self._thread = threading.Thread(target=self._heartbeat, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, *_exc):
        self._stop.set()
        self._thread.join()


def _tail_ffmpeg_stderr(stream, stop, progress):
    """Read without moving the child's file offset or blocking its stderr writes."""
    offset = 0
    pending = ""
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    while True:
        data = os.pread(stream.fileno(), 8192, offset)
        if data:
            offset += len(data)
            pending += decoder.decode(data)
            lines = pending.replace("\r", "\n").split("\n")
            pending = lines.pop()
            for line in lines:
                if line:
                    progress.log(f"FFmpeg stderr: {line}", level=2)
        elif stop.is_set():
            break
        else:
            stop.wait(0.2)
    pending += decoder.decode(b"", final=True)
    if pending:
        progress.log(f"FFmpeg stderr: {pending}", level=2)


@contextmanager
def ffmpeg_output(command, progress, *, stdin=subprocess.PIPE):
    """Stream diagnostics, retaining the runtime's file-backed stderr safety.

    A pipe could fill while a native GPU call holds the GIL. Keep stderr in a
    temporary file and tail it with pread, which leaves the writer offset alone.
    stdout remains inherited; video bytes still travel only through stdin.
    """
    with tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(command, stdin=stdin, stderr=stderr)
        stop = threading.Event()
        reader = threading.Thread(target=_tail_ffmpeg_stderr, args=(stderr, stop, progress), daemon=True)
        reader.start()
        progress.log(f"FFmpeg process {process.pid}: started; streaming stderr", level=2)
        try:
            yield process
        finally:
            try:
                if process.stdin is not None and not process.stdin.closed:
                    with suppress(BrokenPipeError):
                        process.stdin.close()
                try:
                    process.wait()
                except BaseException:
                    if process.poll() is None:
                        process.terminate()
                    process.wait()
                    raise
            finally:
                stop.set()
                reader.join()
        if process.returncode:
            stderr.seek(0, os.SEEK_END)
            stderr.seek(max(0, stderr.tell() - 4096))
            tail = stderr.read().decode("utf-8", errors="replace").strip()
            raise RuntimeError(f"FFmpeg exited with status {process.returncode}:\n{tail}")
        progress.log(f"FFmpeg process {process.pid}: finished", level=2)


@contextmanager
def decoder_activity(progress):
    """Observe the pinned decoder's existing evaluated tile/chunk boundaries."""
    from ltx_core_mlx.model.video_vae import video_vae
    from ltx_pipelines_mlx.utils import _orchestration, blocks

    original_groups = video_vae._group_tiles_by_temporal_slice
    original_decode = video_vae.VideoDecoder.tiled_decode

    def group_tiles(tiles):
        groups = original_groups(tiles)
        total = len(tiles)
        completed = 0
        progress.log(f"VAE decode: {total} tiles in {len(groups)} temporal groups", level=2)

        class ObservedTiles(list):
            def __iter__(self):
                nonlocal completed
                for tile in super().__iter__():
                    label = f"VAE decoding tile {completed + 1}/{total}"
                    started = time.monotonic()
                    progress.activity(f"{label}: starting", level=3)
                    yield tile
                    # The original loop has evaluated and blended this tile.
                    completed += 1
                    progress.completed(completed / total * 0.95)
                    progress.activity(f"{label}: decoded and blended in {time.monotonic() - started:.1f}s", level=3)

        return [ObservedTiles(group) for group in groups]

    def tiled_decode(decoder, latent, *args, **kwargs):
        total = latent.shape[2] * 8 - 7
        ready = 0
        index = 0
        progress.activity(f"VAE decoding {total} video frames: starting")
        for chunk in original_decode(decoder, latent, *args, **kwargs):
            index += 1
            ready += chunk.shape[2]
            progress.activity(f"VAE chunk {index}: {ready}/{total} frames decoded; sending to FFmpeg", level=2)
            yield chunk
            # Do not retain an extra GPU buffer while the next chunk decodes.
            del chunk
        progress.activity(f"VAE decode: {ready}/{total} frames queued; finishing FFmpeg writes")

    def phase_wrapper(method, label):
        def wrapped(*args, **kwargs):
            with progress.phase(label):
                return method(*args, **kwargs)
        return wrapped

    def native_ffmpeg(command):
        command = [command[0], "-hide_banner", "-stats_period", "1", *command[1:]]
        return ffmpeg_output(command, progress)

    with ExitStack() as stack:
        stack.enter_context(patch.object(video_vae, "_group_tiles_by_temporal_slice", group_tiles))
        stack.enter_context(patch.object(video_vae.VideoDecoder, "tiled_decode", tiled_decode))
        if progress.verbose >= 2:
            stack.enter_context(patch.object(video_vae, "_ffmpeg_sink", native_ffmpeg))
        for target, name, label in (
            (blocks.VideoDecoder, "load", "Loading video VAE weights"),
            (blocks.AudioDecoder, "__call__", "Preparing generated audio waveform"),
            (_orchestration, "save_waveform", "Materializing audio and writing temporary WAV"),
        ):
            stack.enter_context(patch.object(target, name, phase_wrapper(getattr(target, name), label)))
        yield


@contextmanager
def pipeline_activity(pipeline, progress, *, profile="distilled"):
    """Observe existing sync points without changing tensors or GPU scheduling.

    These hooks target RUNTIME_REVISION in quality_backend.py. Restore them on
    success, failure, and interruption; never edit installed runtime files.
    """
    from ltx_core_mlx.loader.block_streaming import BlockStreamer
    from ltx_pipelines_mlx.utils import samplers

    stage = 0
    step = 0
    steps = 0
    step_started = 0.0
    stage_started = 0.0
    hq = profile == "hq"
    if hq:
        from ltx_pipelines_mlx import ti2vid_two_stages_hq
        loop_owner = sys.modules[pipeline.__class__.__module__] if pipeline.__class__.__module__ == "audio_hq" else ti2vid_two_stages_hq
        loop_name = "res2s_denoise_loop"
    else:
        loop_owner, loop_name = pipeline, "_run_denoise_loop"
    original_loop = getattr(loop_owner, loop_name)
    original_bind = BlockStreamer.bind
    original_tqdm = samplers.tqdm

    def no_bar(*args, **kwargs):
        kwargs["disable"] = True
        return original_tqdm(*args, **kwargs)

    def bind(streamer, block, idx, *args, **kwargs):
        label = f"Stage {stage}/2, step {step}/{steps}"
        # The model has already synchronized the preceding block before the
        # next bind. This reports observed work, not time-based fake progress.
        progress.activity(
            f"{label}: loading transformer block {idx + 1}/{streamer.block_count} "
            f"from disk ({idx} blocks completed)", level=3,
        )
        # HQ repeats blocks for several guidance passes and two evaluations
        # per step. Only its native step callback advances completed work.
        if not hq and steps and streamer.block_count:
            progress.completed((step - 1 + idx / streamer.block_count) / steps)
        result = original_bind(streamer, block, idx, *args, **kwargs)
        with progress._lock:
            progress.operation = f"{label}: running transformer block {idx + 1}/{streamer.block_count} on Apple GPU"
        return result

    def denoise_loop(**kwargs):
        nonlocal stage, step, steps, step_started, stage_started
        stage += 1
        step = 1
        full_steps = len(kwargs["sigmas"]) - 1
        terminal = int(hq and kwargs["sigmas"][-1] == 0)
        steps = full_steps + terminal
        evaluations = 2 if hq else 1
        total_work = full_steps * evaluations + terminal
        stage_started = time.monotonic()
        step_started = stage_started
        progress.activity(f"Stage {stage}/2 denoising: 0% — {steps} steps")
        with progress._lock:
            if progress.estimate is not None:
                progress.estimate.start(f"stage{stage}", f"Stage {stage}/2 denoising")
        video_tokens = kwargs["video_state"].latent.shape[1]
        audio_tokens = kwargs["audio_state"].latent.shape[1]
        progress.log(f"Stage {stage}/2: {video_tokens:,} video + {audio_tokens:,} audio tokens per step", level=2)
        on_step = kwargs.get("on_step")

        def step_done(index, total, *args):
            nonlocal step, step_started
            if on_step is not None:
                on_step(index, total, *args)
            elapsed = time.monotonic() - stage_started
            # res2s includes a final single-evaluation denoise after the
            # configured two-evaluation steps. It is real remaining work.
            fraction = min((index + 1) * evaluations, total_work) / total_work
            remaining = elapsed / fraction * (1 - fraction)
            progress.completed(fraction)
            progress.activity(
                f"Stage {stage}/2 denoising: {fraction:.0%} — "
                f"step {index + 1}/{total} complete in {time.monotonic() - step_started:.1f}s; "
                f"stage ETA ~{remaining:.0f}s (excludes later stages)"
            )
            step = min(index + 2, total)
            step_started = time.monotonic()

        result = original_loop(**{**kwargs, "on_step": step_done})
        with progress._lock:
            if progress.estimate is not None:
                progress.estimate.finish(f"stage{stage}")
                if hq and stage == 1:
                    label = "Upscaling video and preparing full-resolution refinement"
                    progress.activity(label)
                    progress.estimate.start("upscale", label)
        return result

    def phase_wrapper(method, label, key):
        def wrapped(*args, **kwargs):
            with progress.phase(label, key):
                return method(*args, **kwargs)
        return wrapped

    with ExitStack() as stack:
        stack.enter_context(decoder_activity(progress))
        stack.enter_context(patch.object(samplers, "tqdm", no_bar))
        stack.enter_context(patch.object(BlockStreamer, "bind", bind))
        stack.enter_context(patch.object(loop_owner, loop_name, denoise_loop))
        phases = (
            ("_stage1", "Encoding prompt and preparing initial video/audio", "prompt"),
            ("_upsample_latent", "Upscaling video latents 2×", "upscale"),
            ("_stage2", "Preparing full-resolution refinement", "refine"),
            ("_load_decoders", "Loading video/audio decoders", "decoders"),
            ("_decode_and_save_video", "Decoding frames, generating audio, and writing native MP4", "decode"),
        )
        if hq:
            # HQ encodes and prepares its latents inline, then calls the
            # module's res2s sampler twice. Decoder methods are shared.
            phases = phases[-2:]
            label = "Encoding prompt and preparing initial video/audio"
            progress.activity(label)
            with progress._lock:
                if progress.estimate is not None:
                    progress.estimate.start("prompt", label)
        for name, label, key in phases:
            stack.enter_context(patch.object(pipeline, name, phase_wrapper(getattr(pipeline, name), label, key)))
        yield
