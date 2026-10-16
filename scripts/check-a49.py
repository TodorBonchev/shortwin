#!/usr/bin/env python3
"""A49: release artifacts know nothing of the TEST-ONLY mock venue (ARCHITECTURE §8.2, §9 M2).

    scripts/check-a49.py [--has-id F...] [--no-id F...] [--has-marker F...] [--no-marker F...]

The mock venue (tests/mock_cpamm, never deployed) and short.win's `test-venue` build
(`VenueKind::Mock`) leave two fingerprints in a binary:

- the mock program id (`declare_id!` in tests/mock_cpamm/src/lib.rs), as 32 bytes of data, as
  its four 8-byte words, or as the four 64-bit `lddw` immediates the SBF compiler turns a constant
  key into (each word split 4 + 4 around four zero bytes);
- the marker string `shortwin:test-venue:mock-cpamm` the mock adapter logs.

`--no-id` / `--no-marker` files must carry neither; `--has-id` / `--has-marker` files must carry
it. A `--no-*` check needs at least one `--has-*` file of the same kind, which proves the search
can see what it looks for; a run with no file at all is an error, not a pass. Used by ci.yml's anchor-build, verifiable-build and services jobs.
Run from the repository root.
"""
import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MARKER = b"shortwin:test-venue:mock-cpamm"
ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def mock_id():
    src = (ROOT / "tests/mock_cpamm/src/lib.rs").read_text()
    pid = re.search(r'declare_id!\("([1-9A-HJ-NP-Za-km-z]{32,44})"\)', src).group(1)
    n = 0
    for c in pid:
        n = n * 58 + ALPHABET.index(c)
    return pid, n.to_bytes(32, "big")


def embeds_id(elf, key):
    words = [key[i:i + 8] for i in range(0, 32, 8)]
    return (key in elf
            or all(w in elf for w in words)
            or all(w[:4] + bytes(4) + w[4:] in elf for w in words))


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    for flag in ("--has-id", "--no-id", "--has-marker", "--no-marker"):
        ap.add_argument(flag, nargs="+", default=[], action="extend", metavar="FILE")
    a = ap.parse_args()
    if not (a.has_id or a.no_id or a.has_marker or a.no_marker):
        ap.error("nothing to check: give at least one --has-*/--no-* file")
    if a.no_id and not a.has_id or a.no_marker and not a.has_marker:
        ap.error("each --no-* check needs a --has-* file that proves the search finds it")
    pid, key = mock_id()
    errors = []
    for files, needle, want, what in (
            (a.has_id, key, True, f"the mock venue id {pid}"),
            (a.no_id, key, False, f"the mock venue id {pid}"),
            (a.has_marker, MARKER, True, f"the test-venue marker {MARKER.decode()}"),
            (a.no_marker, MARKER, False, f"the test-venue marker {MARKER.decode()}")):
        for f in files:
            elf = pathlib.Path(f).read_bytes()
            found = embeds_id(elf, needle) if needle is key else needle in elf
            if found != want:
                errors.append(f"{f} {'lacks' if want else 'embeds'} {what}")
            else:
                print(f"ok: {f} {'embeds' if want else 'is free of'} {what}")
    for e in errors:
        print(f"::error::{e}")
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
