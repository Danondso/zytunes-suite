---
description: Pull CI checks for the current branch and triage failures into a categorized report
---

# triage-ci

Pull CI check status for the current branch's PR and triage every failure into a categorized report. This skill stops at triage — it does not fix.

## When to use

When the user runs `/triage-ci` or asks to triage CI failures, audit pipeline status, or get a categorized rundown of what's broken on the PR. Use this when the goal is *understand and classify*, not *fix* — for fixing, point them at `/fix-ci`.

## Instructions

Follow these steps in order. Run independent `gh` calls in parallel where possible.

### 1. Identify the PR for the current branch

Run `gh pr view --json number,title,url,headRefName,statusCheckRollup` to find the PR and pull the rollup in one call.

If no PR exists, tell the user and stop.

### 2. Inventory all checks

From the `statusCheckRollup` (or a follow-up `gh pr checks`), list every check with its status. Separate into:

- **Passing** — green, no action needed
- **Pending / In progress** — note these but don't block triage on them; flag for re-run later
- **Failing / Errored / Cancelled** — the triage targets
- **Skipped / Neutral** — note but generally informational

If nothing is failing, tell the user CI is green (or still pending) and stop.

### 3. Pull logs for each failing check

For every failing check, in parallel:

1. Extract the run ID from the rollup or `gh pr checks` output
2. Run `gh run view <run-id> --json jobs,conclusion,displayTitle` for the job summary
3. Run `gh run view <run-id> --log-failed` for the actual failure output

Cap log reads at the failing job's relevant section — don't pull whole-run logs unless `--log-failed` returns nothing useful.

### 4. Classify each failure

For every failing check, assign one of these categories. The category drives the recommendation, so be deliberate.

| Category | Signal |
|----------|--------|
| **Build error** | `cargo build` / `cargo check` fails — compile error, missing dep, type error |
| **Test failure** | A specific test assertion failed; reproducible from the diff |
| **Lint / format** | `cargo clippy -- -D warnings` or `cargo fmt --check` failed |
| **Flake** | Test passed on retry, timing-sensitive, network-dependent, or known-flaky test name |
| **Infra / runner** | Runner died, image pull failed, action version error, secret missing, GitHub outage |
| **External regression** | Failure is in code untouched by this PR — likely a `main` regression or upstream dep change |
| **Config drift** | Workflow file, action pin, or matrix setting changed and broke the job |
| **Unknown** | Can't classify with available info — note what's missing |

For each failure, capture:

- **Check name** and run URL
- **Category** (from the table)
- **Root cause** — one sentence, in plain language. For build/test/lint, cite `file:line` from the log
- **Touched by this PR?** — yes / no / unclear, based on whether the failing file appears in `git diff main...HEAD`
- **Severity** — blocking (must fix to merge) / non-blocking (flake or infra, retry-able) / advisory (lint cleanup)
- **Recommended action** — one of: *fix in this PR*, *retry the job*, *file separate issue*, *needs human investigation*

When a log references a source file, read the relevant lines locally to confirm the diagnosis (don't guess from the log alone). Run `git diff main...HEAD -- <file>` to check whether the PR actually touched it — this is what distinguishes a real regression from an external one.

### 5. Report

Output a single concise triage report. Format:

```
CI Triage — PR #<num> "<title>"
<url>

Summary: <N> failing, <M> pending, <K> passing

Failures
────────
1. <check name> — <category>, <severity>
   Root cause: <one-sentence diagnosis> (<file>:<line> if applicable)
   Touched by PR: <yes/no/unclear>
   Action: <recommendation>

2. ...

Pending (re-check after triage):
  - <check name>

Notes:
  - <anything cross-cutting: multiple failures share a root cause, infra-wide outage, etc.>
```

Keep it scannable. One block per failure, no prose padding. If two failures share a root cause, say so once and group them.

### 6. Stop

Do not enter plan mode. Do not edit files. Do not run `cargo fmt` or `cargo clippy`. If the user wants to act on the triage, they can run `/fix-ci` or ask directly.

The one exception: if a failure is clearly a flake or infra blip and re-running the job is the entire fix, offer to run `gh run rerun <run-id> --failed` and wait for confirmation before doing it.
