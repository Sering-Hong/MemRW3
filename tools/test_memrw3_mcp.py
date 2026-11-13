import importlib.util
import pathlib
import unittest
import json
import socket
import threading
import tempfile
import subprocess
import sys
from unittest import mock

spec = importlib.util.spec_from_file_location("memrw3_mcp", pathlib.Path(__file__).with_name("memrw3_mcp.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class ProtocolTests(unittest.TestCase):
    def test_handshake_and_readonly_tools(self):
        result = m.dispatch({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2024-11-05"}})
        self.assertEqual(result["result"]["protocolVersion"], "2024-11-05")
        tools = m.dispatch({"id":2, "method":"tools/list"})["result"]["tools"]
        self.assertEqual({t["name"] for t in tools}, {"list_sessions", "get_status", "list_variables", "read_variables", "get_history", "get_write_permissions", "write_variable", "get_write_result", "pid_begin", "pid_snapshot", "pid_apply", "pid_accept", "pid_rollback", "pid_end"})
        self.assertTrue(all(t["annotations"]["readOnlyHint"] for t in tools if t["name"] != "write_variable" and not t["name"].startswith("pid_")))
        self.assertFalse(next(t for t in tools if t["name"] == "write_variable")["annotations"]["readOnlyHint"])

    def test_notifications_have_no_response(self):
        self.assertIsNone(m.dispatch({"jsonrpc":"2.0", "method":"notifications/initialized"}))

    def test_unknown_write_tool_is_rejected(self):
        result = m.dispatch({"id":3,"method":"tools/call","params":{"name":"write_variable","arguments":{}}})
        self.assertTrue(result["result"]["isError"])

    def test_missing_session_and_bad_arguments_are_errors(self):
        for arguments in [{"session_id":"does-not-exist"}, {"session_id":[]}]:
            result = m.dispatch({"id":4,"method":"tools/call","params":{"name":"get_status","arguments":arguments}})
            self.assertTrue(result["result"]["isError"])

    def test_batch_and_invalid_requests_are_rejected(self):
        self.assertEqual(m.dispatch([])["error"]["code"], -32600)
        self.assertEqual(m.dispatch({"id":5,"method":"tools/call","params":[]})["error"]["code"], -32602)

class WriteToolTests(unittest.TestCase):
    def test_write_arguments_are_required_and_preserve_exact_strings(self):
        definition = next(t for t in m.TOOLS if t["name"] == "write_variable")
        self.assertFalse(definition["annotations"]["readOnlyHint"])
        self.assertTrue(definition["annotations"]["destructiveHint"])
        with self.assertRaises(ValueError):
            m.call_tool("write_variable", {"variable_id":0,"value":"2"})
        args = {"variable_id":0,"value":"18446744073709551615","expected_value":"0","request_key":"write-1"}
        with mock.patch.object(m,"select_session",return_value={}), mock.patch.object(m,"query_local",return_value={"state":"pending"}) as query:
            self.assertEqual(m.call_tool("write_variable",args)["state"],"pending")
            self.assertEqual(query.call_args.args[2]["value"],"18446744073709551615")

class TransportTests(unittest.TestCase):
    def test_authenticated_loopback_response_and_multiple_instances(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            server = socket.socket()
            server.bind(("127.0.0.1", 0))
            server.listen(1)
            server.settimeout(3)
            port = server.getsockname()[1]
            token = "a" * 64
            session = {"session_id":"test-one", "port":port, "token":token, "pid":1}
            (root / "session-test-one.json").write_text(json.dumps(session), encoding="utf-8")
            def reply():
                with server:
                    connection, _ = server.accept()
                    with connection, connection.makefile("rb") as reader:
                        request = json.loads(reader.readline())
                        self.assertEqual(request["token"], token)
                        self.assertEqual(request["method"], "get_status")
                        connection.sendall(b'{"ok":true,"data":{"connected":false,"readonly":true}}\n')
            thread = threading.Thread(target=reply)
            thread.start()
            with mock.patch.object(m, "session_dir", return_value=root):
                result = m.call_tool("get_status", {})
                self.assertTrue(result["readonly"])
                self.assertFalse(result["connected"])
                listed = m.call_tool("list_sessions", {})
                self.assertNotIn("token", json.dumps(listed))
                self.assertNotIn("port", json.dumps(listed))
                other = dict(session, session_id="test-two")
                (root / "session-test-two.json").write_text(json.dumps(other), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "session_id"):
                    m.call_tool("get_status", {})
            thread.join(3)
            self.assertFalse(thread.is_alive())

    def test_real_stdio_handshake_and_tool_listing(self):
        requests = [
            {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}},
            {"jsonrpc":"2.0","method":"notifications/initialized"},
            {"jsonrpc":"2.0","id":2,"method":"tools/list"},
            {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_variable","arguments":{}}},
        ]
        completed = subprocess.run([sys.executable, str(pathlib.Path(m.__file__))],
            input="".join(json.dumps(r)+"\n" for r in requests), text=True,
            encoding="utf-8", capture_output=True, timeout=5)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        replies = [json.loads(line) for line in completed.stdout.splitlines()]
        self.assertEqual(len(replies), 3)
        self.assertEqual(replies[0]["result"]["protocolVersion"], "2025-06-18")
        self.assertEqual(len(replies[1]["result"]["tools"]), 14)
        self.assertTrue(replies[2]["result"]["isError"])

if __name__ == "__main__":
    unittest.main()
