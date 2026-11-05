# Independent insertion regression corpus

Run `python3 benchmarks/insertion/run.py` from any directory. The runner compiles
the actual `src/insertion.rs`, requires only Python and rustc, and does not alter
source or require desktop access. It exits nonzero for any unmet expectation.

These 60 expectations were authored in the independent verification pass before
the subsequent fixes. They cover English/Polish prose, selections, punctuation,
identifiers, code, names and fallback. They are a challenge set, not a user
success-rate estimate. Expectations have not been rewritten to hide failures.

Initial feature implementation: 55/60. Subsequent insertion fixes: 59/60.
I47 remains an explicit ambiguity: `We chose ` + `the blue.` + `New York route.`
is expected to remove the dictated period. The conservative formatter retains
it because uppercase following text could instead start a separate sentence.
The script reports this as a failure; it is not marked expected-pass or skipped.

The corpus holds exact string expectations; whitespace matters. Additional
implementation-authored cases and Unicode matrices are in `src/insertion.rs`
unit tests and are not counted as independent benchmark evidence.
