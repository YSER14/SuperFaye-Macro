# -*- coding: utf-8 -*-
"""反向编译自测：旧宏行 / XMBC 宏串 -> 节点图。"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

from automation.compiler import parse_graph                      # noqa: E402
from automation.registry import PluginRegistry, register_builtin_nodes  # noqa: E402
from macro_io.reverse import reverse_macro, rows_to_graph, xmbc_text_to_rows  # noqa: E402

register_builtin_nodes()


def node_types(graph):
    return [n["type"] for n in graph["graph"]["nodes"]]


def find(graph, ntype):
    for n in graph["graph"]["nodes"]:
        if n["type"] == ntype:
            return n
    return None


class ReverseRowsTest(unittest.TestCase):
    def test_chain_has_start_and_end(self):
        g = rows_to_graph([["kd", "w", 20], ["ku", "w", 0]])
        types = node_types(g)
        self.assertEqual(types[0], "flow/start")
        self.assertEqual(types[-1], "flow/end")
        self.assertEqual(types.count("action/key_action"), 2)

    def test_key_delay_is_carried(self):
        g = rows_to_graph([["kd", "w", 37]])
        node = find(g, "action/key_action")
        self.assertEqual(node["params"]["key"], "w")
        self.assertEqual(node["params"]["action"], "press")
        self.assertEqual(node["params"]["delay"], 37.0)

    def test_mouse_down_up_uses_原地节点(self):
        g = rows_to_graph([["md", "left", 0], ["mu", "left", 5]])
        mouse = [n for n in g["graph"]["nodes"] if n["type"] == "action/mouse_button"]
        self.assertEqual(len(mouse), 2)
        self.assertEqual([m["params"]["click_type"] for m in mouse], ["press", "release"])
        self.assertEqual(mouse[1]["params"]["delay"], 5.0)

    def test_side_button_is_skipped_with_warning(self):
        g = rows_to_graph([["md", "x1", 0]])
        self.assertFalse([n for n in g["graph"]["nodes"] if n["type"] == "action/mouse_button"])
        self.assertTrue(any("侧键" in w for w in g["warnings"]))

    def test_wait_view_scroll_import(self):
        g = rows_to_graph([
            ["wait", 250],
            ["view", [12, -3], 80],
            ["scroll", 120, 0],
            ["import", "宏/子宏.json"],
        ])
        self.assertEqual(find(g, "action/wait")["params"]["duration"], 250.0)
        rel = find(g, "action/mouse_move_rel")
        self.assertEqual((rel["params"]["dx"], rel["params"]["dy"]), (12.0, -3.0))
        self.assertEqual(find(g, "action/mouse_scroll")["params"]["delta"], 120)
        self.assertEqual(find(g, "script_block/call_script_sync")["params"]["path"], "宏/子宏.json")

    def test_nested_loop_wires_body_port(self):
        g = rows_to_graph([
            ["kd", "w", 0],
            ["loop", 3, [["md", "left", 0], ["wait", 10], ["mu", "left", 0]]],
            ["ku", "w", 0],
        ])
        loop = find(g, "control/loop_count")
        self.assertEqual(loop["params"]["count"], 3)
        body_edges = [c for c in g["graph"]["connections"]
                      if c["from"]["node"] == loop["id"] and c["from"]["port"] == "body"]
        self.assertEqual(len(body_edges), 1)
        body_first = body_edges[0]["to"]["node"]
        first_body_node = [n for n in g["graph"]["nodes"] if n["id"] == body_first][0]
        self.assertEqual(first_body_node["type"], "action/mouse_button")

    def test_note_becomes_annotation(self):
        g = rows_to_graph([["note", "这里原来手按了三次"], ["wait", 5]])
        self.assertEqual(len(g["graph"]["annotations"]), 1)
        self.assertIn("三次", g["graph"]["annotations"][0]["text"])

    def test_empty_macro_still_valid(self):
        g = rows_to_graph([])
        self.assertEqual(node_types(g), ["flow/start", "flow/end"])
        self.assertTrue(g["warnings"])

    def test_all_produced_types_are_registered(self):
        g = rows_to_graph([
            ["kd", "a", 0], ["ku", "a", 0], ["md", "left", 0], ["mu", "left", 0],
            ["wait", 1], ["view", [1, 1], 1], ["scroll", 120, 0],
            ["loop", 2, [["wait", 2]]], ["import", "a.json"],
        ])
        for ntype in set(node_types(g)):
            self.assertIsNotNone(PluginRegistry.get_node(ntype), "节点未注册：%s" % ntype)

    def test_parse_graph_accepts_generated_graph(self):
        g = rows_to_graph([["kd", "w", 0], ["loop", 2, [["wait", 5]]], ["ku", "w", 0]])
        parsed = parse_graph(g)
        self.assertEqual(parsed.errors, [])
        self.assertEqual(parsed.entry_node_id, "1")


class ReverseXmbcTest(unittest.TestCase):
    def test_simple_click_macro(self):
        rows, warns = xmbc_text_to_rows("{LMB}{WAITMS:300}{LMB}")
        self.assertEqual(rows, [["md", "left", 0], ["mu", "left", 0],
                                ["wait", 300],
                                ["md", "left", 0], ["mu", "left", 0]])
        self.assertEqual(warns, [])

    def test_down_up_and_flush(self):
        rows, warns = xmbc_text_to_rows("{FLUSH}{LMBD}{WAITMS:130}{LMBU}")
        self.assertEqual(rows[0][0], "note")          # FLUSH 无等价，落注释
        self.assertIn(["md", "left", 0], rows)
        self.assertIn(["wait", 130], rows)
        self.assertIn(["mu", "left", 0], rows)
        self.assertTrue(any("FLUSH" in w for w in warns))

    def test_modifier_chord(self):
        rows, _ = xmbc_text_to_rows("{CTRL}c")
        self.assertEqual(rows, [["kd", "ctrl", 0], ["kd", "c", 0],
                                ["ku", "c", 0], ["ku", "ctrl", 0]])

    def test_modifier_does_not_leak_to_next_key(self):
        rows, warns = xmbc_text_to_rows("{CTRL}c{WAITMS:10}d")
        self.assertEqual(rows, [["kd", "ctrl", 0], ["kd", "c", 0], ["ku", "c", 0], ["ku", "ctrl", 0],
                                ["wait", 10],
                                ["kd", "d", 0], ["ku", "d", 0]])
        self.assertEqual(warns, [])

    def test_upper_case_char_gets_shift(self):
        rows, _ = xmbc_text_to_rows("A")
        self.assertEqual(rows, [["kd", "shift", 0], ["kd", "a", 0],
                                ["ku", "a", 0], ["ku", "shift", 0]])

    def test_hold_ms(self):
        rows, _ = xmbc_text_to_rows("{HOLDMS:50}r")
        self.assertEqual(rows, [["kd", "r", 0], ["wait", 50], ["ku", "r", 0]])

    def test_random_delay_takes_midpoint(self):
        rows, warns = xmbc_text_to_rows("{WAITMS:80-140}")
        self.assertEqual(rows, [["wait", 110]])
        self.assertTrue(any("随机" in w for w in warns))

    def test_vkc_maps_to_key_name(self):
        rows, _ = xmbc_text_to_rows("{VKC:65}")
        self.assertEqual(rows, [["kd", "a", 0], ["ku", "a", 0]])

    def test_unknown_vkc_kept_as_note(self):
        rows, warns = xmbc_text_to_rows("{VKC:164}")
        self.assertEqual(rows[0][0], "note")
        self.assertTrue(any("VKC:164" in w for w in warns))

    def test_press_release_only(self):
        rows, _ = xmbc_text_to_rows("{PRESS}e")
        self.assertEqual(rows, [["kd", "e", 0]])
        rows, _ = xmbc_text_to_rows("{RELEASE}e")
        self.assertEqual(rows, [["ku", "e", 0]])

    def test_wheel_and_tilt(self):
        rows, warns = xmbc_text_to_rows("{MWUP}{MWDN}{TILTL}")
        self.assertIn(["scroll", 120, 0], rows)
        self.assertIn(["scroll", -120, 0], rows)
        self.assertTrue(any("倾斜" in w for w in warns))

    def test_full_macro_to_graph_roundtrip(self):
        text = "{FLUSH}{OU}{LMBU}{OU}{RMBU}{WAITMS:120}{HOLDMS:50}r{CTRL}c{LAYER:next}"
        g = reverse_macro(text=text, name="第三方宏")
        parsed = parse_graph(g)
        self.assertEqual(parsed.errors, [])
        self.assertEqual(g["meta"]["name"], "第三方宏")
        self.assertGreaterEqual(len(g["graph"]["annotations"]), 3)   # FLUSH / OU / LAYER


if __name__ == "__main__":
    unittest.main(verbosity=2)
