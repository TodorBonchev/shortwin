#!/usr/bin/env python3
"""short.win audit scope (docs/AUDIT.md §2; M10 WP-10).

    python3 scripts/audit_scope.py write   # regenerate docs/audit/scope.txt from `git ls-files`
    python3 scripts/audit_scope.py check   # exit 1 if docs/audit/scope.txt differs from it, or a
                                           # watched file is in no unit and not on the excluded list
    python3 scripts/audit_scope.py nloc    # the AUDIT.md §2 table: files and non-test nLOC per unit

The scope is a list of units, each a set of git pathspecs. `write` lists the files git tracks at
HEAD under each unit, so the file is regenerated (and `check`ed) at the audit tag; it carries no
commit id, AUDIT.md §0 does. Every file git tracks under a watched root (`programs/`, `crates/`,
`services/`, and each member of the two Cargo workspaces) must be in exactly one unit or match
an EXCLUDED pathspec; `write` and `check` fail otherwise, so a new crate, program or service
(e.g. a governance tool under `services/`) is classified by a reviewed edit here, never by
default.

Non-test nLOC: every `.rs` file of a unit with each `#[cfg(test)]` / `#[cfg(all(test, …))]` item
removed (its attributes and the braced block or `;` item that follows; braces, brackets and `;`
inside string, char and comment text are not counted), then the lines that are neither blank
nor comments (`//…`, `/* … */`). It is the line-based heuristic the M10 audit
preparation used; tokei 14.0.0's "code" column, which keeps the inline test modules, is the
second column of AUDIT.md §2 (`tokei <paths> -t Rust`). Standard library only.
"""
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCOPE = os.path.join(ROOT, "docs", "audit", "scope.txt")

# (unit, scope class, description, pathspecs). Order is the order of scope.txt.
UNITS = [
    ("shortwin", "core", "the program (release build; the test-venue mock adapter is below)",
     ["programs/shortwin/Cargo.toml", "programs/shortwin/src",
      ":(exclude)programs/shortwin/src/venues/mock.rs"]),
    # The stateless straddle helper `complete_curve` (ARCH §5.4, A66) was a core unit of the
    # audited 1.0.0 scope; retired with pump's v3 exits and removed from the tree (design
    # pump-final §11, owner decision D2; AUDIT.md §0).
    ("shortwin-math", "core", "no_std protocol math, linked into the program and the keeper",
     ["crates/shortwin-math/Cargo.toml", "crates/shortwin-math/src"]),
    ("shortwin-venue-state", "core", "no_std venue, mint, Pyth and ProgramData readers",
     ["crates/shortwin-venue-state/Cargo.toml", "crates/shortwin-venue-state/src",
      ":(exclude)crates/shortwin-venue-state/src/mock_cpamm.rs"]),
    ("build", "core", "workspace manifests, lockfile and toolchain pins the release ELF builds from",
     ["Cargo.toml", "Cargo.lock", "Anchor.toml", "rust-toolchain.toml"]),
    ("keeper-core", "keeper", "exit and crank decisions, coverage and budget, the instructions and "
     "account lists the keeper sends (in scope: the owner's decision of 2026-10-02)",
     ["services/keeper/src/decide.rs", "services/keeper/src/coverage.rs",
      "services/keeper/src/budget.rs", "services/keeper/src/fees.rs",
      "services/keeper/src/graduation.rs", "services/keeper/src/ixs.rs",
      "services/keeper/src/venue.rs", "services/keeper/src/view.rs",
      "services/keeper/src/pda.rs", "services/keeper/src/sweep.rs",
      "services/keeper/src/monitor.rs", "services/keeper/src/pool_monitor.rs",
      "services/common/src/tx.rs", "services/common/src/chain.rs"]),
    ("keeper-rest", "context", "the keeper loop, configuration, metrics, alerts, the RPC backend, "
     "the ALT and v1-probe tools, and the services workspace manifests (out of scope; read for "
     "context)",
     ["services/Cargo.toml", "services/Cargo.lock",
      "services/keeper/Cargo.toml", "services/keeper/keeper.example.toml",
      "services/keeper/src", "services/common/Cargo.toml", "services/common/src",
      ":(exclude)services/keeper/src/decide.rs", ":(exclude)services/keeper/src/coverage.rs",
      ":(exclude)services/keeper/src/budget.rs", ":(exclude)services/keeper/src/fees.rs",
      ":(exclude)services/keeper/src/graduation.rs", ":(exclude)services/keeper/src/ixs.rs",
      ":(exclude)services/keeper/src/venue.rs", ":(exclude)services/keeper/src/view.rs",
      ":(exclude)services/keeper/src/pda.rs", ":(exclude)services/keeper/src/sweep.rs",
      ":(exclude)services/keeper/src/monitor.rs", ":(exclude)services/keeper/src/pool_monitor.rs",
      ":(exclude)services/common/src/tx.rs", ":(exclude)services/common/src/chain.rs"]),
    ("governance", "context", "the governance tool `shortwin-gov` (services/ops) and its Squads v4 "
     "encoders (crates/squads-v4-ix): host-only, never on-chain; it stages and verifies, members "
     "review and sign in the Squads app (owner decision of 2026-10-03); out of scope (the owner's "
     "audit scope of 2026-10-02 names the programs and the keeper core), read for context",
     ["crates/squads-v4-ix/Cargo.toml", "crates/squads-v4-ix/src",
      "services/ops/Cargo.toml", "services/ops/governance.example.toml", "services/ops/src"]),
    ("test-only", "out", "TEST-ONLY mock venue code: compiled only with feature `test-venue`, "
     "never in the release ELF (A49; CI checks that the release shortwin.so embeds neither the "
     "mock id nor the test-venue marker)",
     ["programs/shortwin/src/venues/mock.rs", "crates/shortwin-venue-state/src/mock_cpamm.rs",
      "tests/mock_cpamm"]),
]

# Tracked files under a watched root that are deliberately in no unit: (pathspec, reason). They
# are listed as comments at the end of scope.txt. Anything else under a watched root that no unit
# names makes `write` and `check` fail.
EXCLUDED = [
    ("crates/shortwin-math/README.md", "documentation (the math derivations; read for context)"),
    ("crates/shortwin-math/tests", "tests"),
    ("crates/shortwin-math-wasm", "the WASM facade for the frontend; never linked on-chain"),
    ("services/README.md", "documentation"),
    ("services/deny.toml", "CI dependency policy (cargo deny), not built"),
    ("services/common/tests", "tests"),
    ("services/keeper/tests", "tests"),
    ("crates/squads-v4-ix/tests", "tests (the IDL pinning)"),
    ("services/ops/tests", "tests (T-OPS on the mainnet Squads ELF)"),
    ("services/keeper-litesvm", "keeper test harness"),
    ("services/indexer", "the indexer and API: off-chain, read-only (AUDIT.md §2 out of scope)"),
    ("services/indexer-litesvm", "indexer test harness"),
    ("tests/litesvm", "the LiteSVM test suites (root workspace member)"),
    ("tests/fuzz", "the invariant fuzz harness (root workspace member)"),
]

WATCHED_ROOTS = ["programs", "crates", "services"]


def workspace_members():
    """The member paths of the root and services Cargo workspaces, relative to ROOT."""
    members = []
    for manifest in ("Cargo.toml", "services/Cargo.toml"):
        with open(os.path.join(ROOT, manifest), "rb") as fh:
            ws = tomllib.load(fh).get("workspace", {})
        base = os.path.dirname(manifest)
        members += [os.path.normpath(os.path.join(base, m)) for m in ws.get("members", [])]
    return members


def classification_errors():
    """Watched files that are in no unit and not EXCLUDED, and files claimed twice."""
    claims = {}
    for unit, _cls, _desc, specs in UNITS:
        for path in ls_files(specs):
            claims.setdefault(path, []).append(unit)
    for spec, _reason in EXCLUDED:
        for path in ls_files([spec]):
            claims.setdefault(path, []).append(f"EXCLUDED {spec}")
    errors = [f"in no unit and not EXCLUDED: {path}"
              for path in ls_files(WATCHED_ROOTS + workspace_members()) if path not in claims]
    errors += [f"claimed {len(owners)} times ({', '.join(owners)}): {path}"
               for path, owners in sorted(claims.items()) if len(owners) > 1]
    return errors


HEADER = """\
# short.win audit scope (docs/AUDIT.md §2). Generated by `python3 scripts/audit_scope.py write`
# from `git ls-files` at the commit AUDIT.md §0 names; `python3 scripts/audit_scope.py check`
# fails when it differs. Sections: [core] the on-chain code, in scope; [keeper] the keeper core
# subset, in scope (the owner's decision of 2026-10-02); [context] off-chain code that is out of
# scope but read for context; [out] TEST-ONLY code, listed so that its exclusion is explicit.
# Every other tracked file under programs/, crates/, services/ and the Cargo workspace members
# matches a pathspec of the [excluded] list at the end.
"""


def ls_files(specs):
    out = subprocess.run(["git", "ls-files", "--", *specs], cwd=ROOT, check=True,
                         capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line]


def render():
    parts = [HEADER]
    for unit, cls, desc, specs in UNITS:
        parts.append(f"\n# [{cls}] {unit}: {desc}\n")
        parts.extend(f"{path}\n" for path in ls_files(specs))
    parts.append("\n# [excluded] tracked under a watched root, deliberately in no unit "
                 "(pathspec: reason)\n")
    parts.extend(f"# {spec}: {reason}\n" for spec, reason in EXCLUDED)
    return "".join(parts)


TEST_ATTR = re.compile(r"\s*#\[cfg\((all\()?test\b")
CHAR_LIT = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'")
RAW_STR = re.compile(r'b?r(#*)"')


def code_only(lines):
    """Each line with comments and the contents of string and char literals removed (quotes
    kept), so that braces, brackets and `;` inside them are not counted. Tracks block
    comments, multi-line strings and raw strings across lines; a lifetime `'a` is code."""
    out, state = [], None  # None | ("str",) | ("raw", hashes) | ("block", depth)
    for line in lines:
        buf, i, n = [], 0, len(line)
        while i < n:
            if state is not None and state[0] == "block":
                if line.startswith("/*", i):
                    state, i = ("block", state[1] + 1), i + 2
                elif line.startswith("*/", i):
                    state, i = (None if state[1] == 1 else ("block", state[1] - 1)), i + 2
                else:
                    i += 1
            elif state is not None and state[0] == "str":
                if line[i] == "\\":
                    i += 2
                elif line[i] == '"':
                    buf.append('"')
                    state, i = None, i + 1
                else:
                    i += 1
            elif state is not None and state[0] == "raw":
                close = '"' + "#" * state[1]
                if line.startswith(close, i):
                    buf.append('"')
                    state, i = None, i + len(close)
                else:
                    i += 1
            elif line.startswith("//", i):
                break
            elif line.startswith("/*", i):
                state, i = ("block", 1), i + 2
            elif line[i] == '"':
                buf.append('"')
                state, i = ("str",), i + 1
            elif line[i] == "'" and (m := CHAR_LIT.match(line, i)):
                buf.append("''")
                i = m.end()
            elif (line[i] in "br" and (i == 0 or not (line[i - 1].isalnum() or line[i - 1] == "_"))
                  and (m := RAW_STR.match(line, i))):
                buf.append('"')
                state, i = ("raw", len(m.group(1))), m.end()
            else:
                buf.append(line[i])
                i += 1
        out.append("".join(buf))
    return out


def skip_attributes(code, j):
    """Index of the first line after the outer attributes starting at `j` (an attribute may
    span lines: `#[allow(\\n a,\\n b,\\n)]`). `code` is `code_only(lines)`."""
    n = len(code)
    while j < n and code[j].strip().startswith("#["):
        depth = 0
        while j < n:
            depth += code[j].count("[") - code[j].count("]")
            j += 1
            if depth <= 0:
                break
    return j


def indent(line):
    return len(line) - len(line.lstrip())


def strip_tests(lines, path="<input>"):
    """Drop every `#[cfg(test)]` / `#[cfg(all(test, …))]` item: its attributes, then the item —
    up to its `;` when that comes before any `{` (a statement, a `use`, a `static … = …;` over
    several lines), else to the brace that closes its block (a `mod`, `fn` or `impl`). Braces
    and `;` are counted in `code_only` text. Warns on stderr when an item runs to the end of
    the file, or a braced item does not close on a line indented like its attribute (a sign
    the item boundary was misread)."""
    code = code_only(lines)
    out, i, n = [], 0, len(lines)
    while i < n:
        if not TEST_ATTR.match(code[i]):
            out.append(lines[i])
            i += 1
            continue
        k = skip_attributes(code, i + 1)
        depth, seen = 0, False
        while k < n:
            depth += code[k].count("{") - code[k].count("}")
            seen = seen or "{" in code[k]
            if (seen and depth <= 0) or (not seen and code[k].rstrip().endswith(";")):
                break
            k += 1
        if k >= n or (seen and indent(lines[k]) != indent(lines[i])):
            print(f"warning: {path}:{i + 1}: the #[cfg(test)] item does not close on a line "
                  f"indented like its attribute (ended at line {min(k, n - 1) + 1})",
                  file=sys.stderr)
        i = k + 1
    return out


def code_lines(lines):
    code, in_block = 0, False
    for line in lines:
        s = line.strip()
        if in_block:
            in_block = "*/" not in s
        elif not s or s.startswith("//"):
            pass
        elif s.startswith("/*"):
            in_block = "*/" not in s
        else:
            code += 1
    return code


def nloc():
    rows, totals = [], {}
    for unit, cls, _desc, specs in UNITS:
        files = [p for p in ls_files(specs) if p.endswith(".rs")]
        n = 0
        for path in files:
            with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
                n += code_lines(strip_tests(fh.read().split("\n"), path))
        rows.append((cls, unit, len(files), n))
        f, c = totals.get(cls, (0, 0))
        totals[cls] = (f + len(files), c + n)
    print(f"{'class':<7} {'unit':<22} {'.rs files':>9} {'non-test nLOC':>14}")
    for cls, unit, f, n in rows:
        print(f"{cls:<7} {unit:<22} {f:>9} {n:>14,}")
    for cls, (f, n) in totals.items():
        print(f"{cls:<7} {'total':<22} {f:>9} {n:>14,}")


def main(argv):
    cmd = argv[1] if len(argv) == 2 else ""
    if cmd in ("write", "check"):
        errors = classification_errors()
        for err in errors:
            print(f"error: {err}", file=sys.stderr)
        if errors:
            print("classify each file: add it to a unit (in scope) or to EXCLUDED (with the "
                  "reason) in scripts/audit_scope.py, then review the scope.txt diff",
                  file=sys.stderr)
            return 1
    if cmd == "write":
        with open(SCOPE, "w", encoding="utf-8") as fh:
            fh.write(render())
        print(f"wrote {os.path.relpath(SCOPE, ROOT)}")
    elif cmd == "check":
        want = render()
        try:
            with open(SCOPE, encoding="utf-8") as fh:
                have = fh.read()
        except FileNotFoundError:
            have = ""
        if have != want:
            print(f"{os.path.relpath(SCOPE, ROOT)} differs from git ls-files at HEAD: "
                  "run `python3 scripts/audit_scope.py write` and review the diff")
            return 1
        print("ok: scope.txt matches git ls-files at HEAD; every watched file is classified")
    elif cmd == "nloc":
        nloc()
    else:
        print(__doc__.strip().split("\n\n")[1], file=sys.stderr)
        return 64
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
