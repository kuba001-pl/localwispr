# Parent replay after text fixes

834 source-strategy requests on the same 270 sentences + eight acoustic fixtures,
using the updated production STT helpers and unchanged recognition prompt helper.
Source hashes are recorded. This is a replay of a known benchmark, not a new
held-out selection. The parent independently recounted the original raw results.

Only six outputs changed: two pathological long-manual-hint sentence loops in
three conditions. Extended repetition suppression reduced word errors from
903 to 760 context-off, 817 to 674 with relevant context, and 877 to 734 with
distractor context. This removes repeated output; it does not make the remaining
incorrect Polish words correct. Exact name recall remains 48/126 off and 84/126
relevant. The 3 lexical ordinary-word substitutions and 12 case flags remain;
this change does not solve context overcorrection.

The original baseline report/results remain intact. `metrics.json` records every
changed output and the same scoring definitions. Native harness and source
snapshots: `/tmp/localwispr-final-recognition`. No microphone or desktop context
acquisition is part of this experiment. Runtime cleanup/LLM and insertion are
separate from these STT metrics.

## Subsequent safety verification

The snapshot above predates quote-safe filtering and the exact digital-silence
check. A parent replay of all 21 long-hint sentences plus eight acoustic fixtures
(87 requests) after those changes produced identical transcripts in every case.
Evidence is in `benchmarks/verification/safety-replay*`. A subsequent addition
recognizes single guillemets (‹›) as quote delimiters; focused text tests cover
quotation behavior. These checks do not constitute a new full-corpus replay.
