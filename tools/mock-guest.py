#!/usr/bin/env python3
"""Resolve a synthetic guest for tests/mock_pms.py without using a Plex account token."""
import argparse
import json
from pathlib import Path
import re
import urllib.error
import urllib.request

MOCK_TOKEN = "plxnative-mock-guest"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("mock identity endpoint redirected")


def configured_origin(config):
    text = Path(config).read_text()
    host = re.search(r'^\s*#define\s+PMS_HOST\s+"([^"]+)"\s*$', text, re.M)
    port = re.search(r'^\s*#define\s+PMS_PORT\s+(\d+)\s*$', text, re.M)
    if not host or not port or not re.fullmatch(r"[A-Za-z0-9.-]+", host[1]):
        raise ValueError("mock guest requires explicit PMS_HOST and PMS_PORT in config.local.h")
    if not 1 <= int(port[1]) <= 65535:
        raise ValueError("invalid mock PMS port")
    return f"http://{host[1]}:{int(port[1])}"


def resolve(config):
    origin = configured_origin(config)
    opener = urllib.request.build_opener(NoRedirect, urllib.request.ProxyHandler({}))

    def get(path):
        request = urllib.request.Request(origin + path, headers={"Accept": "application/json"})
        with opener.open(request, timeout=5) as response:
            body = response.read(65537)
        if len(body) > 65536:
            raise ValueError("mock identity response too large")
        return json.loads(body)

    identity = get("/identity").get("MediaContainer", {})
    user = get("/api/v2/user")
    if (identity.get("version") != "1.41.0.0000-synthetic"
            or user.get("uuid") != "demo-user" or user.get("username") != "demo"):
        raise ValueError("configured server is not the synthetic mock PMS")
    # This value is fabricated, never an account or per-server access token. The mock accepts
    # arbitrary nonempty tokens; a real PMS cannot authenticate it even if misconfigured later.
    return MOCK_TOKEN


def selftest():
    import http.server
    import tempfile
    import threading
    import unittest

    class Tests(unittest.TestCase):
        def test_synthetic_identity_and_refusal(self):
            seen = []
            state = {"real": False, "redirect": False}

            class Handler(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_GET(self):
                    seen.append((self.path, dict(self.headers)))
                    if state["redirect"]:
                        self.send_response(302)
                        self.send_header("Location", "/api/v2/user")
                        self.end_headers()
                        return
                    body = ({"MediaContainer": {"version": "real" if state["real"]
                             else "1.41.0.0000-synthetic"}} if self.path == "/identity"
                            else {"uuid": "demo-user", "username": "demo"})
                    data = json.dumps(body).encode()
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    config = Path(tmp) / "config.local.h"
                    config.write_text('#define PMS_HOST "127.0.0.1"\n'
                                      f'#define PMS_PORT {server.server_port}\n'
                                      '#define PMS_TOKEN "MUST-NOT-BE-READ-OR-SENT"\n')
                    self.assertEqual(resolve(config), MOCK_TOKEN)
                    self.assertEqual([p for p, _ in seen], ["/identity", "/api/v2/user"])
                    self.assertTrue(all("X-Plex-Token" not in h for _, h in seen))
                    state["real"] = True
                    with self.assertRaisesRegex(ValueError, "not the synthetic"):
                        resolve(config)
                    state["redirect"] = True
                    count = len(seen)
                    with self.assertRaisesRegex(ValueError, "redirected"):
                        resolve(config)
                    self.assertEqual(len(seen), count + 1)
                    config.write_text('#define PMS_HOST "example.invalid/escape"\n#define PMS_PORT 80\n')
                    with self.assertRaisesRegex(ValueError, "explicit PMS_HOST"):
                        resolve(config)
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Tests)
    return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", nargs="?")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    if not args.config:
        parser.error("config.local.h path is required")
    try:
        token = resolve(args.config)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, urllib.error.URLError):
        parser.exit(1, "mock guest refused: configured PMS must answer as tests/mock_pms.py\n")
    print(token)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
