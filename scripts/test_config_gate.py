#!/usr/bin/env python3
"""Tests of scripts/config-gate.sh (T-CONFIG, ARCH §8.2: the gate rejects a proposal with a liveness row
in config_check.py's 1,655-run battery and requires the owner's sign-off for lender-income-only rows).

    python3 scripts/test_config_gate.py            # about 30 s on 8 cores
"""
import os
import subprocess
import sys
import tempfile
import unittest

GATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "config-gate.sh")


def gate(*args):
    p = subprocess.run([GATE, *args], capture_output=True, text=True, check=False)
    return p.returncode, p.stdout + p.stderr


class ConfigGate(unittest.TestCase):
    def expect(self, code, *args, says=None):
        rc, out = gate(*args)
        self.assertEqual(rc, code, f"{args}: exit {rc}\n{out}")
        if says:
            self.assertIn(says, out, f"{args}\n{out}")
        return out

    def test_defaults_are_accepted(self):
        self.expect(0, says="ACCEPTED: no row of 1655")

    def test_out_of_bounds_is_rejected_by_validate_params(self):
        self.expect(2, "penalty_bps=2001", says="REJECTED by validate_params")
        self.expect(2, "A:cr_liq_bps=12000", says="SPEC minima")
        # Unlike config_check.py's analysis mode, the gate never clamps a proposal.
        self.expect(2, "cr_liq_bps=100000", says="cr_open >= cr_liq + 2,000")

    def test_a_liveness_row_rejects(self):
        # In bounds (rate_base_bps ∈ [0, 50,000]), yet an aged Tier D liquidation becomes an S1.
        self.expect(1, "rate_base_bps=0", says="REJECTED (liveness)")
        # The same change in Tier D alone (a per-tier proposal).
        self.expect(1, "D:rate_base_bps=0", says="REJECTED (liveness)")

    def test_lender_income_rows_need_the_owners_signoff(self):
        self.expect(3, "penalty_bps=0", says="NEEDS OWNER SIGN-OFF")
        self.expect(0, "penalty_bps=0", "--owner-signoff", "test-only", says="ACCEPTED with the owner's sign-off")
        self.expect(64, "penalty_bps=0", "--owner-signoff", " ")

    def test_program_names_and_per_tier_sizing(self):
        # ECON §15 / program names map onto the sim's (settle_keeper_* = settle_*).
        self.expect(3, "settle_keeper_fixed=0", says="settle_fixed=0")
        self.expect(0, "A:impact_pos_bps=1000", "A:max_duration_s=2592000", says="A:max_dur_s=2592000")

    def test_parameters_the_sim_does_not_model_are_listed(self):
        out = self.expect(0, "min_notional=5000000", "oracle_max_age_s=60", says="ACCEPTED")
        self.assertIn("not simulated", out)

    def test_proposal_file(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
            f.write("# a governance proposal\nD:rate_base_bps = 0\n\n")
        try:
            self.expect(1, "--file", f.name, says="REJECTED (liveness)")
        finally:
            os.unlink(f.name)

    def test_usage_errors(self):
        for bad in ("tau=7", "ema_tau_s=6", "B:open_fee_bps=1", "E:penalty_bps=1", "nonsense=1",
                    "kappa_liq_bps=x", "kappa_liq_bps", "kappa_liq_bps=-1", "B:ema_dtmax_s=1"):
            self.expect(64, bad)
        self.expect(64, "kappa_liq_bps=2500", "kappa_liq_bps=3000", says="set twice")


if __name__ == "__main__":
    sys.exit(unittest.main())
