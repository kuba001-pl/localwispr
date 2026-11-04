# Context confidence safeguards: research result

**Recommendation: do not ship a confidence gate or the manual-normalization policy from this experiment.** No confidence candidate met the frozen dev criteria. Exact long-prompt repetition removal substantially improved pathological transcripts, but introduced ordinary-word lexical substitutions on dev and case flags on heldout. Those harms conflict with the ordinary-word preservation objective. Parent explicitly decided to retain both investigations as research only. No production files, commits, pushes, desktop operations or subagents were used by this research task. GPU released; no further inference required.

## Scope and controls

Original expanded benchmark source from `benchmarks/recognition/frozen` was used, with the installed CUDA Whisper large-v3-turbo and native VAD. Parent's newer final silence guard and widths-up-to-32 output loop collapse were **not** introduced into any paired research condition. The first 288 instrumented outputs matched all original source dev outputs exactly: zero parity differences. Parent final replay is shown separately for the manual subset and is never mixed into the main confidence comparison.

Only the frozen **93 dev speech sentences + 3 dev acoustic fixtures** were used to choose confidence thresholds/rules. References, target lists, ordinary anchors and decoys were accessed by scoring/reporting only; the decision functions receive baseline text, candidate text, prompt vocabulary and native confidence. No names or expected outputs are hardcoded. No confidence candidate was evaluated on heldout. The already published aggregate/report failure examples were known before this task; they were not used to select a rule or threshold.

Actual inference: 288 source dev passes, 21 dev manual-normalization passes, 42 once-only heldout manual-normalization validation passes, 146 public natural negative-control passes, and 9 dev-derived quiet/overlapping-speech stress passes: **506 total**. The 160 confidence settings are offline decisions using paired raw decodes, not 160 model runs. All raw results and run hashes remain under `results/`.

## Native confidence and methods

Instrumentation stores each generated text token's native probability `p`, native `plog`, token ID and raw bytes, plus segment text, timestamps, no-speech probability and the bounded actual prompt token IDs. Raw bytes avoid corruption when Whisper splits a Polish UTF-8 character across tokens. Confidence maps from byte spans to words, then matches the recognizer's filtered output conservatively; unmatched words receive confidence 1, preventing an unsupported edit. Special tokens are excluded. Segment no-speech filtering remains the source policy.

These are **autoregressive model probabilities conditioned on audio, prompt and previously decoded text**, not independent acoustic likelihoods or calibrated word correctness probabilities. Candidate-vs-baseline probability differences compare different tokenizations and histories. This limitation matters experimentally: the dev lexical false substitution has baseline word minimum p=0.335 and prompted brand minimum p=0.685 (geometric means 0.547 and 0.745). Prompt confidence rises even though the substitution is wrong. Its surrounding changed word has candidate minimum p=0.241. Sentence geometric confidence goes 0.802 to 0.790. A higher prompted name probability therefore cannot establish acoustic evidence for replacing an ordinary word.

The frozen `plan.json` covers:

- Conditional second source decode when context-free minimum token, sentence geometric, or minimum segment geometric confidence is below a threshold.
- Whole-sentence acceptance only when every changed span is a replacement and all changed baseline word confidences are below threshold; optional prompt vocabulary constraint.
- Per-replacement merge with the same confidence/vocabulary rules; preserve baseline spelling/case on unchanged normalized words and reject insertions/deletions.
- Candidate confidence floors and candidate-minus-baseline confidence margins.

Thresholds 0.1 through 0.9 and 0.95, word minimum vs geometric aggregation, vocabulary constraints, and floor/margin combinations give **160 total settings**. For constrained merges every candidate replacement word must occur in the configured prompt. Confidence-only variants were also evaluated. No edit-distance or case-specific name whitelist was introduced.

Frozen numeric qualification: zero dev control lexical false substitutions, no more than source's eight pooled case flags, relevant name recall >=29/42, pooled recall >=44/84, and pooled errors <=792. The prose goal says retain 90% of added recall: its mathematically strict pooled bound is actually 45/84 (baseline34, source46). The predeclared numeric bound44 was one term looser; **zero candidates qualify even under that looser bound**, so this discrepancy cannot reverse the rejection. No criteria were relaxed.

## Confidence results: dev only

Relevant condition: 93 sentences, 946 reference words, 42 targets, 30 ordinary controls. Pooled relevant+distractor: 186 condition outcomes of the same 93 sentences, 1,892 reference words, 84 target occurrences, 60 ordinary-control outcomes. Acoustic clips are excluded from WER and included in latency.

| Strategy | Relevant errors / WER | Relevant terms CI | Pooled errors / WER | Pooled terms CI | Pooled lexical false /60 | Pooled case flags /60 | Extra STT passes /192 | p50 / p95 ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Off |421 /44.50%|17/42|842 /44.50%|34/84|0|0|0|185 /219|
| Source |380 /40.17%|30/42|792 /41.86%|46/84|1|8|0|188 /223|
| Conditional sentence geometric p <0.8 |387 /40.91%|26/42|800 /42.28%|42/84|0|2|84|207.5 /428|
| Conditional minimum token p <0.3 |388 /41.01%|26/42|804 /42.49%|42/84|0|2|82|199 /426|
| Whole diff + prompt vocabulary; baseline word min p <0.95 |409 /43.23%|25/42|829 /43.82%|42/84|0|2|192|373.5 /439|
| Merge + prompt vocabulary; baseline word min p <0.95 |404 /42.71%|28/42|824 /43.55%|45/84|0|2|192|373.5 /439|

Conditional geometric p<0.8 removes the observed lexical substitution and most case flags, but retains only **9/13 =69% of relevant added name gain**, compared with required >=90%. The merge retains11/13=85% while worsening relevant WER and pooled errors. Whole-sentence restrictions discard still more helpful changes. None of the confidence-floor/margin settings qualifies.

For relevant mixed-language dev sentences, there are305 reference words: off135 errors /3 terms, source110 /8, conditional geometric112 /7, conditional minimum113 /7, whole131 /6, merge129 /7. Confidence protections lose mixed-language gains as well.

All seven original dev long-manual fixtures have identical output across off/source relevant/source distractor, because suffix token budgeting drops the document context. They contribute209 errors in each condition. Gating document context cannot repair this. Removing these fixtures for a diagnostic leaves878 reference words; relevant errors are212 off,171 source,178 conditional geometric, and195 merge. The source advantage and confidence tradeoff persist outside the pathological long-prompt bucket. Parent's longer output loop guard reduces those long-hint errors from209 to66 equally, which shifts the absolute totals but does not change any context-gate ranking.

Latency is an offline replay estimate: measured baseline submission time plus candidate submission time when a second pass is required, with no encoder reuse or integrated scheduling optimization. Hardware is RTX3080 and timings are not a controlled performance benchmark. Source/off use one submission each; conditional adds84/192 (43.75%) or82/192 (42.71%); whole/merge doubles submissions. Three acoustic fixtures are VAD-rejected in every condition, so whole/merge's192 extra submissions imply186 actual extra Whisper text decodes. Conditional second passes all concern speech. Instrumentation and decision overhead are small but not isolated.

Exact-case recall, CER, ordinary-anchor preservation, paired regressions, every setting and output are retained in `results/grid.json`, `results/grid.csv`, `results/selected.json`, `results/selected-predictions.jsonl`, `results/buckets.json` and `results/edit-diagnostics.json`.

## Natural speech negative controls and acoustic stress

The73 public LibriSpeech clips are one speaker, previously designated development diagnostics. Fixed representative rules above were replayed after dev analysis; they were not tuned on these recordings and have no promotion decision. All146 raw off/source outputs exactly match the parent's earlier natural replay. Using the **expanded benchmark's** punctuation/Unicode word scorer gives1,169 reference words and49 off errors vs53 source errors; the parent's51/1,150 uses another tokenizer, not different recognition.

| Fixed strategy | Errors /1,169 | Extra passes /73 | p50 /p95 ms |
|---|---:|---:|---:|
| Off |49|0|109 /177|
| Source distracting names |53|0|114 /185|
| Conditional geometric p<0.8 |49|4|115 /195|
| Conditional minimum p<0.3 |50|3|111 /208|
| Whole / merge + prompt vocabulary p<0.95 |49|73|223 /362|

These controls show reduced gratuitous context changes on relatively confident natural speech. They provide no evidence that the lost synthetic name gain returns, and no evidence about natural spoken company names. Full per-clip hypotheses are in `results/natural-metrics.json`.

The three frozen dev acoustic fixtures (silence, pink noise, clicks) remain empty for all source conditions and all gates. Heldout quiet/mixedspeech benchmark fixtures were not consumed to tune confidence. Separate stress waveforms were derived solely from dev audio, defined in `stress-manifest.json`: the same English neutral speech scaled by0.1 and0.01, and simultaneous55% English/45% Polish neutral speech from two existing dev voices. All are tested with identical off/relevant/distractor contexts and multilingual hints. At0.1 gain the sentence is retained in every condition. At0.01 the beginning is omitted in all conditions; source changes punctuation/case only. The overlapping-speaker output is inaccurate, with a small ordinary verb substitution across conditions. No company insertions occur in these nine traces. The overlap has no unambiguous single-transcript reference, so no invented WER is reported. This is a tiny stress probe, not validation of overlap recognition or quiet-speech safety. Parent's newer final silence guard was intentionally excluded from this paired frozen-source experiment.

## Separate exact long-manual normalization probe

Parent specifically requested this after the confidence setup. The frozen `manual-plan.json` policy is independent of confidence:

1. Tokenize the original configured prompt with the actual model, after reserving the existing multilingual prompt tokens.
2. **Only if its length exceeds the remaining native budget**, scan configured text left to right for exact, case-sensitive whitespace-token phrase repeats of widths1..32, shortest width first; require >=4 adjacent copies.
3. Keep the first phrase and all untouched surrounding/unique suffix content. No `collapse_word`, case/punctuation normalization, spelling correction or name whitelist.
4. Retokenize and apply the unchanged native suffix budgeting if still too long. No second decode.

This operates on the configured prompt (source context plus manual suffix); the context helper itself is unchanged. On these fixtures it removes only the artificial exact repeated manual phrase. Actual retained prompt length changes from223 tokens to44–76 on dev relevant requests, so context and the unique manual suffix both fit. The policy remains unchanged after validation.

Dev7 has68 reference words,3 target occurrences and3 ordinary controls. All conditions have six WER improvements, zero WER regressions and one equality. The **WER-only** development qualification was met and frozen in `manual-decision.json` before the permitted once-only14-sentence heldout long-hint validation. That qualification did not require zero lexical-control harm; this omission is consequential and is not hidden below.

| Set / condition | Original errors | Parent final output-loop guard errors | Normalized errors | Terms CI before→after | Lexical false controls before→after | Case-flag controls before→after | WER improved/worse/equal |
|---|---:|---:|---:|---:|---:|---:|---:|
| Dev7 off |209|66|13|1→2 of3|0→0 of3|0→0 of3|6/0/1|
| Dev7 relevant |209|66|14|1→2 of3|0→1 of3|0→0 of3|6/0/1|
| Dev7 distractor |209|66|14|1→2 of3|0→1 of3|0→0 of3|6/0/1|
| Heldout14 off |83|83|34|2→2 of7|0→0 of4|0→1 of4|9/1/4|
| Heldout14 relevant |83|83|31|2→4 of7|0→0 of4|0→1 of4|11/0/3|
| Heldout14 distractor |83|83|33|2→2 of7|0→0 of4|0→1 of4|10/1/3|

**Corrected harm classification:** dev introduces `kursor`→`Cursor`, a lexical brand flag under the frozen scorer, in both relevant and distractor conditions. Heldout introduces `railway`→`Railway`, a case-only flag, in all three conditions. There is no new dev case-only flag and no heldout lexical brand flag. Some earlier parent progress messages briefly conflated these classifications; this table and the raw metric files are definitive.

The heldout WER regression is the same mixed-language sentence in off and distractor: `wyślij`→`wyszli` adds one error (4→5). Heldout exact-case target recall falls2→1 in off/distractor despite unchanged CI recall; relevant exact-case rises2→4. These results were reported without retuning. Normalized heldout p50 is182–186.5ms and p95 192–196ms; original comparison was a separate run, so speed improvements are descriptive, not a causal timing claim. Normalization adds tokenization/string work but **zero extra model decodes**.

The prompt probe diagnoses an avoidable pathological repetition mechanism and is useful research evidence. It is **not an ordinary-word protection policy**: restoring document vocabulary also restores context pressure. Large aggregate WER gains do not erase its lexical/case harms. Parent elected not to ship it by default, consistent with the user's priority.

## Reproduction and artifacts

- `confidence-source/frozen` and `confidence-source/harness.rs`: exact initial instrumented source; source hashes verified against the pre-inference `source-hashes.json`.
- `manual-source/frozen` and `manual-source/harness.rs`: exact normalized-prompt research variant. `manual-recognition-harness` and the current `recognition-harness` contain this variant, **disabled by default** unless a request explicitly sets `normalize_long_prompt:true`. Neither variant includes parent's final silence/loop changes.
- `setup.py`, `setup_manual.py`: exact construction scripts. Do not rerun them over frozen evidence; copy the folder first. `build-command.json` / `manual-build-command.json` document rustc and installed dependency paths. To build the confidence archive independently, substitute its `harness.rs` input/output paths in the rustc command; its relative frozen module paths resolve inside that archive. `manual-source` likewise builds independently.
- `plan.json`, `manual-plan.json`, `manual-decision.json`, request JSONLs and their source/result/binary hashes preserve chronology and once-only validation boundaries. No heldout confidence requests exist.
- `run.py`: serial, refuses to overwrite existing output. `evaluate.py`: all dev rules and scoring; `analyze_extra.py`: natural controls; `analyze_manual.py`: manual before/after and parent-final comparison; `report.py`: selected results/buckets/CSV. No rule consumes references.
- Raw results: `results/dev.jsonl`, `manual.jsonl`, `manual-heldout.jsonl`, `natural.jsonl`, `stress.jsonl`, each with stderr and run metadata. Manual full examples/metrics are `results/manual-metrics.json` and `results/manual-heldout-metrics.json`.

Research limitations: one installed Whisper model, one synthetic voice engine family, related dev templates/brands, one natural public speaker with no company-name positives, tiny quiet/overlap stress coverage, autoregressive confidence rather than forced context-free candidate acoustic rescoring, and offline simulated two-pass scheduling. No claim is made that all possible acoustic safeguards are impossible. This experiment rejects the tested probability thresholds/gates and the proposed exact-normalization policy as a solution to the stated ordinary-word preservation objective. A distinct likelihood-based method or new corpus would require a new frozen evaluation, not retuning this heldout set.
