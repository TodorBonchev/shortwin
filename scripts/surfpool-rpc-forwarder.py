#!/usr/bin/env python3
"""A retrying JSON-RPC forwarder between Surfpool and public mainnet RPCs (M6 Surfpool E2E,
ARCHITECTURE §8.4, §9 M6; tests/litesvm/tests/graduation_surfpool.rs).

Surfpool 1.6 fetches mainnet accounts lazily with batched `getMultipleAccounts`. On 2026-09-30
`solana-rpc.publicnode.com` answered those batches with HTTP 403 (the same batches sent by `curl`
succeed) and `api.mainnet-beta.solana.com` dropped some under its rate limit. This forwarder
listens on 127.0.0.1, sends every request upstream with a `curl` user agent, alternates between
the upstreams and retries up to 12 times with a growing pause; after that it answers with a
JSON-RPC error. Standard library only.

    scripts/surfpool-rpc-forwarder.py [PORT] [UPSTREAM ...]
    # default PORT 18999; default upstreams publicnode, then api.mainnet-beta

scripts/surfpool-e2e.sh starts it, Surfpool behind it, and the ignored E2E.
"""
import http.server
import json
import sys
import threading
import time
import urllib.error
import urllib.request

UPSTREAMS = sys.argv[2:] or [
    "https://solana-rpc.publicnode.com",
    "https://api.mainnet-beta.solana.com",
]
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18999
ATTEMPTS = 12
LOCK = threading.Lock()


def forward(body: bytes) -> bytes:
    last = None
    for attempt in range(ATTEMPTS):
        url = UPSTREAMS[attempt % len(UPSTREAMS)]
        req = urllib.request.Request(url, data=body, headers={
            "Content-Type": "application/json", "User-Agent": "curl/8.9.1", "Accept": "*/*"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            last = f"{url} {e.code}"
        except Exception as e:  # network errors, timeouts
            last = f"{url} {e}"
        time.sleep(min(0.25 * (attempt + 1), 3))
    sys.stderr.write(f"giving up: {last}\n")
    err = {"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": f"forwarder: {last}"}}
    return json.dumps(err).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 (http.server's name)
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        with LOCK:
            out = forward(body)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
