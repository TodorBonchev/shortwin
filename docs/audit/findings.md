# short.win audit findings

This file tracks every finding from the external audit and records how each was resolved. It
belongs to the audit package ([`docs/AUDIT.md`](../AUDIT.md) §12). The M10 exit criterion is
"every finding fixed, or acknowledged with a rationale" (ARCH §9).

**Rules:**
- **One row per finding**, keyed by the firm's id. Keep the firm's severity, and add our own
  only if we disagree; give the reason in the finding's section below.
- **Fixed** means all of the following:
  - the fix commit is on the audit branch;
  - a regression test fails on the audited commit and passes on the fix (a LiteSVM test, a
    fuzz corpus file under `tests/fuzz/corpus/`, or a `shortwin-math` test);
  - the full gate (G-full) is green on the fix commit;
  - a ≥ 24 h fuzz run on the fix commit has 0 failures;
  - the auditors' fix review has accepted it.

  Program fixes go out in batches, so that hashes, CU baselines and fuzz runs are redone once
  per batch.

  *2026-10-13:* the owner waived the ≥ 24 h fuzz run for the AI review's fix batch (AIR-01 …
  AIR-24, [`ai-review/README.md`](ai-review/README.md)); no such run was made on its program.
  That batch still needs CI and an external fix review. The rule above is unchanged for the
  audit's findings.
- **Acknowledged** means not fixed, with a written rationale and the owner's sign-off. The
  finding then gets a row in [`known-issues.md`](known-issues.md), with its bound.
- **Duplicate of KI-n** means the finding restates a known issue within its stated bound.
  Answer it by pointing to the row. If the finding exceeds the bound, it is a new finding.
- **A fix that changes a bound** in `known-issues.md` or the design documents updates them in
  the same commit.

## Engagement

| Firm | Commit / tag audited | Start | End | Report | Fix review |
|---|---|---|---|---|---|
| the private audit's firm (booked by the owner) | `audit-1` / `v1.0.0`, `e4895c2` (AUDIT.md §0) | | | `docs/audit/<firm>-<date>.pdf` | |
| the public contest (Sherlock or Cantina) | the 1.0.x tag after the audit's fixes | | | | |

## Tracker

Status is one of: open, in progress, fixed, acknowledged, duplicate of KI-n, invalid.

| ID | Title | Firm severity | Our severity | Location | Status | Fix commit | Regression test | Fix review | Notes |
|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | |

## Findings

Copy this section once per finding.

### <ID>: <title>

- **Firm severity / ours:**
- **Location:** `path:line` at the audited commit
- **Summary:** what the auditors reported, in one paragraph.
- **Reproducer:** the auditors' steps, or our test that shows it.
- **Assessment:** do we agree? Is it in scope (AUDIT.md §2)? Does it match a known issue
  (KI-n)? What is the impact, in lamports or tokens where it can be bounded?
- **Resolution:** fixed / acknowledged. For a fix, describe the change and give the commit.
  For an acknowledgement, give the rationale and the owner's sign-off (name, date).
- **Regression:** the test name, and that it fails on the audited commit.
- **Gate:** the G-full run and the 24 h fuzz run on the fix commit (commit, duration,
  failures).
- **Fix review:** the auditors' verdict, with date.
