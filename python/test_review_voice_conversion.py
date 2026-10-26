"""Offline PCM and lossless-remux contracts for voice conversion review."""

import importlib.util
import math
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import wave


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "experiments/character-consistency/review_voice_conversion.py"
spec = importlib.util.spec_from_file_location("review_voice_conversion", SCRIPT)
review = importlib.util.module_from_spec(spec)
spec.loader.exec_module(review)


class ReviewVoiceConversionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def _wav(self, path, frames):
        with wave.open(str(path), "wb") as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(review.SAMPLE_RATE)
            samples = [int(10000 * math.sin(index * 0.031)) for index in range(frames)]
            stream.writeframes(b"".join(sample.to_bytes(2, "little", signed=True) for sample in samples))

    def test_padding_preserves_prefix_and_refuses_trim_or_large_deficit(self):
        converted = self.root / "converted.wav"
        target = self.root / "mux.wav"
        self._wav(converted, review.TARGET_FRAMES - 255)
        info = review._make_mux_audio(converted, target)
        with wave.open(str(converted), "rb") as source, wave.open(str(target), "rb") as mux:
            prefix = source.readframes(source.getnframes())
            output = mux.readframes(mux.getnframes())
        self.assertEqual(info["tail_padding_samples"], 255)
        self.assertEqual(info["delay_correction_seconds"], 0.0)
        self.assertEqual(output[:len(prefix)], prefix)
        self.assertEqual(output[len(prefix):], b"\0\0" * 255)

        self._wav(converted, review.TARGET_FRAMES + 1)
        with self.assertRaisesRegex(ValueError, "refusing to trim"):
            review._make_mux_audio(converted, target)
        self._wav(converted, review.TARGET_FRAMES - 256)
        with self.assertRaisesRegex(ValueError, "maximum permitted tail padding"):
            review._make_mux_audio(converted, target)

    @unittest.skipUnless(shutil.which("ffmpeg"), "FFmpeg is required")
    def test_synthetic_h264_remux_preserves_every_frame_and_pcm(self):
        ffmpeg = shutil.which("ffmpeg")
        source = self.root / "source.mp4"
        generated = subprocess.run([
            ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
            "-f", "lavfi", "-i", "color=c=blue:s=128x72:r=24:d=4",
            "-an", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
            "-r", "24", "-frames:v", "96", str(source),
        ], capture_output=True)
        if generated.returncode:
            self.skipTest("Local FFmpeg does not have an H.264 encoder.")
        mux = self.root / "mux.wav"
        self._wav(mux, review.TARGET_FRAMES)
        result = review._remux_and_verify(ffmpeg, source, mux, self.root / "review")
        self.assertEqual(len(result["source_frames"]), 96)
        self.assertTrue(all(a["hash"] == b["hash"] for a, b in zip(result["source_frames"], result["converted_frames"])))
        self.assertEqual(len(result["lossless_frames"]), 96)
        self.assertTrue(result["frame_comparisons"]["mp4"]["all_match"])
        self.assertTrue(result["frame_comparisons"]["mov"]["all_match"])
        self.assertTrue(result["frame_comparisons"]["mov"]["pts_match"])
        self.assertTrue(result["frame_comparisons"]["mov"]["duration_match"])
        self.assertTrue(result["alac_pcm"]["exact_pcm_match"])
        self.assertGreaterEqual(result["aac_pcm"]["correlation"], review.EXPECTED_CORRELATION)

    def test_dtw_fields_report_token_alignment_not_word_centers(self):
        def transcript(words, alignments):
            return {"transcription": [{"tokens": [
                {"text": (" " if index else "") + word,
                 "offsets": {"from": index * 100, "to": (index + 1) * 100},
                 "t_dtw": round(alignment * 100)}
                for index, (word, alignment) in enumerate(zip(words, alignments))]}]}

        expected = "we still have some time".split()
        source = review._word_timings(transcript(expected, [1.68, 1.90, 2.08, 2.24, 2.54]))
        converted = review._word_timings(transcript(expected, [1.66, 1.86, 2.06, 2.22, 2.54]))
        comparison = review._compare_word_timings(source, converted)
        self.assertEqual(source[0]["t_dtw_token_alignment_seconds"], [1.68])
        self.assertIn("whisper_offset_start_estimate_seconds", source[0])
        self.assertNotIn("t_dtw_center_seconds", source[0])
        self.assertAlmostEqual(comparison[0]["t_dtw_alignment_delta_seconds"], -0.02)
        self.assertNotIn("t_dtw_center_delta_seconds", comparison[0])


if __name__ == "__main__":
    unittest.main()
