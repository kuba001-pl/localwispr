# Expanded recognition benchmark

This directory owns a frozen **synthetic** recognition experiment. It does not modify production code. Read `REPORT.md` for measured outcomes; the generated files under `results/` contain every request, unmodified recognition output, source/tool/model hashes, latency and scoring detail.

The corpus contains **270 unique authored sentences**, not repeated decodes counted as new sentences: 126 product/company sentences (42 names in English, Polish and mixed prose), 84 ordinary-word/near-homophone controls, 60 name-absent sentences, plus eight non-speech fixtures. References, annotations, context, split and subsets were frozen before recognition. The SHA256 is recorded in `plan.json`.

There are 93 development and 177 held-out speech sentences; the additional acoustic split is three dev and five held out. Product/name templates are related across languages and names overlap between splits. This is a held-out **sentence** experiment, not a held-out-name or natural-speaker generalization claim. eSpeak voice variants are not independent human speakers.

## Reproduce

Prerequisites for exact recognition reproduction: the existing installed large-v3-turbo and Silero models, existing CUDA release dependencies in this checkout, Python 3, GNU tar and zstd. ffmpeg and the installed Proton eSpeak NG library are needed only for fresh synthesis diagnostics. No downloads or private recordings. Model paths can be overridden with `LOCALWISPR_BENCH_MODELS` for the harness.

From a clean checkout, validate committed corpus, result hashes, splits, and scoring without audio or inference:

```bash
python3 benchmarks/recognition/validate.py --skip-audio
```

**Exact historical inference replay requires an additional artifact that is not
published with this repository:** `audio-fixtures.tar.zst`. Its required SHA-256
is recorded in `audio-archive.json`. A clean checkout cannot reconstruct all its
waveforms bit-for-bit. The original archive remains local to the benchmark
machine; obtain it from the experiment owner before using the exact-replay
commands below. New synthesis is a new experiment and must not overwrite the
recorded manifests/results.

With the canonical archive and restored audio present, validate all evidence:

```bash
python benchmarks/recognition/validate.py
python benchmarks/recognition/restore_audio.py --verify-only
```

The supplied `corpus.jsonl` is the input. Do **not** rerun `make_corpus.py`; it refuses to overwrite the frozen file. Voice aliases `en-gb` and `en-sc` resolve to installed eSpeak names `en` and `en-gb-scotland`; `tts-info.json` records this pre-inference compatibility resolution. Sentence text, split and synthetic voice assignment are unchanged.

**Restore the original audio rather than resynthesizing it.** A fresh isolated synthesis check matched only 172/278 hashes: all 106 feminine `f3` fixtures differ across processes (their voice configuration includes breath noise). That failed bit-reproduction check is preserved in `results/audio-reproduction.{json,log}`. `audio-fixtures.tar.zst` is the canonical lossless archive of the actual 278 pre-inference float32 fixtures, about 62 MiB, with an archive hash in `audio-archive.json`. A fresh extraction verified all 278 original hashes. `synthesize.py` now refuses to overwrite existing canonical manifests. `verify_reproduction.py` is the optional diagnostic that reproduces the synthesis mismatch, not the exact-audio restore check.

The runner refuses to overwrite results. To preserve supplied evidence while reproducing, first move `results/` to a new directory, then create an empty `results/` and run:

```bash
python benchmarks/recognition/restore_audio.py
python benchmarks/recognition/build.py
python benchmarks/recognition/run.py dev
python benchmarks/recognition/analyze.py dev
python benchmarks/recognition/run.py heldout
python benchmarks/recognition/run.py balanced
python benchmarks/recognition/run.py repeat
python benchmarks/recognition/run.py dev_manual
python benchmarks/recognition/analyze.py all
python benchmarks/recognition/write_report.py
```

The development decision is locked before held-out inference. Only the original frozen source prompt strategy is tested on held-out data. Alternative prompt strategies are development-only experiments; their scores are not claims about held-out performance. Strategies inspect only nearby context and generic case/distance rules. They never read references/brand annotations or correct recognized text.

`dev_manual` is a separately frozen, post-hoc development-only ablation (`dev-manual-plan.json`): it removes the unusually long repetitive manual hint from the seven dev sentences assigned that hint. It does not change the original strategy decision or run an alternative on held-out sentences. These 21 supplemental decodes are separate from primary totals.

## Measurements

The frozen Rust harness invokes the actual `SttWorker` from `frozen/src/stt.rs`, including the actual context prompt helper, language selection, Silero VAD, model decode and STT hallucination/loop filters. It loads the installed Whisper large-v3-turbo model on CUDA. The production default `Balanced` configuration is preserved for that subset; `Fast` is explicitly set for the main run. Context-off disables document context but retains any preassigned long manual hint and the production multilingual hint.

Each main sentence has three paired conditions: no document context; fabricated nearby notes mentioning its product or control decoy; notes containing irrelevant/misleading products. Counterbalanced condition order and a fixed reverse-order repetition subset reduce simple order effects. Repetitions never enter primary metrics. The Balanced subset is fixed before inference (78 speech sentences plus all eight acoustic fixtures).

WER is micro-averaged Levenshtein word distance after Unicode case folding and punctuation removal; CER uses the same normalized text with single word spaces. Polish diacritics are retained. Silent fixtures are excluded from WER/CER denominators and reported separately. No special product normalization, phonetic aliases, substitutions or target corrections are performed.

Name recall counts exact literal target spellings with word boundaries, first case-insensitively and separately case-sensitively. Token-only name recall is additionally available for punctuation diagnosis. Ordinary-word harms distinguish a decoy spelling newly inserted lexically from capitalization of a source ordinary word sharing its spelling. Case-only flags indicate ambiguity; they do not prove that a model semantically intended a brand. Any-brand false insertions require the brand spelling to be absent from the reference, and case-only flags require a reference occurrence without official capitalization; repetition of a correctly cased target belongs to the expansion bucket instead. Ordinary-anchor losses are also retained. Expected ordinary anchors that do not occur in translated Polish reference text do not enter anchor-preservation denominators.

`submit_ms` excludes audio synthesis, capture, file loading, model loading/warmup and LLM cleanup. It includes worker submission, VAD, language choice, decode and frozen STT filters. `stt_ms` is also retained as returned by production. GPU desktop activity is uncontrolled; latency is descriptive, not a scheduling guarantee.

## Files

* `corpus.jsonl`, `plan.json`: frozen inputs and experiment plan.
* `frozen/`: copied source and original working-tree diff/status/HEAD; hashes distinguish this snapshot from concurrent edits.
* `harness.rs`, `build.py`: isolated recognition entrypoint and reusable native dependency build.
* `synthesize.py`, `audio-manifest.json`, `tts-info.json`: original synthetic generation and hashes. Extracted audio and binary are generated/ignored (about 78 MiB and 34 MiB respectively).
* `audio-fixtures.tar.zst`, `audio-archive.json`, `restore_audio.py`: canonical exact waveforms, archive hash, and lossless restore/verification.
* `run.py`: serial request/response collection, stage input hashes and model logs.
* `analyze.py`, `validate.py`: explicit scoring, split/input checks and ambiguity edge cases.
* `results/*-requests.jsonl`, `results/*.jsonl`: frozen run requests and raw predictions, including prompts.
* `results/metrics.json`, `results/scored.jsonl`: aggregate tables, paired harms, failure buckets and per-decode scores.
* `results/dev-decision.json`: development-only strategy selection, locked before heldout.

Synthetic speech can overstate improvements because names are spelled consistently in text while pronounced mechanically. Neither this benchmark nor the earlier ten-sentence experiment estimates real-user recognition accuracy. Human recordings with consent and a separately held-out speaker test are needed for that claim.

The canonical audio archive is a generated local artifact excluded from Git.
Keep or transfer it alongside results when exact replay is needed; fresh f3
voice synthesis is not bit-identical. No audio or model binaries are committed.
