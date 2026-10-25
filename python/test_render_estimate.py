from contextlib import nullcontext
import io
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

from quality_progress import RenderProgress, pipeline_activity
from render_estimate import PHASE_SECONDS, RenderEstimate


class EstimateTests(unittest.TestCase):
    def setUp(self):
        self.now = 0.0
        self.events = []
        self.estimate = RenderEstimate(lambda **event: self.events.append(event),
                                       budgets={"stage1": 100, "stage2": 200, "validate": 10},
                                       clock=lambda: self.now)

    def test_time_weighted_progress_counts_later_stages_and_never_regresses(self):
        self.estimate.start("stage1", "Denoising")
        self.now = 50
        self.estimate.update(0.5)
        self.assertAlmostEqual(self.events[-1]["fraction"], 50 / 310)
        self.assertAlmostEqual(self.events[-1]["remaining_seconds"], 260)
        self.now = 100
        self.estimate.finish("stage1")
        self.estimate.start("stage2", "Refining")
        self.assertAlmostEqual(self.events[-1]["fraction"], 100 / 310)
        self.now = 200
        self.estimate.update(0.5)
        previous = self.events[-1]["fraction"]
        self.now = 400  # GPU stalls; the recalibrated ETA can grow.
        self.estimate.tick()
        self.assertGreaterEqual(self.events[-1]["fraction"], previous)
        self.assertGreater(self.events[-1]["remaining_seconds"], 0)
        self.estimate.finish("stage2")
        self.estimate.start("validate", "Validating")
        self.now = 410
        self.estimate.finish("validate")
        self.assertEqual(self.events[-1]["fraction"], 0.99)
        self.assertEqual(self.estimate.phase_seconds["stage1"], 100)

    def test_unmeasured_phase_keeps_a_reserve_when_it_overruns(self):
        self.estimate.start("stage1", "Starting")
        self.now = 2000
        self.estimate.tick()
        self.assertGreater(self.events[-1]["remaining_seconds"], 210)
        self.assertLess(self.events[-1]["fraction"], 1)

    def test_history_uses_compatible_successes_and_rejects_bad_timings(self):
        with tempfile.TemporaryDirectory() as directory:
            outputs = Path(directory)
            runtime = {"output_frames": 192, "runtime_revision": "pinned",
                       "elapsed_seconds": 1000}
            good = {"status": "complete", "width": 1280, "height": 704, "runtime": runtime}
            for name, report in [
                ("01", good), ("02", {**good, "status": "failed"}),
                ("03", {**good, "width": 640}),
                ("04", {**good, "runtime": {**runtime, "phase_seconds": {"stage1": float("nan")}}}),
                ("05", {**good, "runtime": {**runtime, "runtime_revision": "old"}}),
            ]:
                folder = outputs / name
                folder.mkdir()
                (folder / "render.json").write_text(json.dumps(report))
            broken = outputs / "06"
            broken.mkdir()
            (broken / "render.json").write_text("broken JSON")
            estimate = RenderEstimate.from_history(outputs, 192, 1280, 704, "pinned", None)
            self.assertAlmostEqual(sum(estimate.budgets.values()), 1000)
            self.assertAlmostEqual(estimate.budgets["stage1"] / estimate.budgets["stage2"],
                                   PHASE_SECONDS["stage1"] / PHASE_SECONDS["stage2"])
            runtime["phase_seconds"] = {key: value * 2 for key, value in PHASE_SECONDS.items()}
            (outputs / "01" / "render.json").write_text(json.dumps(good))
            estimate = RenderEstimate.from_history(outputs, 192, 1280, 704, "pinned", None)
            self.assertEqual(estimate.budgets["decode"], PHASE_SECONDS["decode"] * 2)

    def test_failed_phase_does_not_record_a_successful_timing(self):
        progress = RenderProgress(stream=io.StringIO(), estimate=self.estimate)
        with self.assertRaisesRegex(RuntimeError, "GPU failed"):
            with progress.phase("Generating", "stage1"):
                self.now = 5
                raise RuntimeError("GPU failed")
        self.assertEqual(self.estimate.phase_seconds, {})

    def test_pipeline_hooks_emit_block_progress_for_both_stages_and_restore_methods(self):
        owner = self

        class Streamer:
            block_count = 3

            def bind(self, block, index):
                owner.now += 1

        class Pipeline:
            def _run_denoise_loop(self, **kwargs):
                for index in range(len(kwargs["sigmas"]) - 1):
                    kwargs["model"]()
                    kwargs["on_step"](index, 2)

            def _stage1(self):
                owner.now += 2
                latent = types.SimpleNamespace(latent=types.SimpleNamespace(shape=(1, 100)))
                def model():
                    for index in range(3):
                        Streamer().bind(None, index)
                self._run_denoise_loop(model=model, on_step=None, sigmas=[1, 0.5, 0],
                                       video_state=latent, audio_state=latent)

            _stage2 = _stage1

            def _upsample_latent(self):
                pass

            def _load_decoders(self):
                pass

            def _decode_and_save_video(self):
                pass

        pipeline = Pipeline()
        estimate = RenderEstimate(lambda **event: self.events.append(event), clock=lambda: self.now)
        progress = RenderProgress(stream=io.StringIO(), estimate=estimate)
        modules = {
            "ltx_core_mlx.loader.block_streaming": types.SimpleNamespace(BlockStreamer=Streamer),
            "ltx_pipelines_mlx.utils": types.SimpleNamespace(samplers=types.SimpleNamespace(tqdm=lambda: None)),
        }
        original_bind = Streamer.bind
        original_stage = pipeline._stage1
        with patch.dict("sys.modules", modules), patch("quality_progress.decoder_activity", return_value=nullcontext()):
            with pipeline_activity(pipeline, progress):
                pipeline._stage1()
                pipeline._upsample_latent()
                pipeline._stage2()
            self.assertEqual(Streamer.bind, original_bind)
            self.assertEqual(pipeline._stage1, original_stage)
        estimate.finish("stage2")
        for phase in ("stage1", "stage2"):
            self.assertEqual(estimate.phase_seconds[phase], 6)
        self.assertGreater(sum(event["label"] == "Stage 1/2 denoising" for event in self.events), 6)
        fractions = [event["fraction"] for event in self.events]
        self.assertEqual(fractions, sorted(fractions))


if __name__ == "__main__":
    unittest.main()
