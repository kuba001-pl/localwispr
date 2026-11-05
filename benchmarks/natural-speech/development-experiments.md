# Development-only recognition safety experiments

These are exploratory results, not a production change or held-out validation.
The same 73 public one-speaker recordings and 1,150 reference words were used.
Source snapshot: the original verification harness, before the new text fixes.

| Prompt condition | Word errors |
| --- | ---: |
| No context | 51 |
| Original distracting vocabulary | 54 |
| First four distractor words only | 52 |
| Explicit Vocabulary prefix | 54 |

No shorter/list-prefix alternative was promoted. A two-decode agreement filter
also kept all baseline outputs on these negative controls, but retained fewer
beneficial corrections in synthetic development tests and adds a second decode.
A replacement-only merge was similarly explored; it lost punctuation fidelity
and some name/overall accuracy. Neither is shipped. Results are intentionally
separate from the frozen large benchmark's held-out results.

The raw fixed-condition natural results and metric script are preserved here;
additional exploratory requests/outputs and gate/merge prototypes are in
`/tmp/localwispr-natural-validation`. Baseline WER includes orthographic differences
such as Mister versus Mr and spelled versus digit numbers, not only acoustic
misrecognition. Relative paired comparisons use the same normalization.
