import importlib.util
import sys
import threading
import unittest
from pathlib import Path


@unittest.skipUnless(sys.platform == "win32", "宏执行框架使用 Windows 键鼠监听")
class MacroTriggerTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[2]
        spec = importlib.util.spec_from_file_location("macro_executor_test", root / "宏执行框架3.2.py")
        self.executor = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.executor)
        self.key = "mouse_x2"
        self.executor.MACRO_BINDINGS = {self.key: ["test.json"]}
        self.executor.MACRO_RUNTIME_SETTINGS = {self.key: {"repeat": 1}}
        self.executor.refresh_compiled_cache_for_trigger = lambda *args, **kwargs: {
            "entries": [{"macro_name": "test", "compiled_macro": []}]
        }
        self.executor.boost_thread_priority = lambda: None
        self.executor.ENABLE_PRO_AUDIO = False
        self.executor.ENABLE_THREAD_CORE_BINDING = False
        self.executor.execute_macro_once = lambda _macro, stop, *_args: stop.wait(2)

    def tearDown(self):
        self.executor.stop_all_macros()
        self.assertTrue(self.executor.wait_trigger_macros(self.key))

    def mode(self, name):
        self.executor.MACRO_RUNTIME_SETTINGS[self.key]["running_press_mode"] = name

    def edge(self, pressed):
        self.executor.on_mouse_click(0, 0, self.executor.mouse.Button.x2, pressed)

    def instance(self):
        instances = self.executor._collect_live_macro_infos(self.key)
        self.assertEqual(len(instances), 1)
        return instances[0]

    def test_normal_does_not_stop_on_release_or_add_an_instance(self):
        self.edge(True)
        first = self.instance()
        self.edge(False)
        self.edge(True)
        self.assertIs(first, self.instance())
        self.assertFalse(first["stop_event"].is_set())

    def test_pause_resume_starts_then_toggles(self):
        self.mode("pause_resume")
        self.edge(True)
        pause = self.instance()["pause_event"]
        self.assertFalse(pause.is_set())
        self.edge(True)
        self.assertFalse(pause.is_set())
        self.edge(False)
        self.edge(True)
        self.assertTrue(pause.is_set())
        self.edge(False)
        self.edge(True)
        self.assertFalse(pause.is_set())

    def test_hold_pause_starts_on_release(self):
        self.mode("hold_pause")
        self.edge(True)
        pause = self.instance()["pause_event"]
        self.assertTrue(pause.is_set())
        self.edge(False)
        self.assertFalse(pause.is_set())
        self.edge(True)
        self.assertTrue(pause.is_set())

    def test_release_pause_resumes_the_same_instance(self):
        self.mode("release_pause")
        self.edge(True)
        first = self.instance()
        self.edge(False)
        self.assertTrue(first["pause_event"].is_set())
        self.edge(True)
        self.assertIs(first, self.instance())
        self.assertFalse(first["pause_event"].is_set())

    def test_release_stop_can_start_again_during_cleanup(self):
        self.mode("release_stop")
        cleanup = threading.Event()

        def execute(_macro, stop, *_args):
            stop.wait(2)
            cleanup.wait(0.04)

        self.executor.execute_macro_once = execute
        self.edge(True)
        first = self.instance()
        self.edge(False)
        self.assertTrue(first["stop_event"].is_set())
        self.edge(True)
        self.assertIsNot(first, self.instance())

    def test_stop_restart_replaces_the_previous_instance(self):
        self.mode("stop_restart")
        self.edge(True)
        first = self.instance()
        self.edge(False)
        self.edge(True)
        self.assertTrue(first["stop_event"].is_set())
        self.assertIsNot(first, self.instance())

    def test_parallel_keeps_all_instances_addressable_by_trigger(self):
        self.mode("parallel_trigger")
        for _ in range(3):
            self.edge(True)
            self.edge(False)
        instances = self.executor._collect_live_macro_infos(self.key)
        self.assertEqual(len(instances), 3)
        self.executor.stop_macros_by_trigger(self.key)
        self.assertTrue(all(info["stop_event"].is_set() for info in instances))

    def test_repeating_pause_hotkey_toggles_only_once(self):
        self.executor.GLOBAL_PAUSE_TOGGLE_KEY = "f8"
        key = self.executor.keyboard.Key.f8
        self.executor.on_key_press(key)
        self.executor.on_key_press(key)
        self.assertTrue(self.executor.get_listener_state()["paused"])
        self.edge(True)
        self.assertEqual(self.executor._collect_live_macro_infos(self.key), [])
        self.edge(False)
        self.executor.on_key_release(key)
        self.executor.on_key_press(key)
        self.assertFalse(self.executor.get_listener_state()["paused"])
        self.edge(True)
        self.instance()

    def test_trial_skips_hold_behavior_and_repetition(self):
        from bridge.listener_api import ListenerApiMixin

        class Bridge(ListenerApiMixin):
            _ok = staticmethod(dict)
            _fail = staticmethod(lambda reason: {"error": reason})

        self.mode("hold_pause")
        self.executor.MACRO_RUNTIME_SETTINGS[self.key]["repeat"] = 5
        calls = []
        self.executor.execute_macro_once = lambda _macro, _stop, pause, *_args: calls.append(pause.is_set())
        bridge = Bridge()
        bridge.executor = self.executor
        self.assertEqual(bridge.trigger_macro(self.key), {"trigger": self.key})
        self.assertTrue(self.executor.wait_trigger_macros(self.key))
        self.assertEqual(calls, [False])
