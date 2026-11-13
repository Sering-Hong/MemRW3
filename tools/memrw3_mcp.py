"""MemRW3 MCP bridge with default read/write access to loaded numeric variables; no direct probe access."""
import argparse
import json
import os
import pathlib
import socket
import sys

PROTOCOLS = {"2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"}
MAX_REPLY = 8 * 1024 * 1024


def session_dir():
    base = os.environ.get("LOCALAPPDATA") if os.name == "nt" else os.environ.get("XDG_RUNTIME_DIR", os.path.expanduser("~/.local/state"))
    if not base:
        raise ValueError("LOCALAPPDATA is unavailable")
    return pathlib.Path(base) / "MemRW3" / "ai-readonly"


def sessions():
    result = []
    directory = session_dir()
    if not directory.exists():
        return result
    for path in directory.glob("session-*.json"):
        try:
            if path.stat().st_size > 4096:
                continue
            item = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(item, dict) or not isinstance(item.get("port"), int) or not 0 < item["port"] < 65536:
                continue
            if not isinstance(item.get("token"), str) or len(item["token"]) != 64:
                continue
            if item.get("session_id") != path.stem.removeprefix("session-"):
                continue
            result.append(item)
        except (OSError, ValueError, TypeError):
            continue
    return result


def select_session(session_id):
    if session_id is not None and not isinstance(session_id, str):
        raise ValueError("session_id must be a string")
    available = sessions()
    if session_id is not None:
        available = [s for s in available if s["session_id"] == session_id]
    if not available:
        raise ValueError("未找到运行中的 MemRW3 接口；请打开默认读写版程序。")
    if len(available) != 1:
        raise ValueError("发现多个 MemRW3 会话，请先 list_sessions，再指定 session_id。")
    return available[0]


def query_local(session, method, arguments):
    request = {"token": session["token"], "method": method, "arguments": arguments}
    payload = json.dumps(request, ensure_ascii=False, allow_nan=False).encode("utf-8") + b"\n"
    if len(payload) > 65536:
        raise ValueError("request too large")
    try:
        with socket.create_connection(("127.0.0.1", session["port"]), timeout=4) as connection:
            connection.settimeout(5)
            connection.sendall(payload)
            with connection.makefile("rb") as reader:
                raw = reader.readline(MAX_REPLY + 1)
        if not raw.endswith(b"\n") or len(raw) > MAX_REPLY:
            raise ValueError("invalid or oversized MemRW3 response")
        response = json.loads(raw)
        if not response.get("ok"):
            raise ValueError(response.get("error", "MemRW3 query failed"))
        return response["data"]
    except (OSError, ValueError, KeyError) as error:
        # Never include the token or transport request in diagnostics.
        raise ValueError(f"MemRW3 读取失败：{error}；请确认新版软件仍运行且窗口能够响应。") from None


def tool(name, description, properties=None, required=None):
    props = {"session_id": {"type": "string", "description": "多个实例时必须指定，由 list_sessions 获取"}}
    props.update(properties or {})
    return {"name": name, "description": description,
            "inputSchema": {"type": "object", "properties": props, "required": required or [], "additionalProperties": False},
            "annotations": {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True, "openWorldHint": False}}


TOOLS = [
    {**tool("list_sessions", "列出已启用AI接口的软件实例；不会连接探针。"),
     "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    tool("get_status", "读取软件连接、采集、暂停、固件和接口状态；不改变软件状态。"),
    tool("list_variables", "列出已加载变量的 ID、名称、中文注释、地址及类型；先搜索再按 ID 读取。", {
        "query": {"type": "string"}, "offset": {"type": "integer", "minimum": 0},
        "limit": {"type": "integer", "minimum": 1, "maximum": 200}}),
    tool("read_variables", "批量读取软件已采集的最新值及原始字节；检查 stale 和时间字段，暂停值不能当实时值。", {
        "ids": {"type": "array", "items": {"type": "integer", "minimum": 0}, "minItems": 1, "maxItems": 128}}, ["ids"]),
    tool("get_history", "读取指定变量 Chart 中保留的最近原始历史，不使用显示压缩，不新增采样。时间单位秒，最多10000点。", {
        "variable_id": {"type": "integer", "minimum": 0},
        "seconds": {"type": "number", "exclusiveMinimum": 0, "maximum": 86400},
        "limit": {"type": "integer", "minimum": 1, "maximum": 10000}}, ["variable_id"]),
]

TOOLS.extend([
    tool("get_write_permissions", "查询全部已加载的可写数值变量及类型界限；无需勾选或启用。"),
    tool("get_write_result", "查询请求的最终执行/读回结果。pending不是成功，uncertain不允许自动重试。", {
        "request_key": {"type":"string","minLength":1,"maxLength":128}}, ["request_key"]),
    tool("write_variable", "按用户明确指令修改任一已加载数值参数，无需人工授权设置。先读取变量和当前值，提交唯一request_key及预期旧值；返回排队状态后必须查询结果。不要自动重试不确定写入。", {
        "variable_id": {"type":"integer","minimum":0},
        "value": {"type":"string","description":"目标值，使用十进制字符串，保留整数精度"},
        "expected_value": {"type":"string","description":"预期旧值，软件将先从目标读取并比较"},
        "permission_epoch": {"type":"integer","minimum":0,"description":"可选授权代次；PID会话必须提供，原生端拒绝旧代次"},
        "request_key": {"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9_.-]+$"}},
        ["variable_id","value","expected_value","request_key"]),
])
for definition in TOOLS:
    if definition["name"] == "write_variable":
        definition["annotations"] = {"readOnlyHint":False,"destructiveHint":True,"idempotentHint":False,"openWorldHint":False}



TOOLS.extend([
    tool("pid_begin", "建立通用单组PID调试会话并保存原始基线；不写MCU。三个增益默认可写，max_step属于本轮调参策略。", {
        "bindings":{"type":"object","properties":{role:{"type":"string"} for role in ("kp","ki","kd","target","feedback","output")},
                    "required":["kp","ki","kd","target","feedback","output"],"additionalProperties":False},
        "max_step":{"type":"object","properties":{role:{"type":"string"} for role in ("kp","ki","kd")},"required":["kp","ki","kd"],"additionalProperties":False},
        "guards":{"type":"array","maxItems":16,"items":{"type":"object","properties":{"name":{"type":"string"},"op":{"type":"string","enum":["eq","le","ge"]},"value":{"type":"string"}},"required":["name","op","value"],"additionalProperties":False}},
        "max_age_ms":{"type":"integer","minimum":20,"maximum":1000,"description":"主机观察年龄上限，默认200ms，不代表精确硬件采样年龄"},
        "observe_seconds":{"type":"number","minimum":0.2,"maximum":60},
        "min_interval_seconds":{"type":"number","minimum":0.2,"maximum":60}}, ["bindings","max_step"]),
    tool("pid_snapshot", "读取PID会话的三个增益、目标/反馈/输出及试验状态；过期或安全条件失败时拒绝。", {"tuning_id":{"type":"string"}}, ["tuning_id"]),
    tool("pid_apply", "按明确调参指令每轮只改一个PID增益；基线先落盘，限制单步改变量，复用受控写入。", {
        "tuning_id":{"type":"string"},"gain":{"type":"string","enum":["kp","ki","kd"]},"value":{"type":"string"},"request_key":{"type":"string"}},
        ["tuning_id","gain","value","request_key"]),
    tool("pid_accept", "观察并评价后把本轮PID设为新基线。必须说明依据，不等于软件认证稳定性。", {
        "tuning_id":{"type":"string"},"reason":{"type":"string","minLength":8,"maxLength":2000}}, ["tuning_id","reason"]),
    tool("pid_rollback", "把本轮已确认写入的单个增益恢复到上一基线；先检查三项当前值，拒绝覆盖外部修改。未知或失联时不盲写。", {
        "tuning_id":{"type":"string"},"request_key":{"type":"string"}}, ["tuning_id","request_key"]),
    tool("pid_end", "关闭已解决的PID试验会话；不停止电机、不复位MCU、不改变已接受参数。", {"tuning_id":{"type":"string"}}, ["tuning_id"]),
])
for definition in TOOLS:
    if definition["name"].startswith("pid_"):
        definition["annotations"]["readOnlyHint"] = False
        definition["annotations"]["destructiveHint"] = definition["name"] in ("pid_apply","pid_rollback")
        definition["annotations"]["idempotentHint"] = False


def call_tool(name, arguments):
    definition = next((t for t in TOOLS if t["name"] == name), None)
    if definition is None:
        raise ValueError("Unknown MemRW3 tool")
    if not isinstance(arguments, dict):
        raise ValueError("arguments must be an object")
    schema = definition["inputSchema"]
    if set(arguments) - set(schema["properties"]):
        raise ValueError("unknown arguments")
    if any(key not in arguments for key in schema.get("required", [])):
        raise ValueError("required arguments missing")
    if name.startswith("pid_"):
        import pid_online
        return pid_online.call(name, arguments, call_tool)
    if name == "list_sessions":
        return {"sessions": [{k: s[k] for k in ("session_id", "pid", "started_at_ms", "server_version", "supports_guarded_write") if k in s} for s in sessions()],
                "note": "退出异常时可能留下失效记录；get_status 会检查该会话是否响应。"}
    session = select_session(arguments.get("session_id"))
    return query_local(session, name, {k: v for k, v in arguments.items() if k != "session_id"})


def dispatch(request):
    if not isinstance(request, dict) or not isinstance(request.get("method"), str):
        return {"jsonrpc": "2.0", "id": request.get("id") if isinstance(request, dict) else None,
                "error": {"code": -32600, "message": "Invalid Request"}}
    if "id" not in request:
        return None
    reply = {"jsonrpc": "2.0", "id": request["id"]}
    method = request["method"]
    params = request.get("params", {})
    if not isinstance(params, dict):
        reply["error"] = {"code": -32602, "message": "params must be an object"}
        return reply
    if method == "initialize":
        version = params.get("protocolVersion")
        reply["result"] = {"protocolVersion": version if isinstance(version, str) and version in PROTOCOLS else "2025-11-25",
                           "capabilities": {"tools": {}}, "serverInfo": {"name": "memrw3", "version": "2.2.0"},
                           "instructions": "MemRW3 starts with read/write access to all loaded numeric variables; no GUI enable switch, allowlist or manual range authorization is needed. Write only for a user-requested parameter change after checking current variable identity and values. Supply an expected old value and unique request_key; poll get_write_result. Never retry uncertain writes automatically. For PID trials use pid_begin/snapshot/apply/accept/rollback/end. Change one gain per trial, respect max_step and observation time. A checkpoint is not proof of stability; never guess safety thresholds. No probe connection, halt, reset or flash tools."}
    elif method == "ping":
        reply["result"] = {}
    elif method == "tools/list":
        reply["result"] = {"tools": TOOLS}
    elif method == "tools/call":
        try:
            result = call_tool(params.get("name"), params.get("arguments", {}))
            reply["result"] = {"content": [{"type": "text", "text": json.dumps(result, ensure_ascii=False, allow_nan=False)}], "isError": False}
        except (ValueError, TypeError, OSError) as error:
            reply["result"] = {"content": [{"type": "text", "text": str(error)}], "isError": True}
    else:
        reply["error"] = {"code": -32601, "message": "Method not found"}
    return reply


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--call", choices=[t["name"] for t in TOOLS], help="Direct query for Codex shell fallback")
    parser.add_argument("--arguments", default="{}", help="JSON tool arguments")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    if args.call:
        try:
            print(json.dumps(call_tool(args.call, json.loads(args.arguments)), ensure_ascii=False, allow_nan=False))
            return 0
        except (ValueError, OSError, TypeError) as error:
            print(str(error), file=sys.stderr)
            return 1
    while True:
        raw = sys.stdin.buffer.readline(65537)
        if not raw:
            break
        try:
            if len(raw) > 65536 or not raw.endswith(b"\n"):
                return 2
            response = dispatch(json.loads(raw))
        except (ValueError, TypeError):
            response = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}}
        if response is not None:
            print(json.dumps(response, ensure_ascii=False, allow_nan=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
