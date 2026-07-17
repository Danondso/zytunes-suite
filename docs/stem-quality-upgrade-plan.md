# Plan: stem quality upgrade — Roformer cascade + vocal-harmony stems

Status: **implemented** (all four phases). Live-engine verification
against a real audio-separator install is still pending — see the
as-built notes below.
Builds on the shipped stem-split feature (`docs/stem-splitting-plan.md`, PR #91).

## 0. As-built deviations

The implementation departed from this plan in a few deliberate ways:

- **The cascade composes at the separator level, not the driver.** The
  plan had `background.rs::run_separation` iterating passes; instead
  `CascadeSeparator` implements the existing `StemSeparator` trait over
  a `&[RecipePass]` slice, so the worker driver, cancel token, event
  contract, and cache staging stayed untouched — same behavior, less
  churn.
- **Phase 1's "parity single-pass" never shipped as a mode.** It existed
  for one commit as scaffolding and was superseded in the same PR: the
  `hq` recipe IS the two-pass cascade, and `CascadeSeparator` is the
  only separator for audio-separator recipes.
- **A shared subprocess driver landed first** (`stems/process.rs`,
  `run_engine_process`): the planned "sub-task" turned out to be the
  right opening move, and `DemucsCli`, `AudioSeparatorCli`, and
  `provision::run_streaming` are now thin mappings over it.
- **Availability probe** is `--version` (the CLI's lightest no-op; it
  still imports the package, but runs on the background worker, never
  the UI thread).
- **Layout capacity** is `MAX_STEMS = 8` fixed-size gains with
  recipe-declared layouts, exactly as planned; `StemKind::ALL` and
  discriminant indexing were removed outright rather than kept as a
  superset order.
- **Still to verify against a live engine** (the plan's §6 items 1, 5,
  6): audio-separator's actual progress output format, the karaoke
  model's stem-name keys (assumed `"Vocals"`/`"Instrumental"`), and
  pass-2 listening quality. The stub-engine tests pin our side of the
  contract; a real `hq` run on a dev machine pins theirs.
- **Karaoke checkpoint A/B** (aufr33/viperx vs becruily) remains open —
  the aufr33/viperx model is pinned as the reference until listening
  tests run.

## 1. Why

`htdemucs_6s` was the right first engine — one model, six stems, a plain
`demucs` CLI — but it is also the end of its line:

- **Demucs is unmaintained.** The repo is archived; no new models are
  coming. The `_6s` guitar and piano sources shipped as experimental —
  guitar is passable, piano bleeds badly — and never improved.
- **The field moved to Roformer architectures.** BS-Roformer / Mel-Band
  Roformer checkpoints beat htdemucs on vocals by ~2–4 dB SDR depending
  on the test set — audibly less bleed and "watery" artifacting.
- **Harmony separation exists now.** Mel-Band Roformer *karaoke* models
  split a vocal stem into **lead** and **backing/harmony** vocals — a
  capability demucs never had.

The structural catch: every strong Roformer checkpoint is **2-stem**
(vocals/instrumental or lead/backing). No Roformer-class model produces
drums/bass/guitar/piano, and `htdemucs_6s` remains the only usable
6-source model anywhere. High quality therefore means a **cascade**:
Roformer for the vocal split, htdemucs_6s over the leftover instrumental
for the band.

## 2. Options surveyed

### 2.1 Engines (how we run models)

| Option | What it is | Verdict |
|---|---|---|
| **`python-audio-separator`** (PyPI `audio-separator`, MIT, v0.44.x) | Community-standard CLI wrapping UVR/MSST models: Roformer ckpts, MDX, VR, **and demucs models** under one binary | **Chosen.** Detail below |
| **ZFTurbo MSST** (`Music-Source-Separation-Training`) | The research framework most checkpoints are *trained* in; inference is `python inference.py --config X --start_check_point Y` from a git clone | Rejected as engine: not a pip package, config-file-per-model UX, no stable CLI contract. It is upstream of audio-separator, which wraps its models properly |
| **Native C++/GGML engines**: [`demucs.cpp`](https://github.com/sevagh/demucs.cpp), [`demucs.onnx`](https://github.com/sevagh/demucs.onnx), [`BSRoformer.cpp`](https://github.com/chenmozhijin/BSRoformer.cpp) | Torch-free single-binary inference. demucs.cpp runs htdemucs 4s/6s/ft from GGML weights (memory-lean, slower than torch); demucs.onnx is its faster ORT successor; BSRoformer.cpp (v0.1.0, June 2026) runs BS/Mel-Roformer GGUF conversions with CPU/CUDA/Vulkan and a plain CLI | **Deferred, deliberately kept open.** Together these could eventually cover the whole cascade with no Python and no 1.5 GB torch install — exactly what the reserved `[stems] provision = "bundled"` config value was parked for. Too early today: BSRoformer.cpp is weeks old with 2 releases, no macOS Metal backend, checkpoints need GGUF conversion, and quality parity is unverified. Revisit once it stabilizes; the `StemSeparator` trait means adopting it later costs one new impl |
| **MVSep cloud API** | Free-tier (queued) HTTP API over 100+ configs incl. ensembles | Rejected: uploads the user's library to a third party, queue latency, network dependency. zytunes is an offline-first device-sync tool; separation must stay local |
| **In-process inference in Rust** (`ort`/candle) | Run ONNX conversions inside zytunes | Rejected for this cycle: demucs.onnx proves the demucs path, but Roformer→ONNX is unproven and this is a research project, not a feature. The subprocess+cache architecture explicitly anticipated in-process backends later |

**Why audio-separator wins:** it is the only maintained engine that runs
*every* model we need (both Roformer passes **and** htdemucs_6s) behind
one entry point, and it slots into our provisioning story unchanged:

- Installs the way demucs does today: `uv tool install "audio-separator[cpu]"`.
  **Verified:** `torch>=2.3` is a *core* dependency (the `cpu`/`gpu`
  extras only select an onnxruntime flavor), so the CPU install needs the
  same `TORCH_CPU_INDEX` + `unsafe-best-match` pinning we already do for
  demucs — a known, solved problem, not an open question.
- One subprocess invocation per model pass:
  `audio-separator <input> -m <model> --output_dir <dir> --output_format flac`.
- `--custom_output_names '<json>'` maps each model stem to a filename we
  choose — every pass can emit our canonical `vocals.flac`, `drums.flac`,
  … layout directly (the demucs `expected_output_dir` dance disappears
  for this engine).
- `--model_file_dir` pins checkpoint downloads to a directory we control.
  The default is `/tmp/audio-separator-models/` — wiped on reboot, so we
  must point it at `~/.cache/zytunes/models/`.
- `--single_stem` can skip synthesizing stems a pass doesn't need.

Demucs stays supported: existing installs, `[stems] command` overrides,
and every current cache entry keep working unchanged. audio-separator is
*additive*.

### 2.2 Models (what we run)

**Vocal split (pass 1).** `model_bs_roformer_ep_317_sdr_12.9755.ckpt`
(viperx) — the engine's own default and the community consensus pick.
Newer community checkpoints (KimberleyJensen Mel-Band ~10.98, BS
PolarFormer ~11.0 on MSST's multisong scale — SDR scales differ across
test sets, never compare across tables) trade blows on vocals; the
ep_317 checkpoint stays the reference until listening tests say
otherwise. Swapping later is a one-string change plus a recipe version
bump.

**Lead/backing split (pass 3).** Two candidates, both in
audio-separator's registry:
- `mel_band_roformer_karaoke_aufr33_viperx_sdr_10.1956.ckpt` — the
  established one, recommended by the engine's author for full-spectrum
  output.
- `mel_band_roformer_karaoke_becruily.ckpt` — newer community model,
  reputed better on dense harmony arrangements.

Pick by A/B listening test during Phase 3 (an implementation checkpoint,
not a design question — the plumbing is identical).

**Band pass (pass 2).** `htdemucs_6s.yaml` — still the only model
anywhere that produces guitar and piano. The MSST checkpoint tables
confirm no Roformer/SCNet/MDX23C multi-stem alternative exists beyond
4-stem (vocals/drums/bass/other). Running it on a Roformer-cleaned
instrumental is the community-standard cascade and removes its worst
failure mode (vocal bleed into "other").

**Considered and rejected for now:**
- *Dedicated per-instrument Roformer models* (becruily guitar, MVSep
  piano) — real quality gains exist, but each is another full-track
  inference pass; a 5-pass recipe on CPU is minutes-per-track territory.
  The recipe architecture leaves the door open.
- *MedleyVox / iSRNet multi-singer separation* (per-voice stems for
  duets/unison) — research code with no packaged distribution or engine
  support; scientifically interesting, not shippable. The karaoke
  lead/backing split covers the practical 90%.
- *Ensembles* (averaging multiple models per stem, MVSep-style) —
  multiplies runtime for fractional SDR; wrong trade-off for a local TUI.

## 3. Recipes, not models

Today the unit of configuration and cache identity is a single model
string (`htdemucs_6s`). A cascade is inherently multi-model, so the plan
introduces a **recipe**: a named, versioned pipeline of engine passes
that produces an ordered list of stems.

| Recipe id (config value) | Passes | Stems | Engine |
|---|---|---|---|
| `demucs` *(default, unchanged)* | htdemucs_6s | Vocals, Drums, Bass, Guitar, Piano, Other | demucs CLI |
| `hq` | ① BS-Roformer (vocals/instrumental) ② htdemucs_6s over the instrumental | same six | audio-separator |
| `hq-harmony` | ①② as `hq`, ③ Mel-Roformer karaoke over the vocals | **Lead, Backing**, Drums, Bass, Guitar, Piano, Other (7) | audio-separator |

Selected via a new `[stems] recipe = "demucs" | "hq" | "hq-harmony"`
config key. The existing `model` key keeps meaning "the demucs model for
the `demucs` recipe" so current configs parse identically.

**Cache identity:** `StemCacheMeta.model` is already an opaque string
compared for equality. Recipes store a versioned id in that field
(`"htdemucs_6s"` stays as-is for the demucs recipe — existing entries
remain hits; `"hq/v1"`, `"hq-harmony/v1"` for the new ones, where `v1`
bumps if we ever swap a checkpoint so stale mixes re-separate). No
schema change, no migration.

## 4. Phases

### Phase 1 — engine abstraction + `AudioSeparatorCli` (parity, no new recipes)

Goal: a second `StemSeparator` implementation and the provisioning to
install it, proven by running the *existing* six-stem separation through
audio-separator's `htdemucs_6s.yaml`.

- `stems.rs`: add `AudioSeparatorCli { command: PathBuf }` with a
  `run_pass(input, model, out_dir, output_names, …)` building the argv
  above (including `--model_file_dir`). Reuses the existing subprocess
  scaffolding — `isolate_child_process`, `ChildGroupGuard`, the dual pipe
  drains, `parse_progress_across_chunks`, `stderr_tail` — which is
  engine-agnostic already.
  - **Sub-task (pays down existing debt):** extract that scaffolding into
    a shared `run_engine_process(cmd, hooks)` used by `DemucsCli`,
    `AudioSeparatorCli`, and `provision::run_streaming` — the third copy
    of this driver was already flagged during review as needing
    unification; adding a fourth is not acceptable.
- `stems/provision.rs`: `ENGINE_EXE` becomes per-engine
  (`demucs` / `audio-separator`); `find_engine`, `managed_engine_path`,
  and `build_install_command` take the engine's exe + package. The
  package spec is **version-pinned** (`audio-separator[cpu]==0.44.x`) so
  the argv/JSON contract can't drift under us. The uv flow — bootstrap,
  `TORCH_CPU_INDEX` + `unsafe-best-match` pinning (required: torch is a
  core dep), `UV_TOOL_BIN_DIR` — is shared verbatim.
- Availability probe: demucs uses `--help` because it's argparse-only and
  fast. Verify what audio-separator's cheapest no-op is (`-v`/`--version`
  vs `-h`) — a probe that imports torch would freeze the `M`-press for
  seconds. Implementation checkpoint.
- Consent overlay: wording gains the engine name and download-size
  estimate (audio-separator + CPU torch ≈ 1.5 GB, plus checkpoints
  200 MB–1 GB each on first separation — the checkpoints must be named
  in the consent text since they download outside the install step).
- Config: `[stems] recipe` key parsed (only `demucs` functional this
  phase). The explicit-binary override stays a single `command` key and
  applies to whichever engine the recipe needs.

**Exit criteria:** with `recipe = "hq"` temporarily hard-wired to a
single-pass `htdemucs_6s.yaml` invocation, a track separates end-to-end
through audio-separator into the same cache layout, cancel/quit teardown
included; with no config set, behavior is byte-identical to today.

### Phase 2 — the `hq` cascade

Goal: audibly better vocals with the UI, cache, keys, and mixer untouched.

- A `Recipe` description in `stems.rs`: ordered passes, each declaring
  `(model, input: Source | PriorStem(kind), output_names)`; the driver in
  `background.rs::run_separation` iterates passes inside the existing
  single job — one `gen`, one cancel token, one terminal event, staging
  every pass in the same `{key}.tmp` work dir under the cache root.
- Pass wiring for `hq`:
  1. BS-Roformer over the source → `vocals.flac` + `instrumental.flac`
     (work-dir only, never cached as-is).
  2. `htdemucs_6s.yaml` over `instrumental.flac` → drums/bass/guitar/
     piano/other. Its vocals output (near-silence by construction) is
     discarded via the output-name mapping or post-pass delete.
  3. `store_stems` the assembled six exactly as today.
- **Progress:** map pass-local 0–100 into a fixed per-pass window
  (pass 1 → 0–45, pass 2 → 45–100; weights tuned once we've timed real
  runs). The existing contract already tolerates sparse ticks, so if
  audio-separator's Roformer path logs no tqdm percentages the strip
  degrades to per-pass jumps — acceptable, but inspect its real output
  and extend the parser if it uses a different progress format. The
  `[stems] launching…` log line should name the active pass
  ("pass 1/2: vocals (BS-Roformer)…").
- **CPU cost is the headline caveat:** Roformer inference is
  transformer-heavy; community guidance treats CPU-only Roformer runs as
  multiple-minutes-per-track and effectively assumes GPU. `hq` is
  therefore opt-in, the consent/help text must set the expectation, and
  `[stems] gpu = true` matters far more here than for demucs. (The
  GGML/`BSRoformer.cpp` path in §2.1 is the long-term answer for CPU
  users.)

**Exit criteria:** `recipe = "hq"` produces six stems that toggle, scrub,
swap, cancel, cache-hit, and LRU-prune exactly like demucs stems;
`recipe` value round-trips the cache (switching recipes re-separates,
switching back hits the old entry only if it survived the prune).

### Phase 3 — `hq-harmony`: lead + backing vocal stems

Goal: seven stems — the headline feature. This is the only phase that
touches the six-stem assumption, which today funnels through
`NUM_STEMS`/`StemKind::ALL` into ~10 sites (StemSet/StemGains arrays, the
mixer's `[S; NUM_STEMS]`, `App.stems.enabled`, the strip renderer, the
`1`–`6` key claim).

- `StemKind` gains `LeadVocals` ("Lead"/"Ld ") and `BackingVocals`
  ("Back"/"Bck"); `StemKind::ALL` stays the *superset* order. A recipe
  declares its own ordered `layout: &'static [StemKind]` — `demucs`/`hq`
  list the current six, `hq-harmony` lists seven (Lead, Back, Drums,
  Bass, Guitar, Piano, Other). UI order, key digits, gain indices, and
  stem filenames all derive from the recipe layout, preserving the
  "one array drives everything, a re-order fails loudly" invariant.
- Fixed capacity, variable occupancy: introduce `MAX_STEMS = 8`;
  `StemGains` becomes `Arc<[AtomicU32; MAX_STEMS]>` (atomics stay
  lock-free and cheap; unused slots idle at 0.0), `enabled` likewise.
  `StemSet` carries `Vec<PathBuf>` + its layout (it already travels boxed
  through `BgEvent::StemsReady`, so size is irrelevant). The mixer takes
  `Vec<S>` with a runtime same-params check instead of `[S; NUM_STEMS]` —
  its inner loops already iterate `0..len`.
- Keys: the strip claims `1..=layout.len()` while Active + visible (the
  `stem_strip_visible` gate is count-agnostic). Key `7` has no global
  binding today, so no fall-through conflict; the hidden-strip
  fall-through test extends to it.
- Strip rendering: seven cells fit — the narrow-terminal label-dropping
  breakpoint (66 cols) gets re-measured for 7 cells and adjusted.
- Pass wiring: `hq` passes ① ②, then ③ the karaoke model over pass-①'s
  `vocals.flac` → `lead.flac` + `backing.flac`; the intermediate
  `vocals.flac` is dropped. Karaoke checkpoint chosen by A/B listening
  test (§2.2). Progress windows become 0–35 / 35–75 / 75–100 (tune
  against real timings).
- Cache: seven files + meta, id `"hq-harmony/v1"`. `cached_stems`
  validates existence against the recipe layout, not a hardcoded six.

**Exit criteria:** all existing six-stem tests pass unmodified for the
`demucs`/`hq` recipes; new tests cover 7-stem layout order, key `7`
claim/fall-through, mixer with 7 sources, cache roundtrip, and the
narrow-strip rendering.

### Phase 4 — docs + polish

- README + CLAUDE.md: recipe table, engine matrix, checkpoint sizes,
  CPU-vs-GPU guidance, cache-identity note.
- `THIRD_PARTY.md`: audio-separator (MIT) attribution; checkpoint
  licensing review — the community Roformer weights (viperx, aufr33,
  becruily HF repos) need their license terms recorded before we
  reference them from a released default. If a checkpoint's terms are
  unclear, `hq`/`hq-harmony` stay opt-in with the download consented,
  which they are by design anyway.
- Config reference for `[stems] recipe`; consent-overlay copy review.

## 5. Explicit non-goals

- **No in-process inference and no bundled native engine this cycle.**
  Subprocess + cache stays. The GGML path (§2.1) is the designated
  successor for `provision = "bundled"` when it matures.
- **No cloud separation.** MVSep's API is real but uploading the user's
  library is out of character for an offline device-sync tool.
- **No recipe editor / arbitrary user pipelines.** Three named recipes.
  The `Recipe` struct keeps the door open; the config surface does not.
- **No per-instrument model passes or ensembles** (§2.2) — runtime cost
  out of proportion on CPU.
- **No default flip.** `demucs` remains the default recipe this cycle —
  `hq` needs real-world soak (CPU timings, checkpoint host reliability)
  before it earns default status.
- **No re-separation prompts.** Switching recipes silently re-separates
  on next `M`; old entries age out via LRU.

## 6. Risks / open questions (resolve during implementation)

1. **audio-separator progress output format** — unknown whether Roformer
   passes emit parseable percentages; parser may need a second pattern.
2. **Cheap availability probe** — verify `--version` cost; a torch import
   at `M`-press would be a UX regression.
3. **Karaoke checkpoint A/B** — aufr33/viperx vs becruily, by listening
   test on dense-harmony material during Phase 3.
4. **Checkpoint hosting** — first-run downloads come via the engine from
   HF/GitHub; pin exact filenames and record fallbacks. The
   `--model_file_dir` cache must be exempted from `ZYTUNES_CACHE_DIR`
   like the stem cache (derived data, shareable across worktrees).
5. **Pass-2 bleed** — separating the band from a Roformer-cleaned
   instrumental is the community-standard cascade and generally improves
   those stems, but needs listening tests before we tout it.
6. **`custom_output_names` JSON shape** through `std::process::Command`
   is argv-safe (no shell), but verify the exact stem-name keys each
   model exposes against the pinned engine version.
7. **CPU runtime on real hardware** — time all three recipes on a
   representative laptop before writing the consent/help copy; if `hq`
   lands at >5 min/track CPU, say so in the overlay.

## 7. Testing strategy

- Arg-builder unit tests per engine/pass (the `build_demucs_args`
  precedent), including the output-name JSON.
- Recipe-driver tests with a `FakeSeparator` per pass: pass ordering,
  intermediate-file plumbing, per-pass progress windowing, cancel between
  and during passes, single terminal event per job (`gen` echo).
- Cache tests: recipe-id identity, 7-file layout validation, demucs
  entries surviving an engine upgrade.
- Mixer tests parameterized over 6 and 7 sources.
- TUI harness tests for key `7`, strip rendering at narrow widths, and
  recipe-labelled progress lines.
- Live-engine coverage stays manual (the demucs precedent): a scripted
  checklist against a real install for each recipe.
