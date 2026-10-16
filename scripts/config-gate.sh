#!/usr/bin/env bash
# short.win governance config gate (ARCHITECTURE §3.1 `propose_config`, A73; T-CONFIG).
#
# Runs docs/design/sim/config_check.py's liveness battery (and validate_params) on a proposal
# before the multisig signs it. See scripts/config_gate.py for the proposal syntax and the exit
# codes (0 ACCEPTED, 1 REJECTED liveness, 2 REJECTED validate_params, 3 NEEDS OWNER SIGN-OFF,
# 64 usage).
#
#   scripts/config-gate.sh kappa_liq_bps=3000 D:rate_base_bps=500
#   scripts/config-gate.sh --file proposal.txt --owner-signoff "SPEC §14 N42"
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHONDONTWRITEBYTECODE=1 exec python3 "$here/config_gate.py" "$@"
