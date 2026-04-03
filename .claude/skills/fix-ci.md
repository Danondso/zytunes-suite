# fix-ci

Diagnose and plan fixes for failing CI checks on the current branch's pull request.

## When to use

When the user runs `/fix-ci` or asks to fix CI failures, check failing checks, or diagnose pipeline errors.

## Instructions

Follow these steps in order:

### 1. Identify the PR for the current branch

Run `gh pr view --json number,title,url,headRefName` to find the PR associated with the current branch.

If no PR exists, tell the user and stop.

### 2. Fetch CI check status

Run `gh pr checks` to list all checks and their statuses.

If all checks pass, tell the user everything is green and stop.

### 3. Get details on failing checks

For each failing check:

1. Get the run ID from the failing check output
2. Run `gh run view <run-id>` to get the job summary
3. Run `gh run view <run-id> --log-failed` to pull the actual failure logs

Collect all failure logs before proceeding.

### 4. Analyze failures and correlate with local code

Read the relevant source files referenced in the failure logs. Understand what each failure is about:

- Build errors: identify the file and line
- Test failures: identify the test, the assertion, and the expected vs actual values
- Lint/format errors: identify the rule and location
- Other failures: capture the error message and context

### 5. Create a plan

Enter plan mode and create a structured plan to fix all failures. The plan should:

- Group related failures together
- Order fixes by dependency (e.g., build errors before test errors)
- For each fix, specify:
  - Which file(s) to change
  - What the change should be
  - Why it fixes the failure
- Note if any failure appears to be a flaky test or infrastructure issue rather than a code problem

Include steps in the plan to:
- Run `cargo fmt` and `cargo clippy -- -D warnings` after all fixes to catch formatting and lint issues before pushing
- Add regression tests for any bugs that were fixed — a test that would have caught the failure in CI. Focus on meaningful coverage: test behavior and outcomes, not implementation details. Skip test theatre (trivial assertions, UI rendering tests, tests that restate the implementation)

Present the plan to the user for approval before making any changes.
