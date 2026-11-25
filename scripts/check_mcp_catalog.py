#!/usr/bin/env python3
"""Credential-free verification of the HTTP entries in the MCP catalog.

This is a *reporting* tool, not a gate on unrelated work: a vendor outage, a
changed endpoint or missing OAuth metadata becomes an "attention" line in the
report, not a failed build. Structural errors in catalog.json do fail (exit 2).

No credentials are sent, no response bodies are cached, and at most a handful
of short metadata requests are made per entry. Run it by hand with:

    python3 scripts/check_mcp_catalog.py --timeout 5

See docs/contributing/MAINTAINING.md for how to read the report.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import socket
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "praxi-catalog-check/1 (+https://github.com/emreocell/Praxi-Bot)"
INITIALIZE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "praxi-catalog-check", "version": "1"},
    },
}
INIT_HEADERS = {
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
    "User-Agent": USER_AGENT,
}
MAX_METADATA_FETCHES = 4
TOKEN_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{24,}\b"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
)
SECRET_REF = re.compile(r"\{secret:([^}]+)\}")
ATTENTION = {
    "oauth-no-dcr",
    "oauth-metadata-missing",
    "endpoint-missing",
    "unreachable",
    "tls-error",
    "unexpected-status",
    "bad-response",
}


class FetchResult:
    __slots__ = ("status", "headers", "text", "error")

    def __init__(self, status, headers, text, error):
        self.status = status
        self.headers = headers
        self.text = text
        self.error = error


def fetch(url: str, timeout: float, body: bytes | None = None, headers: dict | None = None) -> FetchResult:
    """One bounded request. Never adds an Authorization header."""
    req = urllib.request.Request(url, data=body, headers=headers or {"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            return FetchResult(response.status, dict(response.headers.items()), response.read(65536).decode("utf-8", "replace"), None)
    except urllib.error.HTTPError as exc:
        body = exc.read(65536).decode("utf-8", "replace")
        headers = dict(exc.headers.items())
        exc.close()
        return FetchResult(exc.code, headers, body, None)
    except ssl.SSLError as exc:
        return FetchResult(0, {}, "", f"tls-error: {exc.reason}")
    except (socket.timeout, TimeoutError) as exc:
        return FetchResult(0, {}, "", f"timeout: {exc}")
    except urllib.error.URLError as exc:
        return FetchResult(0, {}, "", f"unreachable: {exc.reason}")
    except OSError as exc:
        return FetchResult(0, {}, "", f"unreachable: {exc}")


def parse_body(result: FetchResult) -> dict | None:
    """Accept a JSON body or the first data: line of an SSE stream."""
    content_type = result.headers.get("Content-Type", "")
    text = result.text.strip()
    if "text/event-stream" in content_type:
        for line in text.splitlines():
            if line.startswith("data:"):
                text = line[5:].strip()
                break
    try:
        parsed = json.loads(text)
    except (ValueError, TypeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def well_known_candidates(base: str, well_known: str) -> list[str]:
    """RFC 9728 / OIDC discovery paths, including the path-aware variant."""
    parts = urllib.parse.urlsplit(base)
    origin = f"{parts.scheme}://{parts.netloc}"
    path = parts.path.rstrip("/")
    candidates = [f"{origin}{path}/.well-known/{well_known}"] if path else []
    candidates.append(f"{origin}/.well-known/{well_known}")
    if path:
        # Some servers use /.well-known/<name>/<path> instead of <path>/.well-known/<name>.
        candidates.append(f"{origin}/.well-known/{well_known}{path}")
    seen: set[str] = set()
    return [url for url in candidates if not (url in seen or seen.add(url))]


def resource_metadata_url(www_authenticate: str) -> str | None:
    match = re.search(r'resource_metadata\s*=\s*"([^"]+)"', www_authenticate, re.IGNORECASE)
    return match.group(1) if match else None


def discover_auth(url: str, challenge: str, timeout: float) -> dict:
    """Try to discover protected-resource + authorization-server metadata."""
    detail: dict = {"dcr": None}
    fetched = 0
    candidates = []
    advertised = resource_metadata_url(challenge)
    if advertised:
        candidates.append(advertised)
    candidates.extend(well_known_candidates(url, "oauth-protected-resource"))
    protected = None
    for candidate in candidates:
        if fetched >= MAX_METADATA_FETCHES:
            break
        fetched += 1
        response = fetch(candidate, timeout)
        if response.status == 200:
            protected = parse_body(response)
            if protected:
                break
    if not protected:
        detail["auth"] = "oauth-metadata-missing"
        detail["note"] = "no protected-resource metadata found"
        return detail
    servers = protected.get("authorization_servers") or []
    if not servers or not isinstance(servers[0], str):
        detail["auth"] = "oauth-metadata-missing"
        detail["note"] = "protected-resource metadata has no authorization_servers"
        return detail
    issuer = servers[0]
    detail["issuer"] = issuer
    as_candidates = []
    for well_known in ("oauth-authorization-server", "openid-configuration"):
        as_candidates.extend(well_known_candidates(issuer, well_known))
    for candidate in as_candidates:
        if fetched >= MAX_METADATA_FETCHES:
            break
        fetched += 1
        response = fetch(candidate, timeout)
        if response.status == 200:
            metadata = parse_body(response)
            if not metadata:
                continue
            detail["dcr"] = bool(metadata.get("registration_endpoint"))
            detail["auth"] = "oauth-dcr" if detail["dcr"] else "oauth-no-dcr"
            detail["note"] = (
                "dynamic client registration advertised"
                if detail["dcr"]
                else "no registration_endpoint: how does a user sign in?"
            )
            return detail
    detail["auth"] = "oauth-metadata-missing"
    detail["note"] = "authorization-server metadata not discoverable"
    return detail


def check_endpoint(url: str, timeout: float, expects_api_key: bool = False) -> dict:
    """POST an unauthenticated MCP initialize and classify the response."""
    result = {"url": url, "status": "error", "auth": "none", "dcr": None, "note": ""}
    body = json.dumps(INITIALIZE).encode("utf-8")
    response = fetch(url, timeout, body=body, headers=INIT_HEADERS)
    if response.error:
        result["status"] = "tls-error" if response.error.startswith("tls-error") else "unreachable"
        result["note"] = response.error
        return result
    code = response.status
    if 200 <= code < 300:
        parsed = parse_body(response)
        result["status"] = "open" if parsed else "bad-response"
        result["note"] = "answers without credentials" if parsed else "2xx without JSON/SSE body"
        return result
    if code in (401, 403):
        result["status"] = "auth-required"
        challenge = response.headers.get("WWW-Authenticate", "")
        discovered = discover_auth(url, challenge, timeout)
        if discovered.get("auth") == "oauth-metadata-missing" and not challenge and expects_api_key:
            result["status"] = "api-key"
            result["note"] = "401 without OAuth metadata; API key declared in the catalog"
        else:
            result["status"] = discovered["auth"]
            result.update(discovered)
        return result
    if code in (404, 405):
        result["status"] = "endpoint-missing"
        result["note"] = f"HTTP {code}"
        return result
    result["status"] = "unexpected-status"
    result["note"] = f"HTTP {code}"
    return result


def structural_errors(catalog: list) -> list[str]:
    errors: list[str] = []
    ids: set[str] = set()
    for index, entry in enumerate(catalog):
        label = f"entry[{index}]"
        if not isinstance(entry, dict):
            errors.append(f"{label}: not an object")
            continue
        entry_id = entry.get("id")
        if not isinstance(entry_id, str) or not entry_id:
            errors.append(f"{label}: missing id")
        elif entry_id in ids:
            errors.append(f"{entry_id}: duplicate id")
        else:
            ids.add(entry_id)
        label = str(entry_id or label)
        install = entry.get("install")
        if not isinstance(install, dict):
            errors.append(f"{label}: missing install")
            continue
        transport = install.get("transport")
        if transport not in ("http", "stdio"):
            errors.append(f"{label}: unknown transport {transport!r}")
            continue
        declared = {
            secret.get("name")
            for secret in install.get("required_secrets", [])
            if isinstance(secret, dict)
        }
        for secret in declared:
            if not isinstance(secret, str) or not secret.startswith("mcp/"):
                errors.append(f"{label}: secret {secret!r} must start with 'mcp/'")
        for field in ("secret_headers", "secret_env"):
            values = install.get(field) or {}
            if not isinstance(values, dict):
                errors.append(f"{label}: {field} is not an object")
                continue
            for key, value in values.items():
                if not isinstance(value, str):
                    continue
                for name in SECRET_REF.findall(value):
                    if name not in declared:
                        errors.append(f"{label}: {field}.{key} references undeclared secret {name!r}")
        if transport == "http":
            url = install.get("url")
            # Loopback URLs are only useful for the hermetic tests; a shipped catalog is https.
            loopback = isinstance(url, str) and url.startswith(("http://127.0.0.1", "http://localhost"))
            if not isinstance(url, str) or not (url.startswith("https://") or loopback):
                errors.append(f"{label}: http transport needs an https url")
        elif not install.get("command"):
            errors.append(f"{label}: stdio transport needs a command")
        raw = json.dumps(install)
        for pattern in TOKEN_PATTERNS:
            if pattern.search(raw):
                errors.append(f"{label}: install contains a token-looking literal")
    return errors


def run_check(catalog_path: Path, timeout: float, only: str | None = None) -> dict:
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    if not isinstance(catalog, list):
        raise ValueError("catalog must be a JSON list")
    errors = structural_errors(catalog)
    entries = []
    selected = [entry for entry in catalog if only is None or entry.get("id") == only]
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = {
            pool.submit(
                check_endpoint,
                entry["install"]["url"],
                timeout,
                bool(entry["install"].get("required_secrets")),
            ): entry
            for entry in selected
            if isinstance(entry, dict) and entry.get("install", {}).get("transport") == "http"
        }
        checked = {future: future.result() for future in futures}
    for entry in selected:
        if not isinstance(entry, dict):
            continue
        install = entry.get("install", {})
        result = {
            "id": entry.get("id"),
            "transport": install.get("transport"),
            "status": "skipped-stdio",
            "auth": "none",
            "dcr": None,
            "note": "stdio entries are checked offline by catalog_contract.rs",
        }
        if install.get("transport") == "http":
            for future, source in futures.items():
                if source is entry:
                    result.update(checked[future])
                    break
        entries.append(result)
    attention = [
        {"id": item["id"], "status": item["status"], "note": item["note"]}
        for item in entries
        if item["status"] in ATTENTION
    ]
    return {
        "checked_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "catalog": str(catalog_path),
        "entries": entries,
        "attention": attention,
        "structural_errors": errors,
    }


def render_table(report: dict) -> str:
    lines = [f"{'id':<16} {'transport':<8} {'status':<22} {'auth':<20} note"]
    for item in report["entries"]:
        lines.append(
            f"{str(item['id']):<16} {str(item['transport']):<8} {str(item['status']):<22} "
            f"{str(item['auth']):<20} {item['note']}"
        )
    for error in report["structural_errors"]:
        lines.append(f"STRUCTURAL ERROR: {error}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--catalog", type=Path, default=Path("crates/mcp-bridge/catalog.json"))
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--only", help="check a single catalog id")
    parser.add_argument("--json", dest="json_path", type=Path, help="write the machine-readable report here")
    parser.add_argument("--strict", action="store_true", help="also fail on vendor/endpoint attention")
    args = parser.parse_args(argv)
    try:
        report = run_check(args.catalog, args.timeout, args.only)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"check_mcp_catalog: internal error: {exc}", file=sys.stderr)
        return 1
    if args.json_path:
        args.json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(render_table(report))
    if report["structural_errors"]:
        return 2
    if args.strict and report["attention"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
