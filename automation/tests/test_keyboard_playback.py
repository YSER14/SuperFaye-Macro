import ctypes
import importlib.util
import sys
import threading
import unittest
from pathlib import Path
from unittest.mock import patch


@unittest.skipUnless(sys.platform == "win32", "输入包由 Windows 键盘扫描码生成")
class KeyboardPlaybackTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from engine import input_interception, input_sendinput

        cls.si = input_sendinput
        cls.interception = input_interception
        framework = Path(__file__).resolve().parents[2] / "宏执行框架3.2.py"
        spec = importlib.util.spec_from_file_location("keyboard_playback_test", framework)
        cls.executor = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.executor)
        cls.executor.USE_INTERCEPTION = False
        cls.executor.ENABLE_BATCH_SEND = True

    def test_single_key_packets_share_scan_codes_with_interception(self):
        cases = {
            "alt": (0x38, False), "alt_r": (0x38, True),
            "ctrl_r": (0x1D, True), "shift_r": (0x36, False),
            "backspace": (0x0E, False), "left": (0x4B, True),
            "delete": (0x53, True), "home": (0x47, True), "page_down": (0x51, True),
            "print_screen": (0x37, True), "win": (0x5B, True),
            "num1": (0x4F, False), "num_divide": (0x35, True), "numpad_enter": (0x1C, True),
        }
        for name, (scan, extended) in cases.items():
            with self.subTest(name=name), patch.object(self.interception, "get_interception_context") as context:
                packet = self.si.INPUT_STR()
                self.si.fill_key_input(packet, self.si.get_vk(name), True)
                self.assertEqual(packet.u.ki.wVk, 0)
                self.assertEqual(packet.u.ki.wScan, scan)
                self.assertEqual(packet.u.ki.dwFlags, 0x000A | extended)
                self.assertEqual(packet.u.ki.dwExtraInfo, self.si.MACRO_KEYBOARD_EVENT_TAG)
                self.assertTrue(self.interception.send_key_interception(name, True))
                context.return_value.send_key.assert_called_once_with(scan, 1, 2 if extended else 0)

    def test_pause_uses_virtual_key_without_trying_a_driver_packet(self):
        packet = self.si.INPUT_STR()
        self.si.fill_key_input(packet, self.si.get_vk("pause"))
        self.assertEqual(packet.u.ki.wVk, 0x13)
        self.assertEqual(packet.u.ki.dwFlags, 0)
        with patch.object(self.interception, "get_interception_context") as context:
            self.assertFalse(self.interception.send_key_interception("pause"))
            context.assert_not_called()

    def test_alt_number_tap_borrows_wait_without_moving_following_actions(self):
        script = [
            ["kd", "alt", 0], ["kd", "1", 0], ["ku", "1", 0],
            ["wait", 100], ["ku", "alt", 0], ["wait", 1615],
        ]
        compiled = self.executor.compile_macro(script)
        self.assertEqual([round(step[1] * 1000) for step in compiled], [0, 40, 0, 60, 0, 1615])
        batched = self.executor.batch_compiled_macro(compiled)
        self.assertAlmostEqual(sum(step[1] for step in batched), 1.715)
        self.assertEqual([(step[2], step[1]) for step in batched[:3]], [("kd", 0), ("kd", 0.04), ("ku", 0)])

    def test_batch_keeps_explicit_short_keyboard_delay_and_writes_extended_packet(self):
        compiled = self.executor.compile_macro([["kd", "alt_r", 0], ["kd", "left", 2], ["ku", "left", 0]])
        batched = self.executor.batch_compiled_macro(compiled)
        self.assertEqual([(step[2], step[1]) for step in batched], [("batch", 0.002), ("ku", 0)])
        packets = []

        def send(count, pointer, _size):
            items = ctypes.cast(pointer, ctypes.POINTER(self.si.INPUT_STR))
            packets.extend((items[i].u.ki.wScan, items[i].u.ki.dwFlags) for i in range(count))
            return count

        with patch.object(self.executor.user32, "SendInput", side_effect=send):
            for step in batched:
                step[0]()
        self.assertEqual(packets, [(0x38, 0x0009), (0x4B, 0x0009), (0x4B, 0x000B)])

    def test_stopping_between_key_down_and_key_up_releases_held_keys(self):
        end = threading.Event()
        events = []

        def send(vk, up):
            events.append((vk, up))
            if vk == 0x31 and not up:
                end.set()

        script = [["kd", "alt_l", 0], ["kd", "1", 40], ["ku", "1", 0], ["ku", "alt_l", 0]]
        with patch.object(self.executor, "send_key_event", side_effect=send), patch.object(self.executor, "send_mouse_click"):
            compiled = self.executor.compile_macro(script)
            self.executor.execute_macro_once(compiled, end, options={"test_trace": False})
        self.assertEqual(events, [(0xA4, False), (0x31, False), (0x31, True), (0xA4, True)])

    def test_recorder_preserves_keypad_and_unshifted_key_names(self):
        from pynput import keyboard
        from recorder.recorder_3d import normalize_key_name

        self.assertEqual(normalize_key_name(keyboard.KeyCode(vk=0x61, char="1")), "num1")
        self.assertEqual(normalize_key_name(keyboard.KeyCode(vk=0x31, char="!")), "1")
        self.assertEqual(normalize_key_name(keyboard.KeyCode(vk=0x41, char="\x01")), "a")

    def test_unrecognized_key_does_not_compile_into_invalid_input(self):
        with self.assertRaisesRegex(ValueError, "not-a-key"):
            self.executor.compile_macro([["kd", "not-a-key", 0]])
