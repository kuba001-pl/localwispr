# Context recognition and insertion verification — 2026-11-03

All changes are local and uncommitted. Nothing was pushed to GitHub. No desktop
extension reload, live document read, clipboard read, or microphone capture was
performed during this verification. The updated GNOME extension must be loaded
before testing its new context/cancellation behavior in real applications.

## What changed

- Nearby text supplies bounded vocabulary hints to local Whisper. The mechanism
  uses GNOME input-method context or AT-SPI, not application-name rules. Missing,
  stale, ambiguous, private, and terminal context falls back conservatively.
- Cursor insertion handles boundary spaces, sentence casing, duplicate
  punctuation, selections, Unicode, code, and structured text.
- GNOME paste operations now have request-specific cancellation. Focus changes,
  daemon disappearance, and clipboard ownership changes before paste abort it.
  Cancellation after paste cannot undo inserted text. Old loaded extensions use
  the legacy API and cannot provide remote cancellation.
- Cleanup preserves quotes, technical literals, numbers/IDs, names, pronouns,
  tense, negation, and content ordering more conservatively. A rejected model
  edit falls back to the source with deterministic filler cleanup.
- Speech filtering protects quoted repetitions and literal annotations. Known
  boilerplate is removed only as a complete trailing sentence. Digital silence
  detection rejects constant/nonfinite input without applying a volume cutoff
  to otherwise varying audio. Longer decoder loops can be collapsed while
  preserving code, quoted text, paragraphs, and short deliberate repetitions.

These guards cover tested patterns; they do not establish arbitrary semantic
equivalence. Conservative cleanup can reject legitimate substantial spelling,
name, or grammar corrections.

## Measurement

See [expanded benchmark](../recognition/REPORT.md),
[updated text-filter replay](../recognition/results/final-text-fixes/README.md),
[natural speech controls](../natural-speech/README.md), and
[rejected confidence/manual-prompt experiments](../context-confidence/REPORT.md).

The expanded synthetic corpus has 270 unique sentences, 42 company/product names,
84 ordinary-word controls, and eight additional acoustic fixtures. Related
names/templates occur across dev and heldout; it is not unseen-name validation.
The current measured context-off/relevant replay has 760/674 word errors among
2,729 reference words, and 48/84 correctly recognized names among 126 name
occurrences (case-insensitive). Relevant context still causes three lexical
ordinary-word brand substitutions and 12 case-only flags among 84 controls.
Improvement over the earlier feature snapshot partly comes from removing two
pathological output loops, not recognizing their remaining words correctly.

After quote/silence fixes, all 87 long-manual-hint and acoustic requests matched
that earlier replay exactly. See the adjacent raw outputs, comparison, and source
hashes. A subsequent single-guillemet quote addition is covered by text tests.
The full 834-request measurement remains explicitly tied to its own snapshot.

Human speech controls are 73 public clips from one speaker and three chapters;
they do not establish general real-user accuracy or spoken-name performance.

None of 160 tested confidence rules met the frozen development criteria. Exact
long-prompt deduplication greatly reduced pathological repetitions but introduced
ordinary-word lexical/case errors and a heldout sentence regression. Neither
research policy ships. Parent independently recomputed all 63 manual-experiment
condition outputs and reproduced every reported aggregate metric.

## Final checks

Parent reran the unchanged independent assertions against the final source:
51 standalone tests passed, including all 18 review regressions; original cleanup
expectations passed 45/45. The original insertion expectations remain 59/60.
Workspace tests passed 160 app + 4 worker tests. Both optional real-model STT tests
also passed separately (cancellation and native prompt budgeting). Extension tests
passed 48/48 and packaging passed 1/1. Strict clippy, formatting, and diff checks
passed. Final release workspace build passed. The final-source Qwen replay completed
38 production polish requests: 29 polished, eight rejected with source fallback,
and one too short. Six additional direct candidate probes rejected an actual
partial translation, quote stripping, and bracket stripping while accepting the
ER acronym correction. These counts measure behavior on targeted probes, not
human-rated overall cleanup accuracy. Raw results are adjacent JSONL files;
`final-source-hashes.json` identifies the verified production sources.

## Remaining acceptance work

The original insertion corpus remains **59/60**: I47 preserves the period in
`We chose |New York route.` + `the blue.` where the frozen expectation removes
it. That expectation remains unchanged. Internal-fragment versus separator-only
selections can also be ambiguous without selected-boundary metadata.

Real T3 Chat, browser contenteditable, native editors, and terminal paste behavior
with this exact built daemon and newly loaded extension remain unverified.
Apps must expose surrounding text through an OS interface for context adaptation;
plain insertion remains available when they do not. This is capability-based,
not a promise that every app exposes reliable context.

For local acceptance, install this checkout using `./install.sh --from-source`
when ready (this updates the local service/extension), then log out and back in
to load the extension on Wayland. Check `localwispr status`. In disposable text,
test mid-sentence insertion, a proper name visible nearby, its confusable ordinary
word, selections, quotes/code, and changing focus/cancelling before paste. Repeat
in T3 Chat and a native editor. No GitHub publication is required.

## Release review, 2026-11-04

Three new GPT-6.1 Sol/high reviews examine recognition/cleanup, cursor/insertion
integration, and release/benchmark integrity. Findings are verified by the parent
before publication. Both crate versions are now 0.3.0; the extension version is 3.

The installed daemon is active and reports both context statuses `available`
with source `gnome-input-method`. Read-only D-Bus introspection confirms the loaded
extension exposes GetCursorContext, InjectCancellable, and CancelInject. These
are integration availability checks, not a full end-to-end dictation test; no
private surrounding text, clipboard contents, or microphone input was read.

The retained independent regressions and 45 frozen cleanup expectations are now
runnable from a clean checkout using `python3 benchmarks/verification/run.py`.
The runner compiles current source helpers in a temporary Cargo project; it needs
cached ordinary Rust dependencies but no Whisper/Qwen models or native GUI/GPU
libraries. CI runs this check and committed recognition-evidence validation.

`python3 benchmarks/recognition/validate.py --skip-audio` validates committed
results, fixture metadata, splits, and scoring without claiming waveform
verification. Exact historical inference still requires the locally retained
canonical audio archive, which is not publicly hosted. The recognition README
now explicitly states that limitation. A compatible natural-speech adapter and
complete build/run/scoring commands replace the missing `audit decode` command.

The new recognition review reproduced a correction-scope bug and a stutter
regression despite passing existing tests. Corrections now reject multi-clause
and additive interpretations; ordinary short adjacent stutters may be removed by
model cleanup while quoted/literal/numeric/name repetitions remain protected.
The parent replayed the reviewer's exact two safety assertions and stutter
assertion against the changed source; all three pass. Broader positive/negative
regressions are in the production test suite. Technical-name hints matching
function words (for example Will) and ambiguous `ten` numeric formatting remain
conservative limitations; the measured recognition prompt policy is unchanged.

The new integration review also found queued keyboard work surviving cancellation,
clipboard-consumption/cleanup timing gaps, an unbound context handoff, and a
punctuated word-fragment replacement bug. The original reports are retained in
`reviews/` and describe the pre-fix source, not final release behavior.

The follow-up cleanup review found additional coordinated clauses and legitimate
repeated lexical uses. The final parent fix requires ordered retention of all
content positions during corrections (at most one substituted content value),
and permits only bounded pronoun/action restart patterns for stutter removal.
It also accepts combining marks in punctuated word-fragment replacements.
Replaying the reviewer's complete 93-test current-source harness now passes all
93, including every formerly failing adversarial assertion; the original
review and final log are retained under `reviews/`.

GNOME now uses GetInsertionContext/InjectBound with a two-second single-use
handoff. Focus/caret/selection changes before key submission invalidate it;
clipboard preparation cannot silently adopt a different target. Cancellation
before keys restores backups or clears every still-owned injected source;
after keys it preserves the normal consumption grace before restoration.
The non-GNOME keyboard commit is guarded after taking the keyboard mutex, and
daemon cancellation waits for any already-committed chord before reporting idle.
The parent reran the original queued-job reproduction: zero submissions after
cancellation. Three independent keyboard ordering fixtures also passed.

The earlier installed-extension availability check predates this new handoff
protocol. The newly built version still needs installation and a fresh GNOME
login for real-app acceptance. Code readiness and fixture success do not imply
universal app support or acknowledgment that a target consumed its clipboard.

Final local checks after the 0.3.0 fixes: 168 app + 4 worker tests passed (two
optional native STT tests are unchanged and were verified in the prior pass),
63 extension + 1 packaging tests passed, strict workspace clippy and formatting
passed, and the CUDA release workspace build succeeded. A fresh bundle smoke
check confirmed version 0.3.0, extension version 3, both new handoff methods,
native clipboard helper assets, and byte-identical packaged extension code.
The independent helper runner passes 53 tests and all 45 frozen expectations.
The insertion challenge still reports only the disclosed I47 ambiguity (59/60).
`release-0.3.0-source-hashes.json` identifies this source; earlier hashes and
measurements remain preserved as historical evidence.

The final independent GNOME recheck approves the scoped fixes for PR with no
remaining concrete blocker. It passed 93 Rust tests, 63 extension tests, and
five additional independent lifecycle matrices; installed Clutter/Shell 51
metadata matches the APIs used. The parent verified the reviewed source hashes
and retained those five additional tests in the repository extension suite.
Real application consumption and the stated OS limitations remain unverified.

Parent rerun after retaining the independent lifecycle tests: **68 extension + 1 packaging tests passed**.
