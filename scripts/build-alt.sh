#!/usr/bin/env bash
# The protocol address lookup table (ARCHITECTURE §6.4, A28): print, create, extend or check it.
# A thin wrapper around the `shortwin-build-alt` binary of the keeper crate
# (services/keeper/src/bin/build_alt.rs; the list is `shortwin_keeper::alt`):
#
#   scripts/build-alt.sh print                                  # the static keys (no RPC)
#   scripts/build-alt.sh print  --rpc <url>                      # + live pump / PumpSwap recipients
#   scripts/build-alt.sh create --rpc <url> --keypair payer.json [--authority auth.json] [--freeze]
#   scripts/build-alt.sh extend --rpc <url> --keypair payer.json --table <alt>
#   scripts/build-alt.sh check  --rpc <url> --table <alt>        # exit 1 if an address is missing
#   scripts/build-alt.sh market --rpc <url> --market <market> [--table <alt>] [--keypair payer.json]
#                                                                # M7: a market's own table (OWNER-M7-1)
#
# Contents: the programs a short.win instruction names as accounts (pump, pAMM, pfee, AMM v4,
# CPMM, SPL Token, Token-2022, ATA, System), short.win's event authority, the WSOL mint, the
# pump and PumpSwap statics (global / global_config, event authorities, fee configs, global
# volume accumulators), the AMM v4 and CPMM authorities, the 21 CPMM AmmConfigs, short.win's
# config / venue_guard / oracle_cache, the four VenueExtras PDAs, and (with --rpc, unless
# --static-only) the live pump and PumpSwap fee and buyback recipients with their WSOL ATAs.
# Put the created table's address into the keeper's `lookup_tables` (keeper.example.toml) and
# the frontend's v0 transactions. `market` (M7; ARCH §6.4, OWNER-M7-1's fallback) lists a
# market's own table — its accounts, its venue's ReadSet and trade extras and the keys its
# `current` extras sets resolve to, less the protocol table's (`--table`) — with its rent, and
# with `--keypair` creates it (extendable, never frozen); add it to `lookup_tables` too.
#
# The build uses the services workspace (its own Cargo.lock); CARGO_TARGET_DIR is honoured.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
exec cargo run --quiet --locked --manifest-path "$root/services/Cargo.toml" \
  -p shortwin-keeper --bin shortwin-build-alt -- "$@"
