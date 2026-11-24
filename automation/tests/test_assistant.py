import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from pydantic_ai.messages import ModelResponse, RetryPromptPart, TextPart, ToolCallPart, ToolReturnPart
from pydantic_ai.models.function import FunctionModel

from automation.assistant_agent import EditorContext, _make_agent
from automation.assistant_drafts import DraftError, validate_graph, validate_macro
from automation.registry import PluginRegistry, register_builtin_nodes
from core.assistant_model import AssistantModel, ModelError, WORKBUDDY_ENDPOINT, normalize_endpoint


def node(node_id, node_type, **params):
    return {"id": str(node_id), "type": node_type, "params": params}


def link(origin, output, target, input_port, kind="flow"):
    return {"from": {"node": str(origin), "port": output}, "to": {"node": str(target), "port": input_port},
            "type": kind}


class DraftValidationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        register_builtin_nodes()
        cls.definitions = PluginRegistry.get_all_nodes()

    def test_macro_duration_and_missing_release(self):
        result = validate_macro([["loop", 3, [["wait", 25], ["kd", "a", 5], ["ku", "a", 2]]],
                                 ["md", "left", 20]])
        self.assertEqual(result["stats"], {"actions": 10, "duration_ms": 116})
        self.assertIn("left", result["warnings"][0])

    def test_macro_disallows_multiline_notes_and_unknown_keys(self):
        for rows in ([["note", "说明\n[\"wait\",999]"]], [["kd", "invented", 5]],
                     [["import", "宏/../config.json"]], [["loop", 1.5, [["wait", 10]]]]):
            with self.subTest(rows=rows), self.assertRaises(DraftError):
                validate_macro(rows)

    def test_graph_keeps_existing_parameters_and_annotations(self):
        start = node(1, "flow/start")
        wait = node(2, "action/wait", duration=100)
        wait["position"] = {"x": 42, "y": 60}
        annotation = {"id": 2, "text": "保留原注释"}
        source = {"graph": {"nodes": [start, wait], "annotations": [annotation]}}
        draft = {"graph": {"nodes": [start, node(2, "action/wait")],
                           "connections": [link(1, "next", 2, "in")]}}
        graph = validate_graph(draft, self.definitions, source)["document"]["graph"]
        self.assertEqual(graph["nodes"][1], wait)
        self.assertEqual(graph["annotations"], [annotation])

    def test_graph_rejects_duplicate_and_dangling_links(self):
        start = node(1, "flow/start")
        end = node(2, "flow/end")
        valid = link(1, "next", 2, "in")
        for nodes, connections in (
            ([start, start], []),
            ([start, end], [valid, valid]),
            ([start, end], [link(1, "next", 3, "in")]),
            ([start, end], [link(1, "unknown", 2, "in")]),
        ):
            with self.subTest(nodes=nodes, connections=connections), self.assertRaises(DraftError):
                validate_graph({"graph": {"nodes": nodes, "connections": connections}}, self.definitions)

    def test_graph_rejects_data_cycles(self):
        nodes = [node(1, "flow/start"), node(2, "utility/math_op"), node(3, "utility/math_op")]
        connections = [link(2, "result", 3, "A", "data"), link(3, "result", 2, "A", "data")]
        with self.assertRaisesRegex(DraftError, "循环依赖"):
            validate_graph({"graph": {"nodes": nodes, "connections": connections}}, self.definitions)

    def test_wait_condition_requires_timeout_and_input(self):
        nodes = [node(1, "flow/start"), node(2, "control/wait_cond", timeout=4000)]
        with self.assertRaisesRegex(DraftError, "超时出口"):
            validate_graph({"graph": {"nodes": nodes, "connections": [link(1, "next", 2, "in")]}},
                           self.definitions)


class ModelSettingsTest(unittest.TestCase):
    def test_fresh_install_cannot_contact_a_model_without_configuration(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict("os.environ", {}, clear=True):
            model = AssistantModel(Path(temp) / "config.json")
            self.assertEqual(model.settings(), {"endpoint": "", "model": "", "key_source": ""})
            with patch.object(model, "_request_json") as request:
                with self.assertRaises(ModelError):
                    model.list_models()
                request.assert_not_called()

    def test_normalizes_base_and_completion_paths(self):
        endpoint = "http://127.0.0.1:11434/v1/chat/completions"
        self.assertEqual(normalize_endpoint("http://127.0.0.1:11434/v1/"), endpoint)
        self.assertEqual(normalize_endpoint(endpoint), endpoint)
        with self.assertRaises(ModelError):
            normalize_endpoint("http://example.test/v1")
        with self.assertRaises(ModelError):
            normalize_endpoint("https://example.test/v1?token=secret")

    def test_key_survives_model_edit_but_is_not_saved_or_carried_to_another_endpoint(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "config.json"
            model = AssistantModel(path)
            self.assertEqual(model.configure("https://example.test/v1", "one", "test-key")["key_source"], "session")
            self.assertEqual(model.configure("https://example.test/v1", "two")["key_source"], "session")
            self.assertNotIn("test-key", path.read_text(encoding="utf-8"))
            self.assertEqual(AssistantModel(path).settings()["key_source"], "")
            self.assertEqual(model.configure("https://second.example.test/v1", "two")["key_source"], "")

    def test_workbuddy_catalog_uses_enabled_key_and_selects_listed_default(self):
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp)
            workbuddy = workspace / "workbuddy.json"
            workbuddy.write_text(json.dumps({"api_keys": [
                {"enabled": False, "key": "disabled-key"}, {"enabled": True, "key": "fixture-key"},
            ]}), encoding="utf-8")
            local_config = workspace / "assistant.json"
            with patch.dict("os.environ", {"MACRO_AI_BASE_URL": WORKBUDDY_ENDPOINT}):
                model = AssistantModel(local_config, workbuddy)
            catalog = {"data": [{"id": "other-model"}, {"id": "deepseek-v4.1-flash"}, {"id": "other-model"}]}
            with patch.object(model, "_request_json", return_value=catalog) as request:
                result = model.list_models()
                request.assert_called_once_with("http://127.0.0.1:8790/v1/models", "fixture-key", timeout=8)
            self.assertEqual(result["models"], ["other-model", "deepseek-v4.1-flash"])
            self.assertEqual(model.settings(), {"endpoint": WORKBUDDY_ENDPOINT, "model": "deepseek-v4.1-flash",
                                                "key_source": "workbuddy"})
            self.assertFalse(local_config.exists())

    def test_thinking_controls_use_declared_workbuddy_capabilities(self):
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp)
            workbuddy = workspace / "workbuddy.json"
            workbuddy.write_text('{"api_keys":[{"enabled":true,"key":"fixture-key"}]}', encoding="utf-8")
            with patch.dict("os.environ", {"MACRO_AI_BASE_URL": WORKBUDDY_ENDPOINT}):
                model = AssistantModel(workspace / "assistant.json", workbuddy)
            with patch.object(model, "_request_json", return_value={"data": [
                {"id": "tunable", "reasoning_efforts": ["low", "high"], "reasoning_can_disable": True},
                {"id": "fixed", "reasoning_fixed_effort": "medium"},
            ]}):
                model.list_models()
            self.assertEqual(model.agent_connection("tunable", "off")[-1],
                             {"thinking": {"type": "disabled"}, "reasoning_effort": "none"})
            self.assertEqual(model.agent_connection("tunable", "low")[-1],
                             {"thinking": {"type": "enabled"}, "reasoning_effort": "low"})
            self.assertEqual(model.agent_connection("fixed", "auto")[-1]["reasoning_effort"], "medium")
            with self.assertRaises(ModelError):
                model.agent_connection("fixed", "off")


class AssistantAgentTest(unittest.TestCase):
    def test_pydantic_agent_retries_failed_native_validation(self):
        requests = []
        phases = []

        def respond(messages, _info):
            requests.append(messages[-1])
            rows = [["invented", 80]] if len(requests) == 1 else [["wait", 80]]
            return ModelResponse(parts=[TextPart(json.dumps({"kind": "draft", "message": "已修改", "rows": rows}))])

        context = EditorContext("macro", "new", None, {}, phases.append)
        asyncio.run(_make_agent(FunctionModel(respond), context).run("等待 80 毫秒", deps=context))
        self.assertEqual(context.checked_result["rows"], [["wait", 80]])
        self.assertEqual(len(requests), 2)
        self.assertTrue(any(isinstance(part, RetryPromptPart) for part in requests[1].parts))
        self.assertIn("repairing", phases)

    def test_pydantic_agent_reads_the_selected_rows_as_a_tool(self):
        requests = []
        phases = []

        def respond(messages, info):
            requests.append(messages[-1])
            if len(requests) == 1:
                self.assertEqual([tool.name for tool in info.function_tools], ["read_editor_context"])
                return ModelResponse(parts=[ToolCallPart("read_editor_context", {}, "editor-context")])
            tool_return = next(part for part in requests[-1].parts if isinstance(part, ToolReturnPart))
            self.assertEqual(tool_return.content, [["wait", 100]])
            output = {"kind": "draft", "message": "选中动作已修改", "rows": [["wait", 80]]}
            return ModelResponse(parts=[TextPart(json.dumps(output))])

        context = EditorContext("macro", "selection", [["wait", 100]], {}, phases.append)
        asyncio.run(_make_agent(FunctionModel(respond), context).run("修改选中动作", deps=context))
        self.assertEqual(context.checked_result["rows"], [["wait", 80]])
        self.assertIn("reading_context", phases)


if __name__ == "__main__":
    unittest.main()
