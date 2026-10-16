#!/usr/bin/env python3
"""Tests of scripts/check-security-txt.py (SECTXT-1): synthetic ELF bytes around a security.txt blob,
built the way `solana_security_txt::security_txt!` lays it out.

    python3 scripts/test_check_security_txt.py
"""
import os
import subprocess
import sys
import tempfile
import unittest

CHECK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-security-txt.py")
BEGIN = b"=======BEGIN SECURITY.TXT V1=======\0"
END = b"=======END SECURITY.TXT V1=======\0"

VALID = [
    ("name", "short.win"),
    ("project_url", "https://short.win"),
    ("contacts", "email:security@short.win,link:https://short.win/security"),
    ("policy", "https://short.win/security"),
    ("preferred_languages", "en"),
    ("source_release", "v1.0.0"),
]


def blob(fields):
    return BEGIN + b"".join(k.encode() + b"\0" + v.encode() + b"\0" for k, v in fields) + END


def check(data: bytes, *args):
    with tempfile.NamedTemporaryFile(suffix=".so", delete=False) as f:
        f.write(data)
    try:
        p = subprocess.run([sys.executable, CHECK, *args, f.name], capture_output=True, text=True,
                           check=False)
    finally:
        os.unlink(f.name)
    return p.returncode, p.stdout + p.stderr


class CheckSecurityTxt(unittest.TestCase):
    def expect(self, code, data, *args, says=None):
        rc, out = check(data, *args)
        self.assertEqual(rc, code, out)
        if says:
            self.assertIn(says, out)
        return out

    def test_valid(self):
        self.expect(0, b"\x7fELF" + blob(VALID) + b"\0" * 16, says="carries a valid security.txt")

    def test_expected_name(self):
        data = blob(VALID)
        with tempfile.NamedTemporaryFile(suffix=".so", delete=False) as f:
            f.write(data)
        try:
            ok = subprocess.run([sys.executable, CHECK, f.name, "short.win"], capture_output=True,
                                text=True, check=False)
            bad = subprocess.run([sys.executable, CHECK, f.name, "short.win helper"],
                                 capture_output=True, text=True, check=False)
        finally:
            os.unlink(f.name)
        self.assertEqual(ok.returncode, 0, ok.stdout)
        self.assertEqual(bad.returncode, 1, bad.stdout)
        self.assertIn("expected 'short.win helper'", bad.stdout)

    def test_no_marker_or_two(self):
        self.expect(1, b"\x7fELF" + b"\0" * 64, says="0 security.txt BEGIN markers")
        self.expect(1, blob(VALID) + blob(VALID), says="2 security.txt BEGIN markers")

    def test_missing_end(self):
        self.expect(1, blob(VALID)[: -len(END)], says="END marker not found")

    def test_missing_required(self):
        for key in ("name", "project_url", "contacts", "policy"):
            self.expect(1, blob([(k, v) for k, v in VALID if k != key]), says=f"missing field {key}")

    def test_unknown_and_duplicate(self):
        self.expect(1, blob(VALID + [("bounty", "x")]), says="unknown field bounty")
        self.expect(1, blob(VALID + [("name", "again")]), says="duplicate field name")

    def test_contacts_are_typed(self):
        bad = [(k, "security@short.win" if k == "contacts" else v) for k, v in VALID]
        self.expect(1, blob(bad), says="invalid contact")
        bad = [(k, "fax:123" if k == "contacts" else v) for k, v in VALID]
        self.expect(1, blob(bad), says="invalid contact")

    def test_malformed_blob(self):
        # No NUL between the last value and the END marker.
        self.expect(1, BEGIN + b"name\0short.win" + END, says="does not end with a NUL")
        # A key without a value.
        self.expect(1, blob(VALID)[: -len(END)] + b"auditors\0" + END, says="odd number of")

    def test_non_utf8_value(self):
        data = blob(VALID).replace(b"v1.0.0", b"v1.0.\xff")
        self.expect(1, data, says="field b'source_release' is not UTF-8")

    def test_placeholders_pass_unless_forbidden(self):
        held = [(k, "https://OWNER-FILL-domain" if k == "project_url" else v) for k, v in VALID]
        self.expect(0, blob(held), says="OWNER-FILL placeholders in project_url")
        self.expect(1, blob(held), "--forbid-placeholders", says="the release gate")
        self.expect(0, blob(VALID), "--forbid-placeholders")

    def test_source_release(self):
        self.expect(0, blob(VALID), "--source-release", "v1.0.0")
        self.expect(1, blob(VALID), "--source-release", "v0.1.0",
                    says="source_release 'v1.0.0', expected 'v0.1.0'")
        none = [(k, v) for k, v in VALID if k != "source_release"]
        self.expect(1, blob(none), "--source-release", "v1.0.0", says="source_release None")


if __name__ == "__main__":
    unittest.main()
