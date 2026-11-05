# Expanded synthetic recognition benchmark

Frozen working-tree recognition source; installed Whisper large-v3-turbo on CUDA. **270 unique sentences**, **42 product/company names**, eight additional acoustic fixtures. This is synthetic eSpeak evidence, not a real-dictation accuracy estimate. No production edits, target corrections, LLM cleanup, private recordings or downloads.

The source context helper changed micro WER from **33.09%** without document context to **29.94%** with relevant context and **32.14%** with distractor context. Case-insensitive literal target recall was **48/126**, **84/126**, and **48/126** respectively. Read the paired harm and control results below before interpreting aggregate gains.

## Frozen design and provenance

* Corpus SHA256: `71b8f544aea97f93745ed07147476552ba68fd9656b9e69dc372298aa1e3c0ee`. Inputs and split are in `corpus.jsonl` and `plan.json`; each run records request, binary and tool hashes.

* Development: 93 speech sentences + 3 acoustic fixtures. Held out: 177 speech sentences + 5 acoustic fixtures. Split was frozen before inference. Names and related language templates overlap across sets; this is a held-out-sentence test, not a held-out-brand test.

* Speech: 126 product sentences (42 English, 42 Polish, 42 mixed), 84 ordinary-word/near-homophone controls (28 per language), 60 name-absent sentences (20 per language). All 270 references are unique. Related templates across translations limit diversity.

* Available installed eSpeak voices/variants: US English, standard English, Scottish English, US feminine variant, Polish, Polish feminine and masculine variants. Speeds: 135/160/185 WPM, 90 sentences per speed. Polish voices mechanically pronounce mixed/English text. Voice/language assignments are partially confounded, so voice buckets are descriptive.

* Every primary fixture: context off / source relevant / source distractor. “Relevant” for an ordinary control deliberately mentions its paired brand, testing misleading context. Names-absent controls receive irrelevant brands in both context-on conditions. Context lengths vary with repeated ordinary prose; 21 speech sentences additionally receive the same long manual hint in all conditions. Context-off still preserves manual and multilingual hints.

* The source prompt prioritizes uppercase/identifier vocabulary, admits at most 40 hints / 320 characters, and puts a manual hint last. Source token budgeting may remove the context portion when that manual hint is long. Requests retain full configured prompts; the model sees the bounded prompt from the frozen STT implementation.

* Fast: all 278 fixtures × 3 = 834 primary decodes. Default/Balanced: preselected 78 speech + 8 acoustic fixtures × 3 = 258 decodes. Stability: 24 speech sentences × 3 = 72 repeated decodes, excluded from primary totals. Development alternatives: 96 fixtures × 3 alternatives × 2 context conditions = 576 extra decodes. Original plan: 1740 requests. A separately frozen post-hoc dev-only manual-hint ablation adds 21 requests. **1761 total requests; 270 unique speech sentences.**

* GPU: RTX 3080. Source and dirty working tree are preserved in `frozen/`; dependency/model/audio hashes are in `build-command.json`, `model-hashes.json`, `audio-manifest.json`. The parent may change current production helpers without changing this baseline.

## Primary recognition scores

WER/CER are micro averages over speech only, with Unicode case-folding, punctuation removal, single word spaces and preserved diacritics. Exact name recall uses literal official spelling and word boundaries. Casing is a separate column; no hardcoded recognition corrections are applied. “Case only” control flags are ambiguous orthographic changes, not evidence of a semantic brand intention.

| Set | Context | Unique speech | Word errors / words (WER) | CER | Exact name, CI | Exact name + case |
| --- | --- | --- | --- | --- | --- | --- |
| all | off | 270 | 903/2729 (33.09%) | 17.80% | 48/126 | 31/126 |
| all | relevant | 270 | 817/2729 (29.94%) | 16.55% | 84/126 | 77/126 |
| all | distractor | 270 | 877/2729 (32.14%) | 17.30% | 48/126 | 32/126 |
| dev | off | 93 | 421/946 (44.50%) | 30.65% | 17/42 | 9/42 |
| dev | relevant | 93 | 380/946 (40.17%) | 29.09% | 30/42 | 26/42 |
| dev | distractor | 93 | 412/946 (43.55%) | 30.18% | 16/42 | 10/42 |
| heldout | off | 177 | 482/1783 (27.03%) | 11.13% | 31/84 | 22/84 |
| heldout | relevant | 177 | 437/1783 (24.51%) | 10.05% | 54/84 | 51/84 |
| heldout | distractor | 177 | 465/1783 (26.08%) | 10.62% | 32/84 | 22/84 |


## Paired improvement and harm

| Set | Context | Pairs | WER better | WER worse | WER equal | Name better | Name worse | New lexical-brand harms | New case flags |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| all | relevant | 270 | 68 | 19 | 183 | 38 | 2 | 3 | 12 |
| all | distractor | 270 | 32 | 21 | 217 | 2 | 2 | 2 | 11 |
| heldout | relevant | 177 | 46 | 16 | 115 | 24 | 1 | 2 | 8 |
| heldout | distractor | 177 | 23 | 15 | 139 | 2 | 1 | 2 | 7 |


Counts compare each sentence to its own context-off result. WER equality can hide different outputs; `output_changed` and full harm examples are in `results/metrics.json`. These are descriptive comparisons, without a population-level significance claim.

## Ordinary-word / near-homophone and absent-name controls

| Context | Paired decoy lexical false clips | Paired decoy case-only clips | Ordinary anchors retained | Any lexical false-brand clips, all speech | Nonempty acoustic |
| --- | --- | --- | --- | --- | --- |
| off | 0/84 (0.00%) | 0/84 (0.00%) | 35/60 | 0 | 0/8 |
| relevant | 3/84 (3.57%) | 12/84 (14.29%) | 40/60 | 3 | 0/8 |
| distractor | 2/84 (2.38%) | 11/84 (13.10%) | 40/60 | 2 | 0/8 |


Lexical false-brand clips require a brand token sequence absent from the reference. Case-only flags require a reference occurrence without official capitalization, e.g. notion → Notion, cursor → Cursor, linear → Linear, anthropic → Anthropic; these do not affect normalized WER. Repetition of a correctly cased target is an expansion failure, not a false-brand flag. Near-homophones such as quadrant → Qdrant or cerebral → Cerebras count lexically. Multiword / compound spacing is not silently repaired. Some Polish controls translate the ordinary word and therefore have no literal ordinary-anchor annotation; they remain in the 84-clip false-substitution denominator.

## Default/Balanced versus Fast on the identical frozen subset

| Context | Accuracy | Speech | WER | CER | Exact name, CI | Submit p50 ms | Submit p95 ms |
| --- | --- | --- | --- | --- | --- | --- | --- |
| off | Fast | 78 | 27.55% | 11.61% | 15/36 | 209.0 | 442 |
| off | Balanced | 78 | 29.34% | 12.23% | 15/36 | 208.0 | 245 |
| relevant | Fast | 78 | 24.23% | 10.13% | 27/36 | 212.0 | 435 |
| relevant | Balanced | 78 | 25.51% | 10.57% | 27/36 | 210.0 | 241 |
| distractor | Fast | 78 | 26.15% | 10.92% | 16/36 | 213.5 | 422 |
| distractor | Balanced | 78 | 25.00% | 9.86% | 16/36 | 211.5 | 241 |


| Context | Balanced improves sentence WER | Balanced regresses | Equal |
| --- | --- | --- | --- |
| off | 0 | 2 | 76 |
| relevant | 0 | 1 | 77 |
| distractor | 1 | 0 | 77 |


Balanced uses the actual default config, including fallback temperatures; Fast disables fallback re-decodes. This is not a comparison to the separate Best/beam-search setting. Acoustic fixtures enter latency summaries but not WER/CER. These batches ran at different times with uncontrolled background CPU/GPU load; the observed p95 difference is not evidence that Balanced is inherently faster.

## Latency and stability

| Context | Submit p50 ms | Submit p95 ms | Max ms | Sum seconds |
| --- | --- | --- | --- | --- |
| off | 211.0 | 494 | 699 | 58.20 |
| relevant | 212.0 | 458 | 699 | 57.49 |
| distractor | 214.0 | 454 | 579 | 57.69 |


Timing starts after file loading and excludes synthesis, capture, model loading/warmup and cleanup. It includes worker submission, VAD, language detection, decoding and STT filters. Desktop GPU activity is uncontrolled. Reverse-order repetition of 24 sentences (72 decodes) changed 0 raw outputs and 0 normalized word-error counts. Repeats are not additional unique examples.

## Failure buckets

| Language | Context | WER | CER | Exact name, CI |
| --- | --- | --- | --- | --- |
| en | off | 6.63% | 4.46% | 29/42 |
| en | relevant | 5.67% | 4.09% | 38/42 |
| en | distractor | 7.20% | 4.60% | 27/42 |
| pl | off | 53.40% | 31.93% | 9/42 |
| pl | relevant | 50.06% | 30.73% | 24/42 |
| pl | distractor | 52.16% | 31.42% | 10/42 |
| mixed | off | 45.73% | 18.09% | 10/42 |
| mixed | relevant | 40.16% | 15.82% | 22/42 |
| mixed | distractor | 43.23% | 16.90% | 11/42 |


| Long manual hint | Context | Unique speech | WER | Exact name, CI |
| --- | --- | --- | --- | --- |
| False | off | 249 | 24.27% | 45/116 |
| False | relevant | 249 | 20.85% | 81/116 |
| False | distractor | 249 | 23.23% | 45/116 |
| True | off | 21 | 138.39% | 3/10 |
| True | relevant | 21 | 138.39% | 3/10 |
| True | distractor | 21 | 138.39% | 3/10 |


The long-hint bucket is deliberately extreme: 280 repetitions of “ordinary vocabulary” plus a fixed natural-language suffix. WER can exceed 100% because insertions and repeated sentences count as errors. These cases stay in the primary aggregate; removing them would hide a measured failure. Bucket comparisons alone are confounded by sentence composition; the matched dev-only ablation below addresses that limitation for seven sentences.

Across all three primary context conditions, overlapping failure-record counts (not unique-sentence counts): target_spelling_miss: 198, target_case_only_miss: 40, lexical_false_brand: 5, ordinary_brand_case: 23, ordinary_anchor_loss: 65, acoustic_hallucination: 0, empty_speech: 3, large_expansion: 6. Full records include references, hypotheses, speed, voice, language and context condition in `results/metrics.json`; voice/speed/category aggregate buckets are there too.

Largest held-out regressions, selected mechanically by normalized word-error increase:

| Context | ID | Reference | Off output | Context output | Added errors |
| --- | --- | --- | --- | --- | --- |
| relevant | name-23-mixed | Wyślij notebook z Databricks, but remove the secret values najpierw. | Myśli notebook z databricks, putremovem ssb2s iperf. | Myśli Notebooks, Databricks, Putremovem SX Red, Manu SX, I care. | 3 |
| relevant | control-11-mixed | Balon wysyła open telemetry readings i ląduje za lasem. | Alon wysyła open telemetry reaing i ląduje na lasem. | Alon wysyła OpenTelemetry Radings i londuje na lasem. | 3 |
| relevant | control-24-en | The elastic band snapped when I stretched it around the thick book. | The elastic band stopped when I stretched it around the thick book. | The last tick bond stopped when I stretched it around the thick book | 3 |
| relevant | neutral-05-mixed | Kup fresh bread i dwa jabłka, kiedy będziesz wracać do domu. | Kup fresh bread i dwa jabka, kiedy będziesz wracać do domu. | Kup Fresh Red i 2 jabka, kiedy będziesz wracać do domu. | 2 |
| distractor | control-11-mixed | Balon wysyła open telemetry readings i ląduje za lasem. | Alon wysyła open telemetry reaing i ląduje na lasem. | Alon wysyła OpenTelemetry Reading i londuje na lasem. | 3 |
| distractor | control-24-en | The elastic band snapped when I stretched it around the thick book. | The elastic band stopped when I stretched it around the thick book. | The last tick bond stopped when I stretched it around the thick book | 3 |
| distractor | name-18-en | The alert arrived from Datadog while I was making coffee. | The alert arrived from Datadog while I was making coffee. | The alert arrived from data dog while I was making coffee | 2 |
| distractor | name-22-mixed | Query in Snowflake nadal działa, please stop it before lunch. | Very insno, fuck it, nadal jała. Plea, se stop it before a lunch. | very ins of like a naval jawa, place a stop it before a lunch. | 2 |


Per-name exact spelling recall (three distinct sentences per name; English, Polish and mixed):

| Name | Off | Relevant | Distractor |
| --- | --- | --- | --- |
| NixOS | 0/3 | 0/3 | 0/3 |
| Together AI | 1/3 | 0/3 | 1/3 |
| Anthropic | 1/3 | 1/3 | 1/3 |
| Cerebras | 1/3 | 1/3 | 1/3 |
| ClickHouse | 0/3 | 1/3 | 0/3 |
| DeepSeek | 1/3 | 1/3 | 1/3 |
| DuckDB | 0/3 | 1/3 | 0/3 |
| Elastic | 1/3 | 1/3 | 1/3 |
| ElevenLabs | 0/3 | 1/3 | 0/3 |
| Fly.io | 1/3 | 1/3 | 0/3 |
| Grafana | 0/3 | 1/3 | 0/3 |
| Groq | 0/3 | 1/3 | 0/3 |
| Prometheus | 1/3 | 1/3 | 1/3 |
| Vercel | 0/3 | 1/3 | 0/3 |
| Weaviate | 0/3 | 1/3 | 0/3 |
| Cursor | 2/3 | 2/3 | 2/3 |
| Datadog | 2/3 | 2/3 | 1/3 |
| GitHub | 2/3 | 2/3 | 2/3 |
| Hugging Face | 0/3 | 2/3 | 0/3 |
| Pinecone | 2/3 | 2/3 | 2/3 |
| Qdrant | 1/3 | 2/3 | 1/3 |
| Railway | 2/3 | 2/3 | 2/3 |
| Supabase | 1/3 | 2/3 | 1/3 |
| Tailscale | 1/3 | 2/3 | 1/3 |
| Temporal | 1/3 | 2/3 | 1/3 |
| Cloudflare | 3/3 | 3/3 | 3/3 |
| Databricks | 2/3 | 3/3 | 3/3 |
| GitLab | 1/3 | 3/3 | 1/3 |
| Kubernetes | 2/3 | 3/3 | 2/3 |
| LangChain | 0/3 | 3/3 | 0/3 |
| Linear | 3/3 | 3/3 | 3/3 |
| Mistral | 3/3 | 3/3 | 3/3 |
| Modal | 1/3 | 3/3 | 1/3 |
| Notion | 1/3 | 3/3 | 2/3 |
| OpenTelemetry | 0/3 | 3/3 | 0/3 |
| Perplexity | 2/3 | 3/3 | 2/3 |
| PostgreSQL | 1/3 | 3/3 | 1/3 |
| Replicate | 1/3 | 3/3 | 1/3 |
| SQLite | 2/3 | 3/3 | 2/3 |
| Sentry | 3/3 | 3/3 | 3/3 |
| Snowflake | 1/3 | 3/3 | 1/3 |
| Svelte | 1/3 | 3/3 | 1/3 |


Held-out case-only ordinary-word flags (in frozen corpus order; normalized WER may remain unchanged):

| ID | Context | Reference | Output | Case flags |
| --- | --- | --- | --- | --- |
| control-03-mixed | distractor | Omawiamy the anthropic principle, a potem wracamy do fizyki. | Omawiamy the Anthropic Principle, a potem wracamy do fizyki. | Anthropic |
| control-04-mixed | distractor | Jej perplexity was obvious, ale nikt nie znał odpowiedzi. | jej Perplexity ma z obniął, ale nikt nie znał odpowiedzi. | Perplexity |
| control-04-mixed | relevant | Jej perplexity was obvious, ale nikt nie znał odpowiedzi. | jej Perplexity ma some news, ale nikt nie znał odpowiedzi. | Perplexity |
| control-14-mixed | relevant | Ta pinecone rolled under the bench, zanim zaczęło padać. | Ta Pinecone, rollet umber beben, na nim macie łopadać. | Pinecone |
| control-15-mixed | distractor | Mały snowflake melted on my sleeve, kiedy zamknąłem drzwi. | Mały Snowflake MLT to myslewe, kiedy zamknąłem drzwi. | Snowflake |
| control-15-mixed | relevant | Mały snowflake melted on my sleeve, kiedy zamknąłem drzwi. | Mały Snowflake MLT to my server, kiedy zamknąłem drzwi. | Snowflake |


## Development-only general prompt experiments

These alternatives were specified before inference and evaluated only on the 93 dev sentences + 3 acoustic fixtures. They change only Whisper prompt construction; all use the same frozen recognizer and no output correction. Each score pools one relevant and one distractor decode per dev fixture (186 speech decodes, 93 unique speech sentences). The off condition is shared, not rerun for every alternative.

* `source`: the original 40-word / 320-character scored-vocabulary helper.

* `case16`: at most 16 distinct nearby tokens / 160 characters containing uppercase letters or digits, ordered by proximity, without a target/name whitelist.

* `prose320`: last 160 characters before the cursor plus first 160 after, preserving ordinary prose.

* `short12`: at most 12 distinct nearest tokens / 160 characters, regardless of case, without source stopword or product lists.

| Strategy | Pooled word errors | WER | CER | Name CI | Decoy lexical false | Decoy case-only |
| --- | --- | --- | --- | --- | --- | --- |
| source | 792 | 41.86% | 29.63% | 46/84 | 1/60 | 8/60 |
| case16 | 821 | 43.39% | 30.40% | 45/84 | 2/60 | 8/60 |
| prose320 | 878 | 46.41% | 35.64% | 48/84 | 1/60 | 6/60 |
| short12 | 814 | 43.02% | 30.43% | 45/84 | 2/60 | 11/60 |


The predeclared selection rule picked **`source`** on dev (fewest pooled word errors, then fewest lexical false-brand clips, then source). Decision locked at `2026-11-03T07:38:07.584513+00:00` before any held-out inference. **No alternative was run on held-out data and no production helper was changed.**

Recommendation: treat `source` as a development-supported candidate for a fresh validation corpus. Bound nearby vocabulary and test both relevance and misleading ordinary-word contexts. Capitalization alone cannot disambiguate a brand from an ordinary word. The evidence does not justify automatic target corrections or claims of broad name robustness. Longer manual hints need a separate policy decision because suffix-prioritized token budgeting can suppress document vocabulary.

## Supplemental matched manual-hint ablation — development only

After observing catastrophic repetition in dev long-hint cases, `dev-manual-plan.json` froze a follow-up on the seven existing dev sentences with long manual hints. It removes only the manual hint, rerunning the same three conditions with the same audio/source/model (21 requests). This is post-hoc diagnosis, not a new strategy-selection contest. No corresponding held-out ablation was run and no recognizer/source setting was tuned.

| Context | Unique dev speech | With long hint WER | Without manual WER | Improved | Regressed | Equal | Name CI |
| --- | --- | --- | --- | --- | --- | --- | --- |
| off | 7 | 307.35% | 20.59% | 5 | 0 | 2 | 1/3 → 2/3 |
| relevant | 7 | 307.35% | 20.59% | 6 | 0 | 1 | 1/3 → 2/3 |
| distractor | 7 | 307.35% | 20.59% | 6 | 0 | 1 | 1/3 → 2/3 |


Examples and raw predictions are in `results/metrics.json` under `manual_ablation` and `results/dev_manual.jsonl`. This small diagnosis does not establish how arbitrary natural long hints behave. A future experiment should compare bounded, deduplicated explicit hints on a fresh corpus, with manual information retained rather than blindly deleted.

## Reproduction and practical limits

Run commands are in `README.md`. Source is compiled directly against existing installed release Rust/native dependencies to avoid rebuilding CUDA. `validate.py` checks every audio hash, frozen source, unique references, disjoint sets, request counts and scoring edge cases. `run.py` refuses to overwrite stage evidence and writes input hashes before opening the model. Raw JSONL retains prompts, original predictions, detected language and timing. `analyze.py` is standard-library code with explicit scoring definitions.

Exact audio reproduction uses the canonical `audio-fixtures.tar.zst` lossless archive (about 62 MiB), with its hash in `audio-archive.json`. `restore_audio.py --verify-only` extracts into a fresh temporary directory and verified all 278 original SHA256 hashes. Fresh eSpeak synthesis is **not** bit-identical: a separate isolated process matched 172/278 assets; all 106 feminine `f3` variants differed, consistent with that variant’s breath-noise configuration. The failed regeneration check and mismatch list are preserved in `results/audio-reproduction.json` and `.log`. No original audio or inference result was overwritten. Reproduction commands restore the actual pre-inference waveforms; resynthesis is a separate diagnostic.

The experiment uses one Whisper model and one synthetic engine family, with short scripted sentences and artificial mixed-language pronunciation. Controls are deliberately difficult and are not prevalence weighted. Hints are fabricated and often contain the target verbatim; this tests prompt influence, not automated relevance detection. Eight acoustic fixtures and 24 repeated sentences are limited stress checks. Human-speaker recordings, accents, microphones, natural disfluencies, inflection, real documents and independent unseen brands remain untested. Scores can reveal regressions; they cannot establish real-world accuracy or safety from the absence of a failure in a bucket.
