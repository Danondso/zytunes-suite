# Research: stem-separation model & tooling updates (as of 2026-11-24)

Scope: what changed upstream since the stem quality-upgrade work
(`docs/stem-quality-upgrade-plan.md`) that zytunes could use for better
quality, speed, CPU viability, new stem types, or a smaller install.
Baseline: `AUDIO_SEPARATOR_VERSION = "0.47.0"`; recipes `demucs`, `hq`
(ep_317 → htdemucs_6s), `sw` (BS-Roformer-SW), `hq-harmony` (hq +
aufr33/viperx karaoke on the vocals).

> **Status (2026-11-25).** The research below is as written on 2026-11-24.
> What shipped differs from the original recommendations in a few places;
> §8 is the authoritative record of what each PR in the stack does. In
> short: `hq` was folded into `sw` (the config value still parses), the
> `--demucs_shifts` pin was added and then dropped once no
> audio-separator pass ran demucs, and `full` grew from ten stems to
> fourteen (crowd, male/female backing, separate hi-hat/ride/crash).

**How to read the numbers.** SDR is only comparable within one table or
benchmark. Unless marked otherwise, figures come from the **MVSep
Multisong** leaderboard (100 tracks × 60 s,
https://mvsep.com/quality_checker/multisong_leaderboard) or its sibling
task leaderboards. Those entries are **submitted by users** and checked by
MVSep's evaluator, so the score is measured but the inference settings
behind each entry are whatever the submitter says they were. audio-separator's own
`models-scores.json` uses a different test set that its README does not
document, so its numbers are kept apart.

---

## TL;DR: ranked recommendations

1. **Bump audio-separator 0.47.0 → 0.48.0. The argv contract is unchanged.**
   The 0.47.0…0.48.0 diff changes nothing in `audio_separator/utils/cli.py`.
   Only the version line in `pyproject.toml` moves
   (https://github.com/nomadkaraoke/python-audio-separator/compare/v0.47.0...v0.48.0).
   What we get:
   - **0.47.2 (security):** Demucs `.th` pickles now load through a restricted unpickler. VR/MDXC/RoFormer
     loads pass `weights_only=True` explicitly, and the RoFormer legacy fallback that retried
     without it is dropped
     (https://github.com/nomadkaraoke/python-audio-separator/commit/6d7d3fd60accda60c7ad8807ce5731022475dcf3).
     This matters for us because we download checkpoints at runtime and load them as pickles.
   - **0.48.0 (integrity):** models are fetched from audio-separator's own `model-configs` release first.
     Every file is checked against a shipped `model-checksums.json` (SHA-256 for 243 files).
     Downloads go to `{name}.{pid}.partial` and are renamed into place only when complete
     (https://github.com/nomadkaraoke/python-audio-separator/commit/4dd3477c0a8f5bfb0fb053c6ed444bb935cb150e).
   - **Follow-ups this needs on our side:**
     (a) `prune_model_cache` only retires `.ckpt` files, so it never deletes
     `*.partial`. Upstream removes a partial in a `finally`, but our cancel
     kills the whole process group, and SIGKILL skips `finally`. An abandoned
     0.7–1.7 GB partial would therefore leak forever. Teach the sweep to drop
     stale `*.partial` files.
     (b) Cached files are re-hashed "once per process"
     (same commit). Every recipe pass is a new process, so each pass re-hashes
     its 0.2–1.7 GB checkpoint. That cost is real but **not measured**. Time it.
   - No recipe `cache_id` bump is needed. The checkpoints are unchanged.

2. **Fix a CPU slowdown we are already shipping (0.47.0 changed overlap semantics).**
   - 0.47.0 (#299) made RoFormer overlap mean "number of overlapping windows"
     (`step = chunk_size // overlap`), taken from the YAML `inference.num_overlap`.
     Before, it was a fixed ~8 s step, which meant almost no overlap
     (https://github.com/nomadkaraoke/python-audio-separator/commit/1eaa08898629b131485acceeebc53ba93528d6d2).
   - Upstream's own note says karaoke models with `num_overlap` 4/8 "run 4–6× more forward
     passes per track than in 0.46.x"
     (README "Performance note (since 0.47.0)", https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md;
     https://github.com/nomadkaraoke/python-audio-separator/commit/465b60f0271e880a7289a2c9aaeb13817c3b2bed).
   - Our pinned YAMLs: ep_317 `num_overlap: 4`, aufr33/viperx karaoke `num_overlap: 4`,
     BS-Roformer-SW `num_overlap: 2` (model-configs release YAMLs,
     https://github.com/nomadkaraoke/python-audio-separator/releases/tag/model-configs).
     So **`hq` and `hq-harmony` got markedly slower on CPU when we pinned 0.47.0.**
   - Upstream's suggested escape is `--mdxc_overlap=2` (same README note). **Recommendation:** add
     `--mdxc_overlap 2` to Roformer passes by default. Expose it, for example as `[stems] overlap`, so GPU
     users can raise it.
   - Quality cost looks negligible, from community entries on the same scale:
     - Kim Mel-RoFormer vocals: 11.0742 at overlap 2 vs 11.0655 at overlap 4 vs 11.0304 at overlap 8.
     - BS-Roformer-SW vocals: 11.2666 at "fp16, overlap 2" vs 11.3019 at default.

     (Sources: https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=Kim&sort=vocals&ensemble=0,
     https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=SW&sort=vocals&ensemble=0.)
     These are single community submissions, not a controlled sweep.
   - **Also pass `--demucs_shifts 1`** on the htdemucs_6s pass. audio-separator's
     Demucs default is `shifts: 2` (README `demucs_params` default), which
     means two full inference passes. *Unverified here:* that the plain
     demucs CLI defaults to `shifts=1`. Check before relying on it for
     "parity with the `demucs` recipe".

3. **Rethink `hq`: on every published Multisong metric, `sw` beats it in fewer passes.**

   | Stem (Multisong SDR) | ep_317 (hq pass 1) | htdemucs_6s | BS-Roformer-SW (6 stems) |
   |---|---|---|---|
   | vocals | 10.96 | 8.18 | **11.30** |
   | bass | — | 11.42 | **14.62** |
   | drums | — | 10.59 | **14.11** |
   | other | — | 2.64 | **8.72** |

   Sources: ep_317 entry 7088; htdemucs_6s entry 296; SW entry 8374
   (https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=viperx&sort=vocals&ensemble=0,
   https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=6s&sort=vocals&ensemble=0,
   https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=SW&sort=vocals&ensemble=0).

   On the MVSep **guitar** and **piano** leaderboards, SW sits near the top among models you can download:
   - guitar 9.01, entry 9245;
   - piano 7.80, entry 9578.

   These are level with Logic Pro's built-in splitter (9.00 / 7.79)
   (https://mvsep.com/quality_checker/leaderboard/guitar/?sort=guitar,
   https://mvsep.com/quality_checker/leaderboard/piano/?sort=piano).

   Caveat: `hq` runs htdemucs_6s on a Roformer-cleaned instrumental. No published score covers
   exactly that cascade, so the htdemucs column understates `hq` somewhat. The margin on bass and drums
   (+3 dB) is too large for that to close, though.

   **Recommendation:**
   - Make `hq` = SW single pass, or keep the name and alias it.
   - Re-base `hq-harmony` on **SW → karaoke over SW's vocals**: 2 passes instead of 3, with no
     htdemucs in the high-quality path.
   - Each change bumps the recipe (`hq/v2`, `hq-harmony/v2`).
   - Bonus: this drops htdemucs_6s from the HQ path. Its weight licence is doubtful (see §5).

4. **Swap the karaoke model to a BS-Roformer karaoke checkpoint, but A/B it on vocals-only input first.**
   - **The open aufr33/viperx vs becruily-Mel question is a wash on SDR**, in our exact usage
     ("extract from vocals"):
     - aufr33/viperx: lead 8.8959, back 4.7299 (entry 8211);
     - becruily Mel: lead 8.8817, back 4.7292 (entry 8209).

     The becruily Mel checkpoint is also **1.72 GB** against 913 MB
     (https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead&algo_name_filter=becruily;
     sizes from https://github.com/nomadkaraoke/python-audio-separator/releases/tag/model-configs).
     **Close that A/B as "no".**
   - The better candidates are BS-Roformer karaoke models. They are 4.5× smaller (204 MB) and score
     +0.7–0.9 dB lead SDR on the full-mix test:

     | Model | registry name | lead SDR | aufr33/viperx, same test | licence |
     |---|---|---|---|---|
     | frazer & becruily | `bs_roformer_karaoke_frazer_becruily.ckpt` | 10.06–10.10 | 9.33 | none stated |
     | anvuew | `bs_roformer_karaoke_anvuew.ckpt` | 10.16–10.23 | 9.33 | GPL-3.0 |

     Sources: https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead&algo_name_filter=anvuew,
     https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead&algo_name_filter=aufr33,
     https://huggingface.co/anvuew/karaoke_bs_roformer.
   - **Unknown:** neither BS model has an "extract from vocals" entry, so the gain on our input is
     unmeasured. It is a listening test, not a done deal.
   - Default pick: **frazer-becruily** (no GPL question). Both are in audio-separator's registry
     (https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/models.json).
   - The swap bumps `hq-harmony/v2`.
   - Both YAMLs use `Vocals`/`Instrumental` stem names, so our `output_names` map is unchanged.

5. **Optional vocal-pass upgrade, if a two-pass vocal-first recipe survives (3).**
   - `bs_roformer_vocals_resurrection_unwa.ckpt` (in the registry, 205 MB vs ep_317's 639 MB) scores 11.34
     vocals against ep_317's 10.96
     (https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=Resurrection&sort=vocals&ensemble=0).
   - **Gotcha:** its YAML stems are `vocals`/`other`, not `Vocals`/`Instrumental`
     (`config_bs_roformer_vocals_resurrection_unwa.yaml`). The pass's `output_names` must map
     `other → instrumental`, or `CascadeSeparator` raises `MissingOutput`. Matching is
     case-insensitive (`get_stem_output_path`,
     https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/separator/common_separator.py).
   - unwa's newer **BS-Roformer Leap Xe** scores 11.76–11.79
     (https://mvsep.com/quality_checker/multisong_leaderboard?algo_name_filter=Leap&sort=vocals&ensemble=0).
     It is **not** in audio-separator's registry, and its weights carry no licence
     (https://huggingface.co/pcunwa/BS-Roformer-Leap).

6. **Pin demucs; it is no longer "archived upstream".**
   - `adefossez/demucs` is now "the officially maintained Demucs" (README,
     https://github.com/adefossez/demucs). PyPI `demucs` 4.1.0 shipped 2026-07-11
     (https://pypi.org/pypi/demucs/json).
   - Only `facebookresearch/demucs` is archived (GitHub API `archived: true`).
   - Pin `demucs==4.1.0` the way we pin audio-separator, so the argv contract can't drift.
   - Update the stale claims in `docs/stem-quality-upgrade-plan.md` §1 and in `stems/provision.rs`.

7. **New stem types available today.** *(Shipped: DrumSep in `full`,
   with `MAX_STEMS` raised to 16 and a grouped strip — see §8.)*
   - `MDX23C-DrumSep-aufr33-jarredou.ckpt` (registry, 438 MB) splits drums into kick/snare/toms/hh/ride/crash.
   - DrumSep5 leaderboard: kick 14.53, snare 9.78, toms 10.59
     (https://mvsep.com/quality_checker/leaderboard/drumsep5/?sort=kick).
   - Six sub-stems plus the five non-drum band stems exceeds `MAX_STEMS = 8`. A "drum detail" view would
     need a nested layout or a cap raise.
   - The stronger public jarredou 5-stem MDX23C (kick 16.66) is not in the registry.

---

## 1. python-audio-separator after 0.47.0

| Version | Date | Change | Impact on zytunes |
|---|---|---|---|
| 0.47.1 | 2026-10-19 | Tests and README note pinning the RoFormer forward-pass budget; documents the 0.47 overlap slowdown and `--mdxc_overlap=2` ([465b60f](https://github.com/nomadkaraoke/python-audio-separator/commit/465b60f0271e880a7289a2c9aaeb13817c3b2bed)) | Confirms TL;DR #2. Another user (karaoke-gen) saw 25→75 s and 26→149 s per track on an L4 GPU and pinned 0.44.2 |
| 0.47.2 | 2026-10-29 | Restricted unpickler for Demucs; `weights_only=True` everywhere ([6d7d3fd](https://github.com/nomadkaraoke/python-audio-separator/commit/6d7d3fd60accda60c7ad8807ce5731022475dcf3)) | Security fix for runtime-downloaded pickles |
| 0.48.0 | 2026-10-29 | Mirror-first download, SHA-256 verification, `.partial` + rename ([4dd3477](https://github.com/nomadkaraoke/python-audio-separator/commit/4dd3477c0a8f5bfb0fb053c6ed444bb935cb150e)) | Integrity; `.partial` cleanup needed (TL;DR #1) |

Release dates from https://api.github.com/repos/nomadkaraoke/python-audio-separator/releases.
GitHub release bodies are empty; the changes above come from the commits. PyPI's latest is 0.48.0
(https://pypi.org/pypi/audio-separator/json).

**argv contract (`build_audio_separator_args`).** We pass `-m`, `--output_dir`, `--output_format`,
`--model_file_dir` and `--custom_output_names`.
- All of these are still in the README usage synopsis.
- 0.47.0→0.48.0 touches no CLI code (compare link above).
- The last CLI change was in 0.47.0 itself: the `--mdxc_overlap`/`--mdxc_batch_size` defaults became `None`,
  meaning "use the model YAML". That change was already in what we pinned.

**Acceleration features already in our pin (0.45–0.46).**
- 0.46.0 added opt-in `--use_autocast`, `--use_native_fp16` and `--use_torch_compile`, with RoFormer/MPS optimizations.
- It also moved to a PyTorch 2.13 baseline (`torch>=2.13` on macOS arm64; wheels target macOS 14+)
  ([7a1c06f](https://github.com/nomadkaraoke/python-audio-separator/commit/7a1c06f), pyproject at
  https://github.com/nomadkaraoke/python-audio-separator/blob/main/pyproject.toml).
- Verified combinations (README, "Inference precision and compilation"):
  - **CPU + Mel/BS-RoFormer:** fp32 or autocast, and regional `torch.compile`.
  - **native fp16:** MPS/CUDA only.
- `--use_directml` was exposed on the CLI in 0.44.3 ([ee1fcee](https://github.com/nomadkaraoke/python-audio-separator/commit/ee1fcee)).
- We pass none of these. Candidates for a CPU speed experiment are `--use_autocast` and
  `--use_torch_compile` (compile cost is paid again by every new process). **No CPU speed numbers are
  published**, so measure before adopting.

**Chunking knobs relevant to CPU.**
- `--mdxc_overlap` (2–50).
- `--mdxc_segment_size` together with `--mdxc_override_model_segment_size`.
- `--mdxc_batch_size`.
- `--chunk_duration`, which splits long files into fixed chunks to cap memory, merged "with simple
  concatenation" (README "Processing Large Files"). Seams aren't crossfaded, so it is not a speed knob for us.

## 2. Checkpoints newer or better than what we pin

### Candidate table

| Name | Arch | Stems | SDR (benchmark) | audio-separator registry filename | Licence | Size | Source |
|---|---|---|---|---|---|---|---|
| BS-Roformer ep_317 *(pinned)* | BS-RoFormer | vocals/instrumental | 10.96 vocals (MVSep Multisong #7088) | `model_bs_roformer_ep_317_sdr_12.9755.ckpt` | not stated (couldn't verify) | 639 MB | https://github.com/TRvlvr/model_repo/releases/tag/all_public_uvr_models |
| BS-Roformer-SW *(pinned)* | BS-RoFormer | bass/drums/other/vocals/guitar/piano | vocals 11.30, bass 14.62, drums 14.11, other 8.72 (#8374); guitar 9.01; piano 7.80 | `BS-Roformer-SW.ckpt` | **unknown**: original `jarredou/BS-ROFO-SW-Fixed` HF repo is gone (HF lists no jarredou models); mirror says `license: unknown` | 699 MB | https://huggingface.co/enerjazzer/BS-ROFO-SW-Fixed |
| htdemucs_6s *(pinned)* | HT-Demucs | 6 | vocals 8.18, bass 11.42, drums 10.59 (#296) | `htdemucs_6s.yaml` | weights "likely CC-NC-BY" per the maintainer | ~ | https://github.com/adefossez/demucs |
| Mel karaoke aufr33/viperx *(pinned)* | Mel-RoFormer | lead/rest | lead 9.33 (full mix), **8.90 / back 4.73 (from vocals)** | `mel_band_roformer_karaoke_aufr33_viperx_sdr_10.1956.ckpt` | not stated | 913 MB | MVSep lead/back board |
| Mel karaoke becruily | Mel-RoFormer | lead/rest | lead 9.61 (full mix), **8.88 / back 4.73 (from vocals)** | `mel_band_roformer_karaoke_becruily.ckpt` | none stated | 1719 MB | https://huggingface.co/becruily/mel-band-roformer-karaoke |
| **BS karaoke frazer & becruily** | BS-RoFormer | lead/rest | lead 10.06–10.10 (full mix) | `bs_roformer_karaoke_frazer_becruily.ckpt` | none stated | 204 MB | https://huggingface.co/becruily/bs-roformer-karaoke |
| **BS karaoke anvuew** | BS-RoFormer | lead/rest | lead 10.16–10.23 (full mix) | `bs_roformer_karaoke_anvuew.ckpt` | **GPL-3.0** | 204 MB | https://huggingface.co/anvuew/karaoke_bs_roformer |
| Mel karaoke Gabox v1 / v2 | Mel-RoFormer | lead/rest | 9.77 / 9.77 (full mix) | `mel_band_roformer_karaoke_gabox.ckpt` / `_gabox_v2.ckpt` | none stated | 913 MB | https://huggingface.co/GaboxR67/MelBandRoformers |
| BS Resurrection vocals (unwa) | BS-RoFormer | vocals/other | 11.34 | `bs_roformer_vocals_resurrection_unwa.ckpt` | none stated | 205 MB | https://huggingface.co/pcunwa/BS-Roformer-Resurrection |
| BS Leap Xe vocals (unwa) | BS-RoFormer | vocals/other | 11.76–11.79 | **not in registry** | none stated | 268 MB | https://huggingface.co/pcunwa/BS-Roformer-Leap |
| becruily "deux" | Mel-RoFormer | vocals/instrumental | 11.37 | **not in registry** | not checked | ? | MVSep #9482 |
| Gabox vocals FV7 | Mel-RoFormer | vocals/instrumental | 11.17 (`mbr_vocalsfv7_gabox`) | `mel_band_roformer_vocals_fv7b_gabox.ckpt` (FV7**b**, not the scored FV7) | none stated | 913 MB | MVSep gabox filter |
| Kim Mel-RoFormer | Mel-RoFormer | vocals/other | 11.07–11.09 | `vocals_mel_band_roformer.ckpt` | **MIT** | 913 MB | https://huggingface.co/KimberleyJSN/melbandroformer |
| DrumSep aufr33/jarredou | MDX23C | kick/snare/toms/hh/ride/crash | kick 14.53, snare 9.78, toms 10.59, hh 3.18 (DrumSep5 #10712) | `MDX23C-DrumSep-aufr33-jarredou.ckpt` | not stated | 438 MB | https://github.com/jarredou/models/releases |

Notes:
- **SDR sources.**
  - MVSep Multisong (vocals) entries: https://mvsep.com/quality_checker/multisong_leaderboard (filters as linked in the TL;DR).
  - Karaoke entries: https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead.
  - DrumSep: https://mvsep.com/quality_checker/leaderboard/drumsep5/?sort=kick.
- **Registry and sizes:** `models.json` (https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/models.json)
  and the `model-configs` release assets. ep_317, SW and htdemucs_6s resolve through UVR's
  `download_checks.json` / audio-separator's release; all appear in `model-checksums.json`.
- **"Full mix" vs "from vocals".** Karaoke models are scored on the full mix by default. Our
  `hq-harmony` feeds them the isolated vocal. Only aufr33/viperx and becruily-Mel have "from vocals"
  entries, and those score about 0.4–0.7 dB lower than the same models on the full mix.
- **audio-separator's own `models-scores.json`** (unknown test set) agrees on direction. ep_317 vocals
  11.77; Kim 12.60; karaoke aufr33 8.45 vs Gabox 8.69. It has **no** entries for SW,
  becruily/frazer/anvuew karaoke, or Resurrection
  (https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/models-scores.json).

### Not usable yet (best published, no public weights)
The Multisong vocals board is topped by MVSep-hosted closed models:
- "BS Roformer 124 bands (2026.07.10)", 12.33;
- MVSep SubRoformer, 12.23;
- BS PolarFormer 124 bands, 12.02.

For lead/back, the top entry is "Mel-RoFormer (Karaoke / Duet)" at 11.08 lead.
(https://mvsep.com/quality_checker/multisong_leaderboard?sort=vocals,
https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead.)
These are reachable only through MVSep's cloud, which was rejected on privacy grounds in the plan doc.

## 3. New architectures and papers (2025–2026)

- **SubRoformer** (arXiv 2611.08130, Nov 2026): subband time-domain representation plus a residual RoFormer.
  - Paper claims: 10.63 dB average on MUSDB18-HQ with target-specific models (MUSDB-only training);
    11.59 dB with a single 4-stem model plus 500 extra songs (https://arxiv.org/abs/2611.08130).
  - MSST added it in v1.0.23. On Multisong, the public base checkpoint gets vocals 11.09, bass 11.38,
    drums 11.93, other 7.39
    (https://github.com/ZFTurbo/Music-Source-Separation-Training/releases/tag/v1.0.23).
  - Code is MIT (https://github.com/ArchiMickey/SubRoformer). Weights ship as safetensors/fp16, plus
    **CoreML and GGUF (q8_0, 117 MB)** builds (https://huggingface.co/archimickey/SubRoformer).
  - **No licence on the weights**, and 4-stem only (no guitar/piano). Not in audio-separator's registry.
  - On the published Multisong numbers it is worse than SW for our purposes.
- **BS PolarFormer** (PoPE positional embeddings, arXiv 2509.10534): MSST v1.0.20; public float16
  checkpoint at 11.00 vocals on Multisong
  (https://github.com/ZFTurbo/Music-Source-Separation-Training/releases/tag/v1.0.20,
  https://github.com/ZFTurbo/Music-Source-Separation-Training/blob/main/docs/pretrained_models.md).
  It does not beat ep_317/SW enough to matter, and isn't in the registry.
- **53-stem BS-RoFormer** (MSST v1.0.21, Apr 2026): includes lead-vocal/back-vocal, guitar variants,
  keys, strings and drum parts.
  - The author recommends ≥16 GB VRAM and says per-stem quality trails specialised models
    (https://github.com/ZFTurbo/Music-Source-Separation-Training/releases/tag/v1.0.21).
  - On MVSep: guitar 8.25, piano 6.85, both below SW. Not viable on CPU.
- **SCNet family** (XL IHF: 9.92 Multisong avg, MUSDB-trained; MIT code at
  https://github.com/starrytong/SCNet, last push 2025-09). 4 stems only; superseded for our needs
  (MSST pretrained table, link above).
- **DTTNet** (MSST v1.0.22): a lightweight architecture, but weak (vocals 8.79 Multisong)
  (https://github.com/ZFTurbo/Music-Source-Separation-Training/releases/tag/v1.0.22).

**Conclusion:** no successor with public, licensed weights beats BS-Roformer-SW for a 6-stem split.
The structural claim in the quality-upgrade plan, that "htdemucs_6s remains the only usable 6-source
model", **is now false**: SW is a 6-source Roformer, and it is already shipped as the `sw` recipe.

## 4. Speed and CPU

- **Biggest lever: overlap.** See TL;DR #2. It is the only CPU-relevant change with upstream-documented
  multipliers (4–6× forward passes on karaoke).
- **Pass count.** Re-basing `hq`/`hq-harmony` on SW (TL;DR #3) removes the htdemucs pass. Running it
  through audio-separator costs two shifts by default.
- **Smaller checkpoints.** The BS karaoke models are 204 MB against 913 MB for aufr33's Mel model.
  - On-disk size is not inference cost. The frazer-becruily YAML (dim 256, depth 12, chunk 882000,
    batch 2) is not obviously cheaper per second than aufr33's Mel model (dim 384, depth 6; config at
    https://huggingface.co/becruily/bs-roformer-karaoke and the model-configs release).
  - **CPU timing is unmeasured.** Benchmark both on a dev box.
- **audio-separator's own speed figures.** `models-scores.json` has a `seconds_per_minute_m3` field
  (Apple M3):
  - ~9 s per minute of audio for small Mel/BS models (e.g. `mel_band_roformer_karaoke_gabox.ckpt` 8.8);
  - ~150–190 s for big ones (becruily vocals 190.8, `bs_roformer_vocals_gabox` 154.8);
  - no entry for our pinned models.

  Device (MPS vs CPU) and settings are undocumented, so these only rank models against each other.
- **fp16/ONNX exports.** Third-party SW conversions exist:
  - ONNX fp16 (336 MB), fixed 4 s chunks, with STFT/iSTFT done outside the graph
    (https://huggingface.co/elicwhite/bs-roformer-sw-6stem-onnx);
  - GGUF F16 (https://huggingface.co/studio409/BSRoformer-SW-6Stem-GGUF);
  - MLX and CoreML repackagings (HF search "BS-Roformer-SW").

  On MVSep, "Satolep SW-MLX O2 fp16" scores 11.27 vocals / 8.97 guitar, the same as the torch model at
  overlap 2. That suggests the conversions preserve quality. These repackagings claim MIT, but the
  upstream licence is unknown (§5).

## 5. Licensing (MIT app; weights downloaded by the user at runtime, not redistributed)

| Weights | Stated licence | Evidence |
|---|---|---|
| audio-separator (code) | MIT | pyproject `license = "MIT"` |
| MSST (code), SubRoformer (code), demucs (code), BSRoformer.cpp, demucs.cpp/onnx | MIT | GitHub licence metadata |
| htdemucs_6s | **"license of the weights is not clear … htdemucs 6-source … likely CC-NC-BY"** | https://github.com/adefossez/demucs README "License" |
| Kim Mel-RoFormer | MIT | HF card `license: mit` |
| anvuew BS karaoke | **GPL-3.0** | https://huggingface.co/anvuew/karaoke_bs_roformer |
| BS-Roformer-SW | **unknown** (original repo gone; mirror says `unknown`) | https://huggingface.co/enerjazzer/BS-ROFO-SW-Fixed |
| becruily (all), Gabox, unwa (Resurrection, Leap), SubRoformer weights | **no licence stated** (HF cards carry no `license:` field / no README) | HF API `cardData` |
| ep_317, aufr33/viperx karaoke | not found; **couldn't verify** | — |

Practical reading, as engineering judgement rather than legal advice:
- zytunes never ships weights; audio-separator downloads them on the user's machine after consent. That
  keeps the MIT codebase clean.
- The consent overlay could still name the model authors and state the "no licence / non-commercial"
  status.
- Prefer stated-licence options where quality ties: frazer-becruily over anvuew (GPL), and Kim (MIT)
  where its quality suffices.

## 6. Alternative engines

- **BSRoformer.cpp** (GGML; BS- and Mel-RoFormer; CPU/CUDA/Vulkan; MIT): its only releases are v0.01
  (2026-01) and v0.1.0 (2026-06-16), with **no commits since 2026-06-16**
  (https://github.com/chenmozhijin/BSRoformer.cpp). A SW GGUF now exists for it (studio409, above).
  - Still no Metal backend. The plan doc's "deferred" verdict stands. The SW GGUF makes a torch-free
    `sw` recipe possible in principle, so it is worth a spike once the project shows life again.
- **demucs.cpp**: no release since v0.0.4-alpha (2024-05); last commit is a docs tweak on 2026-11-06.
  **demucs.onnx**: last push 2026-02-08, no releases (GitHub API). Neither moved meaningfully.
- **In-process Rust / `ort`.**
  - SW ONNX exists, with a fixed T=345 (4 s) chunk and STFT outside the graph
    (elicwhite card). A Rust host would need STFT/iSTFT (rustfft) and overlap-add, which is feasible.
  - The licence question in §5 applies to bundling or auto-fetching it ourselves. In that case
    *we* would choose the download, not audio-separator.
  - Per-chunk attention cost is O(T²) per that card.
- **UVR GUI**: last release v5.6 (2023-09); the repo is still pushed (2026-10) but has no new release
  (https://github.com/Anjok07/ultimatevocalremovergui). Irrelevant as an engine.
- **MSST** (v1.0.23, 2026-11-08) is where new architectures land first
  (https://github.com/ZFTurbo/Music-Source-Separation-Training/releases). It is still not a packaged
  CLI, so the plan doc's verdict stands.

## 7. Risks and unknowns

- **CPU timings are not measured.** Every speed claim here is upstream's (GPU/M3) or relative. Before
  landing anything, time each recipe on one CPU dev box under 0.47.0 vs 0.48.0 + `--mdxc_overlap 2`.
- **Karaoke on vocals-only input.** The BS karaoke models' advantage is measured on full mixes only.
- **SW's provenance.** If audio-separator's mirror is the only remaining host, a takedown would break
  the `sw` recipe. The 0.48 mirror-first change helps, because audio-separator now hosts the copy itself.
- **Cache-id policy.** Overlap or shift flag changes alter output bytes. The recommendation is to *not*
  bump cache ids for them: old entries are equal or better quality. Swapping a checkpoint or a pass
  graph *does* bump.
- **Stem-name contracts.** New checkpoints use varied stem names (`other` vs `Instrumental`). Every
  swap needs its `output_names` checked against the model YAML's `training.instruments`.
- **Per-process SHA-256 re-hash in 0.48.0** adds per-pass latency of unknown size.

## 8. Implementation plan (stacked PRs)

Decision: more stems is a goal, so `MAX_STEMS = 8` is no longer a hard
limit and drum sub-stems move from "deferred" to the top of the plan.
The work ships as `gh stack` #46 on `main`, one PR per layer, bottom
first. This table records what each layer actually does:

| # | Branch | PR | Scope as shipped | Release |
|---|---|---|---|---|
| 1 | `research/stem-updates` | #40 | This document | `docs:` |
| 2 | `stems/cpu-overlap` | #41 | Pass `--mdxc_overlap 2` and `--demucs_shifts 1` in `build_audio_separator_args` (TL;DR #2). The shifts pin is removed again in layer 4 | `perf:` |
| 3 | `stems/audio-separator-0.48` | #42 | Bump `AUDIO_SEPARATOR_VERSION` to 0.48.0; sweep `*.partial` downloads idle 10 min in `prune_model_cache`; pin `demucs==4.1.0`; `StemSeparator::preflight` checks for ffmpeg on a cache miss; fix the stale "archived" claims | `fix:` |
| 4 | `stems/sw-recipes` | #43 | `hq` is retired: `recipe = "hq"` parses as `sw`, and the panel lists `demucs` / `sw` / `hq-harmony`. `hq-harmony` becomes SW → karaoke on SW's vocals (cache `hq-harmony/v2`). ep_317 and htdemucs_6s leave the pinned set, so `--demucs_shifts` goes too. The karaoke swap did not land (no listening test yet) | `feat:` |
| 5 | `stems/raise-cap` | #44 | `MAX_STEMS` 8 → 16; new `StemKind`s; grouped stem strip: a split source is one cell, `◐` when partly audible, and `0` cycles the expanded group so the digits reach its parts (`Tab` was taken by panel cycling) | `feat:` |
| 6 | `stems/full-recipe` | #45 | Ten-stem `full`: SW → karaoke on vocals → DrumSep on drums, hi-hat/ride/crash summed to `cymbals` by an ffmpeg `amix` mixdown; the bulk-split size estimate scales with stem count. Superseded by layer 7 | `feat:` |
| 7 | `stems/full-14` | #47 | Fourteen-stem `full` (cache `full/v2`): Mel-Roformer crowd on the source first, SW on the crowd-free mix, karaoke, Sucial's chorus male/female model on the backing, DrumSep with hi-hat/ride/crash kept separate. The mixdown step is removed. Seven strip cells; Crowd is key 7 | `feat:` |

Considered and dropped: lead/rhythm guitar (no published model; a
mid/side approximation was offered and declined), woodwinds (old UVR
VR-arch model), and dry vocal + reverb.

Later, outside the stack: the jarredou 5-stem DrumSep (not in the
registry — first check whether audio-separator loads unregistered
checkpoints), the karaoke swap to `bs_roformer_karaoke_frazer_becruily.ckpt`
pending a listening test on vocals-only input, and a licence/author
line on the install-consent screen.

## Sources

- audio-separator releases: https://api.github.com/repos/nomadkaraoke/python-audio-separator/releases
- audio-separator compare v0.46.0…v0.48.0 / v0.47.0…v0.48.0: https://github.com/nomadkaraoke/python-audio-separator/compare/v0.47.0...v0.48.0
- Commits 1eaa088, 465b60f, 6d7d3fd, 4dd3477, 7a1c06f, ee1fcee (links inline)
- README: https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md
- pyproject: https://github.com/nomadkaraoke/python-audio-separator/blob/main/pyproject.toml
- models.json, models-scores.json, model-checksums.json: https://github.com/nomadkaraoke/python-audio-separator/tree/main/audio_separator
- common_separator.py (`get_stem_output_path`): https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/separator/common_separator.py
- model-configs release (sizes, YAMLs): https://github.com/nomadkaraoke/python-audio-separator/releases/tag/model-configs
- PyPI: https://pypi.org/pypi/audio-separator/json, https://pypi.org/pypi/demucs/json
- MVSep leaderboards: https://mvsep.com/quality_checker/multisong_leaderboard, https://mvsep.com/quality_checker/leaderboard/lead_back_vocals/?sort=lead, https://mvsep.com/quality_checker/leaderboard/guitar/?sort=guitar, https://mvsep.com/quality_checker/leaderboard/piano/?sort=piano, https://mvsep.com/quality_checker/leaderboard/drumsep5/?sort=kick
- MSST pretrained models: https://github.com/ZFTurbo/Music-Source-Separation-Training/blob/main/docs/pretrained_models.md
- MSST releases v1.0.20–v1.0.23: https://github.com/ZFTurbo/Music-Source-Separation-Training/releases
- SubRoformer: https://arxiv.org/abs/2611.08130, https://github.com/ArchiMickey/SubRoformer, https://huggingface.co/archimickey/SubRoformer
- Hugging Face model cards: becruily/mel-band-roformer-karaoke, becruily/bs-roformer-karaoke, anvuew/karaoke_bs_roformer, GaboxR67/MelBandRoformers, pcunwa/BS-Roformer-Resurrection, pcunwa/BS-Roformer-Leap, KimberleyJSN/melbandroformer, enerjazzer/BS-ROFO-SW-Fixed, elicwhite/bs-roformer-sw-6stem-onnx, studio409/BSRoformer-SW-6Stem-GGUF (all at https://huggingface.co/<id>)
- Demucs (maintained fork): https://github.com/adefossez/demucs
- BSRoformer.cpp: https://github.com/chenmozhijin/BSRoformer.cpp
- demucs.cpp / demucs.onnx: https://github.com/sevagh/demucs.cpp, https://github.com/sevagh/demucs.onnx
- UVR: https://github.com/Anjok07/ultimatevocalremovergui
