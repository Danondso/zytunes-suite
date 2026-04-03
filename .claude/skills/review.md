# review

Local code review of the current branch's diff against main, posted as PR comments, then fixed.

## When to use

When the user runs `/review` or asks for a code review, diff review, or PR review locally.

## Instructions

Follow these steps in order:

### 1. Get the diff

Determine the base branch by running `git merge-base main HEAD` to find the common ancestor.

Run `git diff $(git merge-base main HEAD)...HEAD` to get the full diff of changes on this branch.

If there are also uncommitted changes, run `git diff` and `git diff --cached` separately and include those in the review scope, clearly noting they are uncommitted.

If there is no diff (branch is identical to main and no uncommitted changes), tell the user and stop.

### 2. Read changed files in full

For every file that appears in the diff, read the complete current version of that file. Reviewing only the diff lines without surrounding context leads to shallow reviews — you need the full picture of how changes interact with existing code.

### 3. Review the changes

Evaluate the diff across the following categories. For each finding, reference the specific file and line number.

**Correctness**
- Logic errors, off-by-one mistakes, missing edge cases
- Incorrect error handling (swallowed errors, wrong error types, panics in library code)
- Potential runtime panics: unwrap/expect on fallible paths, unchecked indexing, integer overflow
- Soundness issues with unsafe blocks if present

**Rust best practices**
- Ownership and borrowing: unnecessary clones, redundant references, moves where borrows suffice
- Idiomatic patterns: prefer `if let` over match with one arm, use `?` over manual match on Result/Option, iterator chains over manual loops where clearer
- Type system usage: stringly-typed values that should be enums, missing From/Into impls, overly broad trait bounds
- Error handling: appropriate use of Result vs panic, error type granularity, context on errors (anyhow/thiserror patterns)
- Concurrency: data races, lock ordering, channel misuse, Send/Sync issues

**Cleanliness**
- Dead code: unused imports, unreachable branches, commented-out code, unused variables (beyond `_` prefixed)
- Naming: unclear abbreviations, misleading names, inconsistent conventions within the codebase
- Duplication: repeated logic that should be extracted, copy-paste with minor variations
- Module organization: public items that should be private, items in the wrong module

**Readability**
- Functions that are too long or do too many things
- Deeply nested control flow that could be flattened with early returns or guard clauses
- Complex expressions that would benefit from intermediate variables with descriptive names
- Missing or misleading comments on non-obvious logic (do NOT suggest adding comments to self-explanatory code)

**Refactoring opportunities**
- Suggest refactors only when they provide a clear, concrete improvement — not speculative "might be useful later" changes
- Structural improvements: breaking up large functions, extracting shared logic, simplifying state machines
- API surface improvements: better function signatures, more ergonomic builder patterns, reducing public surface area

**Security audit**
- Input validation at system boundaries (CLI args, file paths, USB data, parsed plists)
- Path traversal risks in file operations
- Unbounded allocations from untrusted input (e.g., MTP device reporting absurd sizes)
- Sensitive data handling (keys, device serial numbers in logs)
- Command injection if shelling out to external tools (ffmpeg, ffprobe)

### 4. Identify the PR

Run `gh pr view --json number,url` to get the PR number for the current branch.

If no PR exists, print the review summary to the terminal instead and ask the user if they want to proceed with fixes. Skip commenting steps.

### 5. Post findings as PR review comments

Use `gh api` to post a pull request review with file-level comments for each finding.

For each finding, create a review comment anchored to the relevant file and line using:

```
gh api repos/{owner}/{repo}/pulls/{pr}/reviews \
  --method POST \
  -f event=COMMENT \
  -f body="Review summary" \
  --jq '.id'
```

For individual line comments, use `gh api repos/{owner}/{repo}/pulls/{pr}/comments` with:
- `path` — the file path relative to the repo root
- `line` — the line number in the diff
- `side` — always `RIGHT`
- `body` — the finding description, prefixed with severity: `[Critical]`, `[Warning]`, or `[Suggestion]`

Group related findings on the same file into a single comment when they are within a few lines of each other.

### 6. Fix the issues

Address findings in priority order:

1. **Critical** issues first (bugs, security problems)
2. **Warnings** second (best practice violations, risky patterns)
3. **Suggestions** last (readability, refactoring) — only if the fix is clean and low-risk

For each fix:
- Make the code change
- Verify it compiles with `cargo check`

After all fixes are applied:
- Run `cargo fmt` to ensure consistent formatting
- Run `cargo clippy -- -D warnings` and fix any warnings
- Run `cargo test` to confirm nothing broke

Commit the fixes in logical groups (e.g., one commit for correctness fixes, one for cleanup). Do not lump all fixes into a single commit. If `cargo fmt` or `cargo clippy` produced additional changes, include those in the relevant commit rather than a separate formatting commit.

### 6b. Add test coverage

After fixing issues, evaluate whether the changed code has adequate test coverage. Add tests where they provide real value — not for the sake of coverage numbers.

**What to test:**
- New logic branches, especially edge cases that were just fixed
- Parsing and serialization (MTP containers, plists, object property lists)
- State transitions (device status, sync status, browse mode changes)
- Functions with non-trivial return values or side effects that can be observed

**What NOT to test:**
- Trivial getters/setters, Display impls, or simple delegation
- UI rendering (ratatui widget output) — these change constantly and break for cosmetic reasons
- Code that just calls through to an external tool (ffmpeg, gh) — mock boundaries, not internals
- One-liner functions where the test would be a restatement of the implementation

**Guidelines:**
- Prefer one focused test that exercises a real scenario over multiple micro-tests that each assert one field
- Test behavior, not implementation — assert on outcomes, not internal state
- A small amount of redundancy between tests is fine if it makes each test self-contained and readable
- If a bug was fixed, add a regression test that would have caught it
- Keep test setup minimal — if a test needs 20 lines of setup for 2 lines of assertion, the code under test may need a better API instead

### 7. Update PR comments with resolution status

After fixes are committed, go back to each PR comment posted in step 5 and reply to it with a resolution note using:

```
gh api repos/{owner}/{repo}/pulls/{pr}/comments/{comment_id}/replies \
  --method POST \
  -f body="Fixed in <commit-sha> — <brief description of what was changed>"
```

For findings that were intentionally skipped, reply explaining why (e.g., "Skipped — this is a known trade-off for X reason").

### 8. Final summary

Print a summary to the terminal:
- Total findings by severity
- How many were fixed, how many skipped and why
- Commits created
- Link to the PR
