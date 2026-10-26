import unittest

from dialogue_helpers import duration_estimate


class DurationTests(unittest.TestCase):
    def test_screenplay_reserves_action_time_without_counting_directions_as_speech(self):
        prompt = '''Cast: @eli plays JOE; @ava plays SUE. Keep a wide camera.

EXT. DESERT ROAD - DUSK

Sue holds a postcard. Joe steps out from behind the engine.

JOE: "Good news. It's definitely the car."

Sue hands him the postcard.

SUE: "He made it sound closer."

Joe lowers the hood.

JOE (looking around): "We've still got some light."

Sue pockets the postcard. They start walking.

FADE OUT.
'''
        timing = duration_estimate(prompt)
        speech_only = duration_estimate('\n'.join([
            '"Good news. It\'s definitely the car."',
            '"He made it sound closer."',
            '"We\'ve still got some light."',
        ]))
        self.assertEqual(timing["dialogue"], speech_only["dialogue"])
        self.assertEqual(timing["word_count"], 16)
        self.assertEqual(timing["turn_count"], 3)
        self.assertEqual(timing["action_count"], 4)
        self.assertEqual(timing["action_seconds"], 12)
        self.assertEqual(timing["seconds"], speech_only["seconds"] + 12)
        self.assertEqual(timing["method"], "screenplay-dialogue-and-action-estimate")

    def test_wrapping_does_not_add_beats_and_scene_headings_and_transitions_are_excluded(self):
        prompt = '''EXT. DESERT ROAD - DUSK

Joe walks to the car and
lowers the hood.

JOE (quietly): "Ready?"

CUT TO:

INT. STATION WAGON - NIGHT

Sue sits behind the wheel.

(after a pause)

SUE: "Let's go."

FADE OUT.
'''
        timing = duration_estimate(prompt)
        self.assertEqual(timing["action_count"], 2)
        self.assertEqual(timing["action_seconds"], 6)
        self.assertEqual(timing["turn_count"], 2)
        self.assertEqual(timing["seconds"], 9)

    def test_prose_camera_and_motion_directions_keep_existing_speech_timing(self):
        timing = duration_estimate(
            'A wide shot. Eli walks across the desert and says '
            '"hello everyone my name is Eli and I am so excited to be here talking with you".'
        )
        self.assertEqual(timing["seconds"], 8)
        self.assertEqual(timing["method"], "quoted-dialogue-estimate")
        self.assertNotIn("action_count", timing)

    def test_outer_prompt_quotes_preserve_screenplay_timing(self):
        prompt = 'EXT. ROAD - DAY\n\nJoe walks away.\n\nJOE: "Bye."\n\nFADE OUT.'
        self.assertEqual(duration_estimate(f'"{prompt}"'), duration_estimate(prompt))


if __name__ == "__main__":
    unittest.main()
