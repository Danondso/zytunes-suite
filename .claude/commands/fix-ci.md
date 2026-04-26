---
description: Diagnose failing CI checks on the current branch's PR and plan fixes
---

Diagnose and plan fixes for failing CI checks on the current branch's pull request. Follow these steps in order.

## 1. Identify the PR

Run `gh pr view --json number,title,url,headRefName,statusCheckRollup` to find the PR for the current branch and pull the check rollup in one call.

If no PR exists, tell the user and stop.

## 2. Inventory checks

From the rollup, separate checks into passing, pending, and failing. If nothing is failing, tell the user CI is green (or still pending) and stop.

## 3. Pull failure logs

For every failing check, in parallel:

1. Extract the run ID from the rollup or `gh pr checks`
2. Run `gh run view <run-id> --json jobs,conclusion,displayTitle` for the job summary
3. Run `gh run view <run-id> --log-failed` for the actual failure output

## 4. Correlate with local code

For each failure, read the source files referenced in the log — don't diagnose from the log alone. Run `git diff main...HEAD -- <file>` when you need to know whether this PR actually touched the failing code (separates real regressions from pre-existing breakage on `main`).

Identify, for each failure:
- **Build error**: file, line, compile/type error
- **Test failure**: test name, assertion, expected vs. actual
- **Lint / format**: rule name, file, line
- **Other**: the error message and surrounding context

## 5. Plan

Enter plan mode and present a structured fix plan:

- Group related failures (one root cause → one plan entry)
- Order by dependency: build errors before test errors before lint
- For each fix: which file(s), what change, why it fixes the failure
- Flag suspected flakes or infra issues separately — don't propose code changes for those, propose `gh run rerun` or filing a separate issue
- Include final steps: run `cargo fmt` and `cargo clippy -- -D warnings` to catch lint/format before pushing
- Add a regression test for any real bug fixed — a test that would have caught this failure. Meaningful coverage only; skip test theatre (trivial assertions, tests that restate the implementation, UI rendering tests)

Present the plan and wait for approval before making any changes.
