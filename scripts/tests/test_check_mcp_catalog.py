"""Hermetic tests for the credential-free MCP catalog checker.

All HTTP traffic stays on 127.0.0.1; no real endpoint is contacted and no
credentials are involved. The workflow tests only read YAML as text.
"""

from contextlib import contextmanager
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check_mcp_catalog.py"
WORKFLOW = ROOT / ".github" / "workflows" / "mcp-catalog-check.yml"
CI = ROOT / ".github" / "workflows" / "ci.yml"


def load_module():
    spec = importlib.util.spec_from_file_location("check_mcp_catalog", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


checker = load_module()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, code, body, content_type="application/json", headers=None):
        data = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        base = f"http://127.0.0.1:{self.server.server_port}"
        if self.path == "/open":
            self.reply(200, json.dumps({"jsonrpc": "2.0", "id": 1, "result": {}}))
        elif self.path == "/sse":
            self.reply(200, "event: message\ndata: " + json.dumps({"jsonrpc": "2.0", "id": 1, "result": {}}) + "\n\n", "text/event-stream")
        elif self.path == "/api-key":
            self.reply(401, "no auth header", "text/plain")
        elif self.path == "/oauth-dcr":
            self.reply(401, "unauthorized", "text/plain", {"WWW-Authenticate": f'Bearer resource_metadata="{base}/prm-dcr"'})
        elif self.path == "/prm-dcr":
            self.reply(200, json.dumps({"authorization_servers": [f"{base}/as-dcr"]}))
        elif self.path == "/as-dcr/.well-known/oauth-authorization-server":
            self.reply(200, json.dumps({"registration_endpoint": f"{base}/register"}))
        elif self.path == "/oauth-no-dcr":
            self.reply(401, "unauthorized", "text/plain", {"WWW-Authenticate": f'Bearer resource_metadata="{base}/prm"'})
        elif self.path == "/prm":
            self.reply(200, json.dumps({"authorization_servers": [f"{base}/as"]}))
        elif self.path == "/as/.well-known/oauth-authorization-server":
            self.reply(200, json.dumps({"issuer": f"{base}/as"}))
        elif self.path == "/missing":
            self.reply(404, "not found", "text/plain")
        elif self.path == "/slow":
            time.sleep(1)
            self.reply(200, "{}")
        elif self.path == "/badjson":
            self.reply(200, "not json at all", "text/plain")
        elif self.path == "/unexpected":
            self.reply(500, "oops", "text/plain")
        else:
            self.reply(404, "not found", "text/plain")

    def do_GET(self):
        self.do_POST()


@contextmanager
def server():
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_port}"
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=2)


class EndpointTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._ctx = server()
        cls.base = cls._ctx.__enter__()

    @classmethod
    def tearDownClass(cls):
        cls._ctx.__exit__(None, None, None)

    def check(self, path, expects_api_key=False, timeout=0.5):
        return checker.check_endpoint(self.base + path, timeout, expects_api_key)

    def test_open_and_sse(self):
        self.assertEqual(self.check("/open")["status"], "open")
        self.assertEqual(self.check("/sse")["status"], "open")

    def test_api_key_is_ok_only_when_declared(self):
        declared = self.check("/api-key", expects_api_key=True)
        self.assertEqual(declared["status"], "api-key")
        self.assertNotIn(declared["status"], checker.ATTENTION)
        self.assertEqual(self.check("/api-key")["status"], "oauth-metadata-missing")

    def test_oauth_dcr_and_no_dcr(self):
        dcr = self.check("/oauth-dcr")
        self.assertEqual(dcr["status"], "oauth-dcr")
        self.assertTrue(dcr["dcr"])
        no_dcr = self.check("/oauth-no-dcr")
        self.assertEqual(no_dcr["status"], "oauth-no-dcr")
        self.assertFalse(no_dcr["dcr"])
        self.assertIn(no_dcr["status"], checker.ATTENTION)

    def test_failures_are_attention_not_exceptions(self):
        self.assertEqual(self.check("/missing")["status"], "endpoint-missing")
        self.assertEqual(self.check("/badjson")["status"], "bad-response")
        self.assertEqual(self.check("/unexpected")["status"], "unexpected-status")
        self.assertEqual(self.check("/slow", timeout=0.1)["status"], "unreachable")

    def test_no_authorization_header_is_sent(self):
        # The handler answers /open regardless, so assert the request headers directly.
        seen = {}

        class Spy(Handler):
            def do_POST(self):
                seen["authorization"] = self.headers.get("Authorization")
                super().do_POST()

        httpd = ThreadingHTTPServer(("127.0.0.1", 0), Spy)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            checker.check_endpoint(f"http://127.0.0.1:{httpd.server_port}/open", 0.5)
            self.assertIsNone(seen["authorization"])
        finally:
            httpd.shutdown()
            httpd.server_close()
            thread.join(timeout=2)


class ReportTests(unittest.TestCase):
    def fixture(self, base):
        return [
            {"id": "open", "install": {"transport": "http", "url": base + "/open", "required_secrets": []}},
            {"id": "local", "install": {"transport": "stdio", "command": "npx", "required_secrets": []}},
            {"id": "nodcr", "install": {"transport": "http", "url": base + "/oauth-no-dcr", "required_secrets": []}},
        ]

    def test_report_schema_and_exit_codes(self):
        with server() as base, TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "catalog.json"
            catalog.write_text(json.dumps(self.fixture(base)), encoding="utf-8")
            out = Path(tmp) / "report.json"
            report = checker.run_check(catalog, 0.5)
            self.assertEqual(report["structural_errors"], [])
            self.assertEqual(len(report["entries"]), 3)
            by_id = {item["id"]: item for item in report["entries"]}
            self.assertEqual(by_id["open"]["status"], "open")
            self.assertEqual(by_id["local"]["status"], "skipped-stdio")
            self.assertEqual(by_id["nodcr"]["dcr"], False)
            self.assertEqual([item["id"] for item in report["attention"]], ["nodcr"])
            datetime.strptime(report["checked_at"], "%Y-%m-%dT%H:%M:%SZ")
            code = checker.main(["--catalog", str(catalog), "--timeout", "0.5", "--json", str(out)])
            self.assertEqual(code, 0)
            written = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual(written["attention"], report["attention"])
            self.assertEqual(checker.main(["--catalog", str(catalog), "--timeout", "0.5", "--strict"]), 1)

    def test_only_and_structural_errors(self):
        with server() as base, TemporaryDirectory() as tmp:
            catalog = Path(tmp) / "catalog.json"
            entries = self.fixture(base) + [
                {"id": "open", "install": {"transport": "http", "url": base + "/open", "required_secrets": []}},
                {"id": "bad-secret", "install": {"transport": "http", "url": base + "/open", "required_secrets": [{"name": "token"}], "secret_headers": {"X": "{secret:mcp/missing}"}}},
                {"id": "plain-http", "install": {"transport": "http", "url": "http://example.com/mcp", "required_secrets": []}},
                {"id": "token", "install": {"transport": "stdio", "command": "npx sk-abcdefghijklmnopqrstuvwxyz012345", "required_secrets": []}},
            ]
            catalog.write_text(json.dumps(entries), encoding="utf-8")
            report = checker.run_check(catalog, 0.5)
            joined = " | ".join(report["structural_errors"])
            for needle in ("duplicate id", "must start with 'mcp/'", "undeclared secret", "https url", "token-looking"):
                self.assertIn(needle, joined)
            self.assertEqual(checker.main(["--catalog", str(catalog), "--timeout", "0.5"]), 2)
            one = checker.run_check(catalog, 0.5, only="open")
            self.assertEqual({item["id"] for item in one["entries"]}, {"open"})


class WorkflowTests(unittest.TestCase):
    def test_scheduled_workflow_is_safe(self):
        text = WORKFLOW.read_text(encoding="utf-8")
        self.assertNotIn("pull_request:", text)
        self.assertIn("workflow_dispatch:", text)
        self.assertIn("cron:", text)
        self.assertIn("github.repository == 'emreocell/Praxi-Bot'", text)
        self.assertIn("permissions: {}", text)
        self.assertIn("issues: write", text)
        self.assertNotIn("secrets:", text)
        uses = [line.strip() for line in text.splitlines() if line.strip().startswith("uses:")]
        self.assertTrue(uses)
        for line in uses:
            self.assertRegex(line, r"@[0-9a-f]{40}(?: #.*)?$", line)
        self.assertIn("issue", text.lower())  # create/update/close a tracking issue

    def test_offline_unit_test_runs_in_ci(self):
        ci = CI.read_text(encoding="utf-8")
        self.assertIn("python3 -B -m unittest discover -s scripts/tests -p test_check_mcp_catalog.py -v", ci)


if __name__ == "__main__":
    unittest.main()
