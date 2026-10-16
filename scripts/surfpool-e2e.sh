#!/usr/bin/env bash
# The M6 Surfpool E2E through a real graduation (ARCHITECTURE §8.4, §9 M6; U19):
# tests/litesvm/tests/graduation_surfpool.rs, `#[ignore]`d in the normal suites because it needs
# a running Surfpool forking live mainnet and network access.
#
#   scripts/surfpool-e2e.sh
#
# Starts scripts/surfpool-rpc-forwarder.py on 127.0.0.1:$FORWARDER_PORT (a retrying JSON-RPC
# forwarder: the public RPCs refuse some of Surfpool's batched requests), a fresh Surfpool 1.6
# fork behind it on $SURFPOOL_PORT, runs the ignored test against it, and stops both.
#
# Environment:
#   SURFPOOL          the Surfpool binary (default: `surfpool` on PATH). Tested with the v1.6.0
#                     release `surfpool-linux-x64.tar.gz` (sha256 cb471b00…, the release digest).
#   SHORTWIN_SO       the release short.win the test plants (default target/deploy/shortwin.so;
#                     build it first: anchor build --tools-version v1.57 --arch v3).
#   FORWARDER_PORT    default 18999;  SURFPOOL_PORT default 18899 (websocket: SURFPOOL_PORT + 1).
#   UPSTREAMS         space-separated mainnet RPCs for the forwarder (default: publicnode, then
#                     api.mainnet-beta).
#   SURFPOOL_TRACE=1  the test prints every transaction's CU and errors.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
surfpool=${SURFPOOL:-surfpool}
fport=${FORWARDER_PORT:-18999}
sport=${SURFPOOL_PORT:-18899}
wport=$((sport + 1))
work=$(mktemp -d "${TMPDIR:-/tmp}/shortwin-surfpool.XXXXXX")

command -v "$surfpool" >/dev/null || { echo "error: Surfpool not found (set SURFPOOL)" >&2; exit 1; }

cleanup() {
  local rc=$?
  set +e
  if [ -n "${sp:-}" ]; then kill "$sp" 2>/dev/null; wait "$sp" 2>/dev/null; fi
  if [ -n "${fw:-}" ]; then kill "$fw" 2>/dev/null; wait "$fw" 2>/dev/null; fi
  rm -rf "$work"
  exit "$rc"
}
trap cleanup EXIT

# shellcheck disable=SC2086
python3 "$root/scripts/surfpool-rpc-forwarder.py" "$fport" ${UPSTREAMS:-} >"$work/forwarder.log" 2>&1 &
fw=$!

(cd "$work" && exec "$surfpool" start --rpc-url "http://127.0.0.1:$fport" --no-tui --no-studio \
  --no-deploy --port "$sport" --ws-port "$wport" --airdrop-amount 0 --log-path ./logs \
  >"$work/surfpool.log" 2>&1) &
sp=$!

up=0
for _ in $(seq 1 120); do
  if curl -s -X POST -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' "http://127.0.0.1:$sport" >/dev/null 2>&1; then
    up=1
    break
  fi
  sleep 0.5
done
[ "$up" = 1 ] || { echo "error: Surfpool did not answer on :$sport" >&2; cat "$work/surfpool.log" >&2; exit 1; }

cd "$root"
SURFPOOL_RPC="127.0.0.1:$sport" cargo test -p shortwin-litesvm-tests --locked \
  --test graduation_surfpool -- --ignored --nocapture
