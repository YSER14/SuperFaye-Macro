import math
import re
from collections import deque
from pathlib import PureWindowsPath

from automation.compiler import parse_graph
from engine.input_sendinput import VK_MAP


class DraftError(ValueError):
    pass


MAX_MACRO_ACTIONS = 20_000
MAX_DURATION_MS = 3_600_000
MAX_GRAPH_NODES = 120
MAX_GRAPH_LINKS = 360
_ALLOWED_MOUSE_BUTTONS = {"left", "middle", "right"}


def _number(value, *, min_value=0, max_value=MAX_DURATION_MS, whole=False):
    return (
        type(value) in (int, float)
        and min_value <= value <= max_value
        and math.isfinite(value)
        and (not whole or int(value) == value)
    )


def _macro_reference(value):
    if not isinstance(value, str) or not value.strip():
        return False
    path = PureWindowsPath(value)
    return (
        not path.anchor
        and len(path.parts) >= 2
        and path.parts[0] == "宏"
        and ".." not in path.parts
        and not any(re.search(r'[<>:"|?*\x00-\x1f]', part) for part in path.parts)
        and path.suffix.lower() == ".json"
    )


def _macro_rows(rows, prefix="动作", depth=0):
    if not isinstance(rows, list) or len(rows) > 1200:
        raise DraftError(f"{prefix}应为动作数组，最多包含 1200 行")
    if depth > 8:
        raise DraftError("循环嵌套过深")
    actions = 0
    duration = 0
    for index, row in enumerate(rows, 1):
        label = f"{prefix} {index}"
        if not isinstance(row, list) or not row or not isinstance(row[0], str):
            raise DraftError(f"{label}格式错误")
        cmd = row[0]
        if cmd == "loop":
            if len(row) != 3 or not _number(row[1], min_value=1, max_value=1000, whole=True):
                raise DraftError(f"{label}的循环次数应为 1 到 1000 的整数")
            nested_actions, nested_duration = _macro_rows(row[2], label + " · 动作", depth + 1)
            if not row[2]:
                raise DraftError(f"{label}的循环体为空")
            actions += nested_actions * int(row[1])
            duration += nested_duration * int(row[1])
        elif cmd in {"import", "note"}:
            if len(row) != 2 or not isinstance(row[1], str) or len(row[1]) > 2000:
                raise DraftError(f"{label}需要一段文本")
            if cmd == "import" and not _macro_reference(row[1]):
                raise DraftError(f"{label}需要宏目录中的相对子宏路径，例如 宏/示例.json")
            if cmd == "note" and ("\r" in row[1] or "\n" in row[1] or "\x00" in row[1]):
                raise DraftError(f"{label}的注释应写在一行内")
        elif cmd == "wait":
            if len(row) != 2 or not _number(row[1]):
                raise DraftError(f"{label}的等待时长应为非负毫秒数")
            actions += 1
            duration += row[1]
        elif cmd == "view":
            if len(row) != 3 or not isinstance(row[1], list) or len(row[1]) != 2 or not (
                all(_number(delta, min_value=-100_000, max_value=100_000) for delta in row[1])
                and _number(row[2], min_value=1)
            ):
                raise DraftError(f"{label}需要 [dx, dy] 和正数毫秒时长")
            actions += 1
            duration += row[2]
        elif cmd in {"kd", "ku", "md", "mu"}:
            keys = VK_MAP if cmd.startswith("k") else _ALLOWED_MOUSE_BUTTONS
            if len(row) != 3 or not isinstance(row[1], str) or row[1].lower() not in keys:
                raise DraftError(f"{label}的按键名称不受支持")
            if not _number(row[2]):
                raise DraftError(f"{label}的动作后延迟应为非负毫秒数")
            actions += 1
            duration += row[2]
        else:
            raise DraftError(f"{label}使用了不支持的指令 {cmd[:40]}")
        if actions > MAX_MACRO_ACTIONS or duration > MAX_DURATION_MS:
            raise DraftError("展开后的动作数或时长过大，请拆成几段")
    return actions, duration


def _key_warnings(rows, partial):
    issues = []
    held = {}

    def walk(items, prefix, depth=0):
        if depth > 8:
            return
        for index, row in enumerate(items, 1):
            if not isinstance(row, list) or not row or not isinstance(row[0], str):
                continue
            cmd = row[0]
            label = f"{prefix} {index}"
            if cmd == "loop" and len(row) == 3 and isinstance(row[2], list):
                before = held.copy()
                walk(row[2], label + " · 动作", depth + 1)
                if held.keys() != before.keys() and len(issues) < 8:
                    issues.append(f"{label}循环结束时，有按键状态与入口不同")
            if cmd == "import" and len(issues) < 8:
                issues.append(f"{label}调用子宏，子宏时长及按键状态需单独检查")
            if cmd in {"kd", "ku", "md", "mu"} and len(row) == 3 and isinstance(row[1], str):
                key = ("键盘" if cmd.startswith("k") else "鼠标", row[1].lower())
                if cmd.endswith("d"):
                    if key in held and len(issues) < 8:
                        issues.append(f"{label}重复按下 {key[1]}")
                    held[key] = label
                elif held.pop(key, None) is None and not partial and len(issues) < 8:
                    issues.append(f"{label}松开 {key[1]} 前未检测到按下")

    walk(rows, "动作")
    if held and not partial and len(issues) < 8:
        issues.append("结束时仍按住 " + "、".join(key for _, key in list(held)[:10]))
    return issues


def validate_macro(rows, partial=False):
    actions, duration = _macro_rows(rows)
    return {"rows": rows, "stats": {"actions": actions, "duration_ms": round(duration, 3)},
            "warnings": _key_warnings(rows, partial)}


def _node_id(value):
    if type(value) is int and 1 <= value <= 2_147_483_647:
        return str(value)
    if isinstance(value, str) and re.fullmatch(r"[1-9][0-9]{0,9}", value) and int(value) <= 2_147_483_647:
        return value
    raise DraftError("节点 ID 应为正整数")


def _matches_param(value, spec):
    choices = spec.get("enum", spec.get("options"))
    if choices and value not in choices:
        return False
    kind = spec.get("type")
    reference = isinstance(value, str) and re.fullmatch(r"\$\{[^{}]{1,128}\}|@[\w]+\.[\w]+", value)
    if kind == "number":
        return bool(reference) or _number(value, min_value=spec.get("min", -1e12),
                                          max_value=spec.get("max", 1e12))
    if kind in {"boolean", "bool"}:
        return bool(reference) or type(value) is bool
    if kind == "array":
        if not isinstance(value, list):
            return False
        if spec.get("format") == "region":
            return len(value) == 4 and all(_number(n, min_value=-100_000, max_value=100_000) for n in value)
        return True
    return kind not in {"string", "enum"} or isinstance(value, str)


def _graph_nodes(items, definitions, original):
    nodes = {}
    for ix, raw in enumerate(items):
        if not isinstance(raw, dict):
            raise DraftError("节点必须为 JSON 对象")
        node_id = _node_id(raw.get("id"))
        node_type = raw.get("type")
        if node_id in nodes:
            raise DraftError(f"节点 ID {node_id} 重复")
        if not isinstance(node_type, str) or node_type not in definitions:
            raise DraftError(f"节点 {node_id} 使用了未注册的类型")
        definition = definitions[node_type]
        prior = original.get(node_id, {})
        params = raw.get("params", {})
        if not isinstance(params, dict):
            raise DraftError(f"节点 {node_id} 参数应为 JSON 对象")
        if prior.get("type") == node_type:
            params = {**prior.get("params", {}), **params}
        for name, value in params.items():
            spec = definition["params_schema"].get(name)
            if spec is None or not _matches_param(value, spec):
                if prior.get("type") != node_type or name not in prior.get("params", {}) or prior["params"][name] != value:
                    raise DraftError(f"节点 {node_id}（{node_type}）的参数 {name[:80]} 无效")

        if node_type == "action/key_action" and str(params.get("key", "")).lower() not in VK_MAP:
            raise DraftError(f"节点 {node_id} 需要有效的按键名称")
        if node_type in {"control/wait_cond", "control/loop_count", "control/loop_while"}:
            name, limit = {"control/wait_cond": ("timeout", MAX_DURATION_MS),
                           "control/loop_count": ("count", 20_000),
                           "control/loop_while": ("max_iterations", 20_000)}[node_type]
            value = params.get(name, definition["params_schema"][name]["default"])
            if not _number(value, min_value=1, max_value=limit, whole=True):
                raise DraftError(f"节点 {node_id} 的 {name} 需要有界的正整数")

        position = raw.get("position", prior.get("position", {"x": 80 + 300 * (ix % 4), "y": 100 + 260 * (ix // 4)}))
        if not isinstance(position, dict) or not all(
            _number(position.get(axis), max_value=100_000) for axis in ("x", "y")
        ):
            raise DraftError(f"节点 {node_id} 的画布坐标无效")
        node = {"id": node_id, "type": node_type, "position": position, "params": params}
        custom_name = raw.get("customName", prior.get("customName", ""))
        if not isinstance(custom_name, str) or len(custom_name) > 100:
            raise DraftError(f"节点 {node_id} 的自定义名称无效")
        if custom_name:
            node["customName"] = custom_name
        nodes[node_id] = node
    return nodes


def _port(endpoint, nodes, definitions, direction):
    if not isinstance(endpoint, dict) or not isinstance(endpoint.get("port"), str):
        raise DraftError("连线缺少节点或端口")
    node_id = _node_id(endpoint.get("node"))
    if node_id not in nodes:
        raise DraftError(f"连线引用了不存在的节点 {node_id}")
    name = endpoint["port"]
    for port in definitions[nodes[node_id]["type"]][direction]:
        if port["name"] == name:
            return (node_id, name), port["type"]
    raise DraftError(f"节点 {node_id} 上不存在端口 {name[:80]}")


def _graph_connections(items, nodes, definitions):
    connections = []
    occupied = set()
    for raw in items:
        if not isinstance(raw, dict):
            raise DraftError("连线必须为 JSON 对象")
        source, out_type = _port(raw.get("from"), nodes, definitions, "outputs")
        target, in_type = _port(raw.get("to"), nodes, definitions, "inputs")
        if (out_type == "flow") != (in_type == "flow") or (
            out_type != in_type and "any" not in (out_type, in_type)
        ):
            raise DraftError(f"节点 {source[0]} 和 {target[0]} 的端口类型不匹配")
        if source[0] == target[0] or target in occupied:
            raise DraftError(f"节点 {target[0]} 的 {target[1]} 端口只能有一条输入连线，且不能自连")
        occupied.add(target)
        link_type = "flow" if out_type == "flow" else "data"
        if raw.get("type", link_type) != link_type:
            raise DraftError(f"节点 {source[0]} 和 {target[0]} 的连线类型应为 {link_type}")
        connections.append({"from": {"node": source[0], "port": source[1]},
                            "to": {"node": target[0], "port": target[1]}, "type": link_type})
    return connections


def _check_cycle(edges, error):
    degrees = dict.fromkeys(edges, 0)
    for children in edges.values():
        for child in children:
            degrees[child] += 1
    queue = deque(key for key, degree in degrees.items() if degree == 0)
    seen = 0
    while queue:
        seen += 1
        for child in edges[queue.popleft()]:
            degrees[child] -= 1
            if degrees[child] == 0:
                queue.append(child)
    if seen != len(edges):
        raise DraftError(error)


def _check_flow(graph):
    flow = {key: [child for children in node.flow_children.values() for child in children]
            for key, node in graph.nodes.items()}
    dependencies = {key: [source["source_node_id"] for sources in node.data_inputs.values() for source in sources]
                    for key, node in graph.nodes.items()}
    for node_id, node in graph.nodes.items():
        if node.type == "control/goto":
            flow[node_id].append(node.params["target_node_id"])
        if node.type == "control/wait_cond":
            sources = node.data_inputs.get("cond")
            if not sources or not node.flow_children.get("timeout"):
                raise DraftError(f"等待条件节点 {node_id} 需要连接条件输入与超时出口")
            if any(graph.nodes[source["source_node_id"]].type.startswith("detection/") for source in sources):
                graph.warnings.append(f"等待条件节点 {node_id} 不会重新运行普通检测节点。轮询检测请使用监视节点或有限循环。")
    _check_cycle(flow, "流程连线形成环，请使用有次数上限的循环节点")
    _check_cycle(dependencies, "数据连线形成循环依赖")


def validate_graph(draft, definitions, source=None):
    raw = draft.get("graph") if isinstance(draft, dict) else None
    if not isinstance(raw, dict) or not isinstance(raw.get("nodes"), list) or not isinstance(raw.get("connections"), list):
        raise DraftError("脚本需要 graph.nodes 和 graph.connections 两个数组")
    if not 1 <= len(raw["nodes"]) <= MAX_GRAPH_NODES or len(raw["connections"]) > MAX_GRAPH_LINKS:
        raise DraftError("节点数量或连线数量超出单个草稿的范围")
    source = source or {}
    source_graph = source.get("graph", {})
    original = {str(node["id"]): node for node in source_graph.get("nodes", [])}
    nodes = _graph_nodes(raw["nodes"], definitions, original)
    connections = _graph_connections(raw["connections"], nodes, definitions)
    document = {
        "variables": source.get("variables", {}),
        "settings": source.get("settings", {}),
        "graph": {
            "nodes": list(nodes.values()), "connections": connections,
            "annotations": source_graph.get("annotations", []),
            "groups": source_graph.get("groups", []),
        },
    }
    if sum(node["type"] == "flow/start" for node in nodes.values()) != 1:
        raise DraftError("脚本需要且只能有一个开始节点")
    try:
        compiled = parse_graph(document)
    except (TypeError, ValueError, KeyError) as exc:
        raise DraftError("图参数无法编译，请检查节点引用") from exc
    if compiled.errors:
        raise DraftError("；".join(compiled.errors[:4]))
    _check_flow(compiled)
    stats = {"nodes": len(nodes), "connections": len(connections)}
    node_names = {node["type"]: definitions[node["type"]]["display_name"] for node in nodes.values()}
    return {"document": document, "stats": stats, "node_names": node_names, "warnings": compiled.warnings[:12]}
