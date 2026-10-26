import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import wave

import speech_validation as speech


class TranscriptComparisonTests(unittest.TestCase):
    def test_punctuation_case_and_typographic_apostrophes_are_not_speech_errors(self):
        report = speech.compare_transcript("Good news. It’s definitely the car!", "good news, it's definitely the car.")
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["differences"], [])

    def test_missing_repeated_extra_substituted_and_reordered_words_fail(self):
        for transcript in (
            "Good news. It's the car.",
            "Good news. Good news. It's definitely the car.",
            "Good news. It's definitely the car. Goodbye.",
            "Good news. It's definitely a car.",
            "It's definitely the car. Good news.",
            "",
        ):
            with self.subTest(transcript=transcript):
                report = speech.compare_transcript("Good news. It's definitely the car.", transcript)
                self.assertEqual(report["status"], "mismatch")
                self.assertTrue(report["differences"])

    def test_non_dialogue_shot_rejects_any_recognized_words(self):
        self.assertEqual(speech.compare_transcript("", "")['status'], "passed")
        self.assertEqual(speech.compare_transcript("", "Thank you for watching.")['status'], "mismatch")

    def test_lexical_changes_are_not_normalized_into_matches(self):
        self.assertEqual(speech.compare_transcript("We cannot wait.", "We can't wait.")["status"], "mismatch")
        self.assertEqual(speech.compare_transcript("Twenty one.", "21.")["status"], "mismatch")


class ShotAudioValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.clip = self.root / "video.mp4"
        self.clip.write_bytes(b"video fixture")
        self.identity = {"executable": "/bin/test-whisper-cli", "model": "/models/test.bin"}

    def fake_commands(self, transcript, *, create_json=True):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            if command[0] == "ffmpeg":
                with wave.open(command[-1], "wb") as stream:
                    stream.setnchannels(1)
                    stream.setsampwidth(2)
                    stream.setframerate(16000)
                    stream.writeframes(b"\0" * 16000 * 2 * 4)
            elif create_json:
                destination = Path(command[command.index("--output-file") + 1] + ".json")
                destination.write_text(json.dumps({"transcription": transcript}), encoding="utf-8")
            return subprocess.CompletedProcess(command, 0, "", "recognizer diagnostics")

        return calls, run

    def test_actual_transcript_saved_and_script_never_supplied_to_recognizer(self):
        text = "The signal is below."
        segments = [{"text": text, "offsets": {"from": 200, "to": 2250}}]
        calls, run = self.fake_commands(segments)
        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=run):
            report = speech.validate_shot_audio(self.clip, text, self.root / "review")
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["last_speech_end_seconds"], 2.25)
        self.assertEqual(report["trailing_seconds_estimate"], 1.75)
        self.assertEqual(report["transcript"], text)
        self.assertEqual((self.root / "review/speech-transcript.txt").read_text(), text + "\n")
        saved = json.loads((self.root / "review/speech-validation.json").read_text())
        self.assertEqual(saved, report)
        self.assertEqual(json.loads((self.root / "review/speech-recognition.json").read_text())["transcription"], segments)
        self.assertNotIn("--prompt", calls[1])
        self.assertNotIn(text, " ".join(calls[1]))
        self.assertNotIn("--grammar", calls[1])
        self.assertIn("--no-gpu", calls[1])
        self.assertEqual(calls[1][calls[1].index("--language") + 1], "auto")
        self.assertEqual(self.clip.read_bytes(), b"video fixture")

    def test_segments_without_leading_spaces_do_not_merge_words(self):
        segments = [{"text": "Hello", "offsets": {"from": 0, "to": 800}},
                    {"text": "world.", "offsets": {"from": 900, "to": 1500}}]
        _, run = self.fake_commands(segments)
        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=run):
            report = speech.validate_shot_audio(self.clip, "Hello world.")
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["transcript"], "Hello world.")

    def test_extra_tail_speech_raises_with_saved_difference_report(self):
        segments = [
            {"text": " He was an optimistic man.", "offsets": {"from": 0, "to": 2100}},
            {"text": " Good night.", "offsets": {"from": 2800, "to": 3900}},
        ]
        _, run = self.fake_commands(segments)
        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=run), \
                self.assertRaises(speech.SpeechValidationError) as raised:
            speech.validate_shot_audio(self.clip, "He was an optimistic man.")
        self.assertEqual(raised.exception.report["status"], "mismatch")
        self.assertEqual(raised.exception.report["differences"][0]["recognized"], ["good", "night"])
        self.assertEqual(json.loads((self.root / "speech-validation.json").read_text()), raised.exception.report)

    def test_missing_recognizer_fails_closed_and_saves_error(self):
        with patch.object(speech, "check_available", side_effect=RuntimeError("Recognizer unavailable")), \
                patch.object(speech.subprocess, "run") as run, \
                self.assertRaises(speech.SpeechValidationError) as raised:
            speech.validate_shot_audio(self.clip, "Hello.")
        run.assert_not_called()
        self.assertEqual(raised.exception.report["status"], "failed")
        self.assertIn("unavailable", (self.root / "speech-validation.json").read_text())

    def test_stale_recognition_cannot_satisfy_failed_recognizer(self):
        old = {"transcription": [{"text": "Hello.", "offsets": {"from": 0, "to": 1000}}]}
        (self.root / "speech-recognition.json").write_text(json.dumps(old))
        _, run = self.fake_commands([], create_json=False)
        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=run), \
                self.assertRaises(speech.SpeechValidationError) as raised:
            speech.validate_shot_audio(self.clip, "Hello.")
        self.assertEqual(raised.exception.report["status"], "failed")
        self.assertFalse((self.root / "speech-recognition.json").exists())

    def test_recognizer_failure_keeps_diagnostics(self):
        _, run = self.fake_commands([])

        def fail(command, **kwargs):
            if command[0] == "ffmpeg":
                return run(command, **kwargs)
            raise subprocess.CalledProcessError(2, command, stderr="could not decode model")

        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=fail), \
                self.assertRaises(speech.SpeechValidationError):
            speech.validate_shot_audio(self.clip, "Hello.")
        self.assertEqual((self.root / "speech-recognition.log").read_text(), "could not decode model")

    def test_empty_transcript_is_valid_only_without_expected_speech(self):
        _, run = self.fake_commands([])
        with patch.object(speech, "check_available", return_value=self.identity), \
                patch.object(speech.subprocess, "run", side_effect=run):
            report = speech.validate_shot_audio(self.clip, "")
            self.assertEqual(report["status"], "passed")
            self.assertIsNone(report["last_speech_end_seconds"])
            with self.assertRaises(speech.SpeechValidationError):
                speech.validate_shot_audio(self.clip, "Hello.")

    def test_malformed_recognizer_output_never_passes(self):
        for invalid in ([{}], [{"text": "Hello.", "offsets": {"from": 2000, "to": 1000}}],
                        [{"text": "Hello.", "offsets": {"from": float("nan"), "to": 1000}}]):
            with self.subTest(invalid=invalid):
                _, run = self.fake_commands(invalid)
                with patch.object(speech, "check_available", return_value=self.identity), \
                        patch.object(speech.subprocess, "run", side_effect=run), \
                        self.assertRaises(speech.SpeechValidationError) as raised:
                    speech.validate_shot_audio(self.clip, "Hello.")
                self.assertEqual(raised.exception.report["status"], "failed")


class SpeechSetupTests(unittest.TestCase):
    def test_incompatible_cli_is_rejected_before_reading_model_or_downloading(self):
        result = subprocess.CompletedProcess(["whisper-cli", "--help"], 0, "", "old CLI --model --file")
        with patch.object(speech.shutil, "which", return_value="/bin/whisper-cli"), \
                patch.object(speech.subprocess, "run", return_value=result), \
                patch.object(speech, "_sha256") as digest, \
                patch.object(speech.urllib.request, "urlopen") as download:
            with self.assertRaisesRegex(RuntimeError, "incompatible.*--output-json-full"):
                speech.check_available()
            with self.assertRaisesRegex(RuntimeError, "incompatible"):
                speech.setup()
            digest.assert_not_called()
            download.assert_not_called()

    def test_cli_readiness_reads_stderr_and_fails_on_execution_error(self):
        result = subprocess.CompletedProcess(["whisper-cli", "--help"], 0, "", "\n".join(speech._CLI_OPTIONS))
        with patch.object(speech.subprocess, "run", return_value=result):
            speech._check_cli("whisper-cli")
        with patch.object(speech.subprocess, "run", side_effect=subprocess.TimeoutExpired("whisper-cli", 15)), \
                self.assertRaisesRegex(RuntimeError, "could not start"):
            speech._check_cli("whisper-cli")

    def test_download_is_pinned_verified_atomic_and_reused(self):
        content = b"model weights"
        digest = hashlib.sha256(content).hexdigest()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(speech.shutil, "which", return_value="/bin/whisper-cli"), \
                patch.object(speech, "_check_cli"), \
                patch.object(speech, "MODEL_BYTES", len(content)), \
                patch.object(speech, "MODEL_SHA256", digest), \
                patch.object(speech.urllib.request, "urlopen", return_value=io.BytesIO(content)) as download:
            root = Path(directory)
            first = speech.setup(root)
            second = speech.setup(root)
            self.assertEqual(first, second)
            download.assert_called_once()
            self.assertIn(speech.MODEL_REVISION, download.call_args.args[0])
            self.assertEqual(Path(first["model"]).read_bytes(), content)
            self.assertEqual(list(Path(first["model"]).parent.iterdir()), [Path(first["model"])])

    def test_bad_download_preserves_existing_model_and_removes_temporary_file(self):
        content = b"bad model"
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(speech.shutil, "which", return_value="/bin/whisper-cli"), \
                patch.object(speech, "_check_cli"), \
                patch.object(speech, "MODEL_BYTES", len(content)), \
                patch.object(speech, "MODEL_SHA256", "0" * 64), \
                patch.object(speech.urllib.request, "urlopen", return_value=io.BytesIO(content)):
            root = Path(directory)
            model = root / ".cache/whisper" / speech.MODEL_FILENAME
            model.parent.mkdir(parents=True)
            model.write_bytes(b"previous model")
            with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                speech.setup(root)
            self.assertEqual(model.read_bytes(), b"previous model")
            self.assertEqual(list(model.parent.iterdir()), [model])


if __name__ == "__main__":
    unittest.main()
