"""Gentle, read-only Solana JSON-RPC client of the gap-calibration crawler (WP-9; plan §1 WP-9).

Standard library only, in the style of ``scripts/dump-fixtures.py``. What it guarantees:

- **Read-only.** Only the ``get*`` methods in ``READ_METHODS`` are ever sent; anything else is
  refused before it reaches the network.
- **Gentle.** Each ``Rpc`` instance has its own token bucket, which spaces every HTTP request of
  that instance (retries included, both endpoints together) at most ``1/rps`` apart; ``rps`` is
  capped at ``MAX_RPS`` = 3. A process with several instances must keep their sum within the
  budget: the crawler (``fetch.py``) has two, ``fg_rps + bg_rps ≤ 3`` enforced, and
  ``universe.py census`` refuses to run beside a live crawl. A 429, a
  5xx, a network error or a "node behind" RPC error backs off exponentially with jitter
  (honouring ``Retry-After``), from 2 s up to 5 min, and the crawler keeps going.
- **Two endpoints.** ``primary`` (publicnode: recent history, about 28 h of transactions) serves
  the signature listings and transactions; ``archive`` (api.mainnet-beta) serves what publicnode
  refuses without a token — ``getProgramAccounts`` ("Indexed requests require a personal token")
  and ``getMultipleAccounts`` of more than ``PRIMARY_MULTI_MAX`` = 10 keys ("Request blocked",
  measured 2026-10-02; mainnet-beta takes 100, so a batch is one request instead of ten) — and
  the backfill of transactions publicnode no longer has (``getTransaction`` returning ``null``).
- **Cached.** ``transaction()`` keeps every finalized transaction as gzip'd JSON under
  ``<cache_dir>/<sig[:2]>/<sig>.json.gz``; a cached signature costs no call, so a re-run of a
  finished day costs 0 calls. Plain ``.json`` files of the M10 pilot's cache layout are read too.

``Rpc.stats`` counts calls per endpoint and method, retries and errors (the crawler logs it).
"""

import gzip
import hashlib
import json
import os
import random
import time
import urllib.error
import urllib.request

PUBLICNODE = "https://solana-rpc.publicnode.com"
MAINNET_BETA = "https://api.mainnet-beta.solana.com"
MAX_RPS = 3.0
PRIMARY_MULTI_MAX = 10  # publicnode blocks getMultipleAccounts above 10 keys without a token
USER_AGENT = "shortwin-gapcal/1 (read-only research crawler, <= 3 req/s)"

READ_METHODS = frozenset({
    "getAccountInfo",
    "getMultipleAccounts",
    "getProgramAccounts",
    "getSignaturesForAddress",
    "getTransaction",
    "getSlot",
    "getBlockTime",
    "getTokenSupply",
    "getHealth",
    "getVersion",
})
# RPC error codes that mean "try again later": rate limits, node behind, slot or block not yet
# available, long-term storage hiccups.
RETRY_CODES = frozenset({429, -32429, -32005, -32004, -32007, -32009, -32014, -32016})

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58encode(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


def b58decode(s):
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    body = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\0" * (len(s) - len(s.lstrip("1"))) + body


def anchor_event_disc(name):
    """Anchor's event discriminator ``sha256("event:<name>")[..8]``."""
    return hashlib.sha256(f"event:{name}".encode()).digest()[:8]


def anchor_account_disc(name):
    """Anchor's account discriminator ``sha256("account:<name>")[..8]``."""
    return hashlib.sha256(f"account:{name}".encode()).digest()[:8]


class RpcError(Exception):
    """A non-retryable JSON-RPC error (the request itself is wrong or unsupported)."""

    def __init__(self, method, url, error):
        super().__init__(f"{method} on {url}: {error}")
        self.error = error


class RateLimiter:
    """At most one request every ``1/rps`` seconds for its ``Rpc`` instance (``clock`` and
    ``sleep`` are injectable for the tests)."""

    def __init__(self, rps, clock=time.monotonic, sleep=time.sleep):
        if not 0 < rps <= MAX_RPS:
            raise ValueError(f"rps must be in (0, {MAX_RPS}]: {rps}")
        self.min_dt = 1.0 / rps
        self.clock, self.sleep = clock, sleep
        self.next_at = 0.0

    def wait(self):
        now = self.clock()
        if self.next_at > now:
            self.sleep(self.next_at - now)
            now = self.next_at
        self.next_at = now + self.min_dt


def _http_post(url, body, timeout):
    """One POST; returns (status, headers, bytes). Network failures raise OSError."""
    req = urllib.request.Request(url, data=body, headers={
        "Content-Type": "application/json", "User-Agent": USER_AGENT,
        "Accept-Encoding": "gzip"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = r.read()
            if r.headers.get("Content-Encoding") == "gzip":
                data = gzip.decompress(data)
            return r.status, dict(r.headers), data
    except urllib.error.HTTPError as e:
        data = e.read() if e.fp else b""
        if (e.headers or {}).get("Content-Encoding") == "gzip":
            try:
                data = gzip.decompress(data)
            except (OSError, EOFError):
                pass
        return e.code, dict(e.headers or {}), data


class Rpc:
    """See the module docs. ``transport(url, body, timeout) -> (status, headers, bytes)`` and
    ``sleep`` are injectable (the offline tests use a fake transport)."""

    def __init__(self, primary=PUBLICNODE, archive=MAINNET_BETA, rps=2.5, cache_dir=None,
                 log=None, transport=_http_post, sleep=time.sleep, clock=time.monotonic,
                 max_backoff=300.0, max_attempts=12, seed_cache_dirs=()):
        self.urls = {"primary": primary, "archive": archive or primary}
        self.limiter = RateLimiter(rps, clock=clock, sleep=sleep)
        self.cache_dir = cache_dir
        self.seed_cache_dirs = list(seed_cache_dirs)  # read-only caches (e.g. the M10 pilot's)
        self.log = log or (lambda msg: None)
        self.transport, self.sleep = transport, sleep
        self.max_backoff, self.max_attempts = max_backoff, max_attempts
        self.stats = {"calls": 0, "retries": 0, "errors": 0, "cache_hits": 0, "by": {}}
        self._id = 0

    # ---- core -----------------------------------------------------------------------------

    def call(self, method, params, endpoint="primary", timeout=60):
        if method not in READ_METHODS:
            raise ValueError(f"refusing non-read RPC method {method}")
        url = self.urls[endpoint]
        delay = 2.0
        for attempt in range(self.max_attempts):
            self._id += 1
            body = json.dumps({"jsonrpc": "2.0", "id": self._id, "method": method,
                               "params": params}).encode()
            self.limiter.wait()
            self.stats["calls"] += 1
            key = f"{endpoint}:{method}"
            self.stats["by"][key] = self.stats["by"].get(key, 0) + 1
            retry_after = None
            try:
                status, headers, data = self.transport(url, body, timeout)
            except (OSError, TimeoutError) as e:  # URLError, connection reset, timeout
                why = f"network {e!r}"
            else:
                if status == 429 or status >= 500:
                    why = f"HTTP {status}"
                    ra = (headers or {}).get("Retry-After")
                    if ra and str(ra).isdigit():
                        retry_after = float(ra)
                elif status != 200 and status != 400:
                    raise RpcError(method, url, f"HTTP {status}: {data[:200]!r}")
                else:
                    try:
                        out = json.loads(data)
                    except json.JSONDecodeError:
                        why = f"bad JSON (HTTP {status})"
                    else:
                        err = out.get("error")
                        if err is None and "result" in out:
                            return out["result"]
                        if err and err.get("code") in RETRY_CODES:
                            why = f"RPC {err.get('code')} {err.get('message', '')[:80]}"
                        else:
                            raise RpcError(method, url, err or out)
            self.stats["retries"] += 1
            wait = retry_after if retry_after is not None else delay * (0.75 + 0.5 * random.random())
            wait = min(wait, self.max_backoff)
            self.log(f"rpc {method} on {endpoint}: {why}; attempt {attempt + 1}, backoff {wait:.0f}s")
            self.sleep(wait)
            delay = min(delay * 2, self.max_backoff)
        self.stats["errors"] += 1
        raise RpcError(method, url, f"gave up after {self.max_attempts} attempts")

    # ---- helpers --------------------------------------------------------------------------

    def slot(self):
        return self.call("getSlot", [{"commitment": "finalized"}])

    def multiple(self, keys, data_slice=None, commitment="confirmed"):
        """``getMultipleAccounts`` (at most 100 keys; more than ``PRIMARY_MULTI_MAX`` go to the
        archive endpoint): ``(context slot, [value or None])``."""
        assert len(keys) <= 100, len(keys)
        opts = {"encoding": "base64", "commitment": commitment}
        if data_slice:
            opts["dataSlice"] = {"offset": data_slice[0], "length": data_slice[1]}
        endpoint = "primary" if len(keys) <= PRIMARY_MULTI_MAX else "archive"
        res = self.call("getMultipleAccounts", [list(keys), opts], endpoint=endpoint)
        return res["context"]["slot"], res["value"]

    def program_accounts(self, program, filters, data_slice=None, timeout=600):
        """``getProgramAccounts`` on the archive endpoint (publicnode refuses indexed requests)."""
        opts = {"encoding": "base64", "filters": filters, "commitment": "confirmed"}
        if data_slice:
            opts["dataSlice"] = {"offset": data_slice[0], "length": data_slice[1]}
        return self.call("getProgramAccounts", [program, opts], endpoint="archive", timeout=timeout)

    def signatures(self, address, before=None, until=None, limit=1000, endpoint="primary"):
        """One page of ``getSignaturesForAddress`` (finalized, newest first)."""
        opts = {"limit": limit, "commitment": "finalized"}
        if before:
            opts["before"] = before
        if until:
            opts["until"] = until
        return self.call("getSignaturesForAddress", [address, opts], endpoint=endpoint)

    # ---- transactions (cached) ------------------------------------------------------------

    def _tx_paths(self, sig, root=None):
        d = os.path.join(root or self.cache_dir, sig[:2])
        return os.path.join(d, sig + ".json.gz"), os.path.join(d, sig + ".json")

    def cached_transaction(self, sig):
        """The cached transaction of ``sig`` or None; never calls."""
        for root in ([self.cache_dir] if self.cache_dir else []) + self.seed_cache_dirs:
            gz, plain = self._tx_paths(sig, root)
            if os.path.exists(gz):
                with gzip.open(gz, "rt") as f:
                    return json.load(f)
            if os.path.exists(plain):
                with open(plain) as f:
                    return json.load(f)
        return None

    def transaction(self, sig, backfill=True):
        """The finalized transaction of ``sig`` (``encoding=json``), from the cache when present.
        A ``null`` from the primary endpoint (outside its retention) is retried on the archive
        endpoint when ``backfill``. Returns None if neither has it."""
        tx = self.cached_transaction(sig)
        if tx is not None:
            self.stats["cache_hits"] += 1
            return tx
        opts = {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}
        tx = self.call("getTransaction", [sig, opts])
        if tx is None and backfill and self.urls["archive"] != self.urls["primary"]:
            tx = self.call("getTransaction", [sig, opts], endpoint="archive")
        if tx is not None and self.cache_dir:
            gz, _ = self._tx_paths(sig)
            os.makedirs(os.path.dirname(gz), exist_ok=True)
            tmp = gz + ".tmp"
            with gzip.open(tmp, "wt", compresslevel=6) as f:
                json.dump(tx, f, separators=(",", ":"))
            os.replace(tmp, gz)
        return tx
