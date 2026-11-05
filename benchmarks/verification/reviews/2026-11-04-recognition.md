Independent review of uncommitted context recognition / cleanup changes, 2026-11-04.

Recommendation: hold release for finding 1 and restore basic stutter cleanup (finding 2). The feature also cannot promise ordinary-word preservation: I independently confirmed three lexical brand substitutions in stored acoustic results, using my own Unicode word-boundary matcher, and verified that the current recognition prompt helper is byte-identical to the benchmark helper. No new acoustic/model inference was run; stored-output checks are explicitly not fresh inference.

1. P1, release blocker: correction fallback can drop independent instructions/prohibitions.
   Source: src/cleanup.rs:884-926, called from check at 1175-1180.
   Raw: "Do not delete the logs, and do not delete the backups, actually do not delete the cache either"
   Candidate: "Do not delete the cache either."
   Current check: Ok(()). HEAD check: Err(LengthDrift { raw_words: 18, out_words: 6 }). The candidate adds a prohibition about the cache while silently losing the two earlier prohibitions; "either" makes this additive, not a retraction.
   Second raw: "Please send the report and keep the backups, actually send the report tomorrow"
   Candidate: "Send the report tomorrow."
   Current: Ok(()). HEAD: Err(LengthDrift { raw_words: 13, out_words: 4 }). This loses the independent backup instruction.
   Cause: correction_sources compares only the first content word, then discards everything since the last sentence boundary; checking the generated suffix against the candidate bypasses both the original skeleton/negations and the original length check.
   Practical fix: restrict replacement to a demonstrably corrected local clause; reject additive markers such as "either" and multiple independent predicates/conjuncts when their scope is uncertain. Preserve all other actions/prohibitions, and run their meaning/negation checks against retained source. Conservatively refusing this correction is safer than dropping content. Add both regression cases.

2. P2, new functional regression: ordinary stutter cleanup is systematically rejected.
   Source: src/cleanup.rs:1088-1090 and 1212-1223.
   Raw: "Please send send the report tomorrow"
   Candidate: "Please send the report tomorrow."
   Current: Err(Meaning); HEAD: Ok(()). Production fallback leaves "Please send send the report tomorrow" unchanged. Same result for "I I think we should send the report" and "We need to we need to update the server".
   Cause: the ordered skeleton permits deleting adjacent "not" only. STT loop removal requires four repeats and therefore cannot handle these common two/three-repeat stutters. Rule 2 still asks the LLM to remove them.
   Practical fix: allow bounded, aligned adjacent repetition removal during guard comparison, retaining surviving word order, names/numbers, quotation/literal boundaries and paragraph boundaries. Do not globally discard repeated words. Preserve intentional repetitions where ambiguous and add positive stutter controls plus negative quoted/numeric controls.

3. P2 compatibility regression, not a meaning-loss blocker by itself: clear English ten normalization is rejected.
   Source: src/cleanup.rs:419-420 (and numeric_words).
   "Send ten files to Alice tomorrow" -> "Send 10 files to Alice tomorrow." now yields Err(NewWords { novel: 1, allowed: 0 }); HEAD accepted it.
   "Send one hundred and ten files tomorrow" -> "Send 110 files tomorrow." is rejected the same way. The bilingual ambiguity of "ten" was deliberately documented in source, so this is a conservative tradeoff with user-visible cleanup loss, rather than an undisclosed corruption.
   Practical fix: use bounded phrase/language evidence to permit English numeral normalization while retaining lexical Polish "ten"; add positive English and negative Polish demonstrative controls. Do not simply remove the special case globally. Written English ordinal normalization ("twenty first" -> "21st") also falls back; unsupported ordinals are disclosed parser limits.

4. P2 extractor limitation, nonblocking independently: case-insensitive stopwords discard proper names before ranking.
   Source: src/insertion.rs:50-54, 117-120.
   before="Please speak with Will", after="", manual="" -> prompt="speak". The name Will never reaches the uppercase scoring. Control before="Please speak with Alice" -> "speak, Alice". before="We listen to The Who" -> "listen".
   Practical fix: retain credible mid-sentence proper-name candidates through stopword filtering, with sentence-position/case controls to avoid promoting ordinary sentence-initial function words. Evaluate acoustic ordinary-word controls before changing policy. This proves missing hints, not a guaranteed recognition failure.

Other limits, not new regression findings:
- Polish inflection allowance still accepts "Jutro wyślę raport do klienta" -> "Jutro wyślą raport do klienta." (I will send -> they will send). HEAD accepts it too. Case inflection needs to be distinguished from verb person agreement if a stronger meaning-preservation claim is made; current behavior is a heuristic limit.
- Known annotation filtering still deletes a genuine literal in "Use [Music] as the field name." -> "Use as the field name." HEAD also did not preserve such annotations. This is the disclosed text-only annotation heuristic; quote/backtick forms are protected.
- Fallback still changes "um iPhone works" -> "IPhone works" and "Use x = um;" -> "Use x =". Both are inherited from unconditional initial capitalization / filler handling rather than newly introduced regressions. The new literal detector protects an assignment LHS, not its RHS in inline prose. Avoid claiming universal technical-literal preservation.
- Stored acoustic lexical substitutions independently confirmed: control-21-mixed grok/grog -> groq; control-09-mixed "11 labs" -> "ElevenLabs"; control-11-mixed "open telemetry" -> "OpenTelemetry". See independent-stored-harm-check.json for full raw outputs/prompts. Existing benchmark disclosure is accurate on these cases; context bias is not an ordinary-word safety guarantee.

Validation:
- Read actual current sources and git diff, inspected HEAD implementations for attribution; no prior-agent claims used as proof.
- cargo test --offline --no-default-features --workspace: 160 app + 4 worker passed; two app native-model tests ignored. No GPU, daemon, desktop, microphone, clipboard, or live document operations.
- Generated a standalone rustc harness by extracting actual deterministic source functions unchanged. Minimal Cleanup stub contains the fields these functions use, with production default values. Baseline module extracts the actual HEAD functions and examples. Current recognition helper/module copied directly. Source hashes are recorded in source-hashes.json and were unchanged at end of probes.
- comparison-results.txt contains direct candidate acceptance, HEAD comparisons, deterministic fallback outputs and prompt outputs. These candidate probes establish guard behavior; they do not assert that an actual LLM generated each candidate.
- review-tests: 23 copied insertion unit tests passed, six added focused assertions failed: two correction-scope safety cases, adjacent stutter, English ten, compound English ten, proper-name stopword. tests.rs and test-results.txt preserve reproduction. The main production workspace suite passed despite these gaps.
- Reproduce: rustc --edition 2024 main.rs -o probes && ./probes; rustc --edition 2024 --test tests.rs -o review-tests && ./review-tests from this directory.

No production edits, commits, pushes, agent spawns, version edits, evidence overwrites, desktop changes, or model inference were performed. All newly created review artifacts are isolated in this temporary directory.
