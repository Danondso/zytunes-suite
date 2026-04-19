# debug

Spawn research agents on a problem, aggregate their findings into `DEBUG.md`, then form ranked hypotheses with validation steps.

## When to use

When the user runs `/debug <topic>` or asks to investigate a bug, unexplained behavior, protocol oddity, or regression. Use this when the cause isn't obvious and the investigation spans multiple angles (code path, git history, protocol/spec knowledge, prior incidents).

Do **not** use this for trivial fixes, one-file bugs with an obvious cause, or tasks where the user already knows the answer.

## Instructions

Follow these steps in order.

### 1. Frame the problem — ask before researching

A vague problem statement produces vague research. Before dispatching any agents, interview the user until you can fill in every slot below with something concrete. Ask in **batches** (group related questions into one message using `AskUserQuestion` or a short numbered list) rather than one at a time. Keep going until the slots are filled — don't settle for "I'm not sure" on material ones without at least probing further.

Slots to fill:

- **Symptom** — what is observed (exact error text, stack trace, log line, wrong output, hang duration, crash signature). Ask for literal copy-paste, not a paraphrase.
- **Precipitant** — what triggered it. Which command? Which device? Which file? Which firmware? Which build? First occurrence or recurring? Reliably reproducible or flaky?
- **Environment** — host OS, cable/hub if USB is involved, cargo profile (debug vs release), env vars set (`IOKIT_USB_DEBUG`, `ZYTUNES_DUMP_ART`, `ZYTUNES_CACHE_DIR`), whether this is a worktree or main checkout.
- **Timeline / regression window** — when did it last work? What changed between then and now (commits, dependencies, firmware, device swap)?
- **Known context** — what has the user already tried, ruled out, or suspected? What's their current theory?
- **Scope boundaries** — what's explicitly *out* of scope for this investigation (e.g. DRM crypto, upstream terminal bugs, iPod vs Zune paths)?
- **Artefacts on hand** — logs, packet captures, `/tmp/zytunes-last-art.jpg`, ZMDB dumps, screenshots, a minimal repro file. If they exist, get paths.
- **Success criterion** — what answer would close this investigation? A root cause? A reliable repro? A workaround? Knowing this shapes which angles are worth researching.

Ask follow-ups whenever an answer is vague ("it hangs sometimes" → "how long? on which op code? is the pipe stalled or is the device unresponsive?"). A good framing pass is usually 2–4 rounds of questions, not one.

Only proceed to step 2 once the problem statement is tight enough that an agent could act on it without needing to come back for more context.

### 2. Pick the angles worth researching

Not every problem needs every angle. Pick 2–4 from:

- **Codebase** — where the code path lives, call sites, invariants, recent edits. Use `Explore` subagent.
- **Git history** — `git log -p`, `git blame`, commits touching the affected symbols. When a regression is suspected. Use `general-purpose` subagent with explicit commands.
- **Protocol / spec / external docs** — MTP op codes, IOKit error codes, firmware behavior, third-party library semantics. Use `general-purpose` subagent with `WebSearch`/`WebFetch`.
- **Prior art in this repo** — existing entries in `DEBUG.md`, memory files under `the local Claude memory directory`, and `todos.md`. Use `Explore` subagent.
- **Reproduction** — a minimal command or test that reliably triggers the symptom. Use `general-purpose` subagent if a programmatic repro is feasible.

Skip angles where the answer is already in hand.

### 3. Dispatch agents in parallel

Spawn the selected agents in a **single message with multiple `Agent` tool calls** so they run concurrently. Each prompt must be self-contained:

- The full problem statement from step 1 (agents don't see this conversation)
- The specific question this agent is answering — narrow, not "research X generally"
- The files/paths/symbols already known to be relevant
- A hard cap on response length (e.g., "under 300 words") so findings stay scannable
- Explicit instruction: **return findings only, do not edit files**

Do not have agents write to `DEBUG.md` themselves — aggregation happens in step 4 so sections stay coherent and deduplicated.

### 4. Aggregate findings into DEBUG.md

**Append** a new section to `DEBUG.md`; do not overwrite existing content. The file is a running log of investigations.

Format:

```markdown
---

## Investigation: <short problem title> — <YYYY-MM-DD>

**Symptom:** <one line>
**Precipitant:** <one line>
**Scope:** <what was in/out of research>

### Findings

#### <angle 1, e.g. "Codebase — SetObjectPropValue call sites">
<condensed findings, with file:line references>

#### <angle 2, e.g. "Git history — art-disabled flag">
<condensed findings>

...
```

Collapse redundant findings across agents. Prefer direct quotes of code/errors over paraphrase when precision matters. Always include `file:line` references so the user can navigate.

### 5. Form hypotheses

In the main conversation (not in a subagent), synthesize the findings into **ranked hypotheses**. Append to the same `DEBUG.md` section:

```markdown
### Hypotheses

1. **<most likely cause>** — <why this fits the evidence>
   - Supporting: <finding refs>
   - Against: <contradicting evidence, if any>
   - Confidence: <high/medium/low>

2. **<next>** — ...
```

Rules for hypothesis quality:

- Each hypothesis must be falsifiable — a specific mechanism, not "something with MTP is broken"
- Cite the findings that support and contradict it; don't handwave
- If the evidence points clearly to one cause, say so — don't manufacture alternatives for balance
- If the evidence is genuinely insufficient, say that too, and list what's missing rather than guessing

### 6. Propose validation steps

For the top 1–2 hypotheses, append concrete next actions:

```markdown
### Next steps

- [ ] <minimal experiment or check that would confirm/refute hypothesis 1>
- [ ] <...>
```

Prefer experiments that are fast, local, and reversible (a `cargo test` addition, a logged print, a single-file repro) over ones that require device time or destructive actions.

### 7. Summarize to the user

Print a short terminal summary:
- The top hypothesis and its confidence
- The 1–2 next steps
- A pointer to the `DEBUG.md` section for the full write-up

Do **not** start implementing a fix unless the user asks. This skill's output is a diagnosis, not a patch.

## Guardrails

- **No DRM crypto work.** If the investigation drifts toward DRM-bypass, wmdrm internals, or firmware signature defeat, stop and flag it — that scope is out per project policy.
- **Respect the project boundary.** Agents must not search outside `~/GitHub/zytunes` without asking. Pass this constraint in agent prompts.
- **Don't pad DEBUG.md.** If an angle turned up nothing useful, note it briefly (`no relevant history`) rather than filling space with null findings.
- **Memory is a source, not gospel.** Memory entries can be stale. If a memory claim contradicts current code, trust the code and flag the stale memory.
