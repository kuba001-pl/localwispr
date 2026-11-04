Independent targeted re-check, 2026-11-04

Recommendation: HOLD for findings 1 and 2. The original reported correction cases, ordinary stutters, plain alphabetic selected fragments, and the targeted keyboard cancellation fixes now pass. Remaining correction-scope loss and a new stutter meaning-loss permission prevent readiness. Finding 3 is an inherited uncovered fragment case, not a newly introduced regression.

1. P1 — correction_sources still discards independent actions and prohibitions (incomplete original blocker fix).

Source: src/cleanup.rs:899-930; accepted alternative returned by check at 1308-1312.

Actual current check returns Ok(()) for all these pairs:
- Raw: "Please send the report also keep the backups, actually send the report tomorrow"
  Candidate: "Send the report tomorrow."
- Raw: "Please send the report plus keep the backups, actually send the report tomorrow"
  Candidate: "Send the report tomorrow."
- Raw: "Please send the report & keep the backups, actually send the report tomorrow"
  Candidate: "Send the report tomorrow."
- Raw: "Do not delete the logs plus do not delete the backups, actually do not delete the cache"
  Candidate: "Do not delete the cache."
- Raw: "Do not delete the logs also do not delete the backups, actually do not delete the cache"
  Candidate: "Do not delete the cache."
- Raw: "Send the report, actually send the invoice in addition"
  Candidate: "Send the invoice in addition."

The local coordination blacklist omits also/plus/&; the additive suffix blacklist omits "in addition". Checking against the replacement source loses the earlier independent actions and even their negative markers. Matching only the first content word remains insufficient proof of a single corrected clause. This is the same correction-scope defect present at first review, with original fixtures fixed but alternative forms still accepted; it is not newly caused by the current patch. The requirement to preserve independent actions/prohibitions is still unmet. Reject uncertain scope and retain untouched instructions/prohibitions; expanding a finite blacklist alone cannot establish that scope.

Positive evidence: both original independent-action/prohibition regressions now reject, additive "either" rejects, then/and joins reject, and a correction following "Keep the backups." retains that preceding sentence. "without deleting the backups" and "never delete the backups" negative controls reject dropping the prohibition.

2. P2 — new stutter alternative accepts legitimate repeated lexical uses and erases meaning.

Source: src/cleanup.rs:974-1056, especially 1033-1049; alternative accepted at 1302-1305.

- "We can can fish tomorrow" -> "We can fish tomorrow." returns Ok(()). The second "can" is the action of preserving fish, not necessarily a hesitation; its removal changes the action to fishing.
- "They will will the house to charity" -> "They will the house to charity." returns Ok(()). Auxiliary future "will" plus lexical "will" becomes a present-tense bequest.
- "The people people trust are careful" -> "The people trust are careful." returns Ok(()), deleting an embedded subject.

The actual first-review check returns Err(Meaning) for each pair. Thus these are new acceptance regressions introduced by stutter restoration. Alphabetic adjacent repetition plus the finite excluded-word list does not demonstrate an accidental stutter. Word-order validation against the already-shortened alternative cannot recover the deleted distinction. Preserve ambiguous repeats and add lexical-modal and relative-clause controls.

Positive evidence: all three originally requested two-repeat stutters and the three-repeat send control are accepted. Replacement of send by delete and we by they remains rejected. Numbers, capitalized names, quoted text, bracket literals, had had, very very and line breaks retain their protections.

3. P2 residual — punctuated decomposed-letter fragments still split a word (inherited incomplete coverage).

Source: src/insertion.rs:163-168. The new special case requires every code point to be alphabetic, unlike existing fragment handling which supports combining marks.

Context before="un", after="ed", is_replacement=true:
- "need." -> "need" for both append-space settings: FIXED.
- "ne\u{301}ed." -> " ne\u{301}ed " for both settings: still inserts spaces inside the surrounding word.
- "ne\u{301}ed" -> "ne\u{301}ed": unpunctuated counterpart preserves the join.

The punctuated decomposed case produces the same broken result in the first-review insertion implementation; this is not a new regression. It is a remaining gap in this targeted punctuation fix. The added alphabetic matrix passes 200 combinations (five left boundaries, five right boundaries, four fragment bodies, two append-space settings), including composed Unicode letters and combining marks at context boundaries. The newly added failing decomposed-body fixture isolates the missing case. Do not classify intentional non-period punctuation or explicit multiword replacements as regressions; those exclusions are explicitly documented in this fix.

Keyboard result: READY within the requested non-desktop fixture scope.

The exact current press_when_current, wait_for_keyboard, Injector::wait_for_keyboard_idle and entire Injector::inject implementation were compiled with adapters replacing only actual keyboard, clipboard and modifier-wait side effects. keyboard-control-flow-verification.json asserts byte-for-byte retention of the production helpers and Injector implementation. No production GNOME protocol behavior was exercised or reviewed.

Three independently written keyboard tests pass:
- Occupy the sole blocking worker after staging clipboard, let the actual lower DataControl branch enqueue its paste, set cancellation and abort/join the processing future, then release the worker. The queued job sends zero chords, including after idle acknowledgment and runtime draining.
- Let the actual DataControl branch enter a deliberately blocked committed chord, then cancel and abort/join. wait_for_keyboard_idle does not acknowledge while the chord is blocked; clipboard restoration also remains blocked. Releasing the chord allows completion before acknowledgment and both selection restores afterward.
- Hold the keyboard mutex while a press helper job waits, cancel/abort, then release it. The post-lock cancellation check prevents every press.

Limited daemon check: current src/daemon.rs:914-928 awaits the aborted processing task, then awaits self.injector.lock().await.wait_for_keyboard_idle().await at line 921, before setting Idle and broadcasting idle. No broader daemon or GNOME review was performed. These fixtures demonstrate cancellation ordering; they do not make claims about compositor-level pasting or actual device delivery.

Test evidence and reproduction:

- frozen45: 45/45 original unchanged expectations passed, including 25 fallback and 20 guard cases; frozen45-inputs.jsonl and frozen45-results.log.
- first18: 18/18 original assertions passed; copied review_regressions.rs is byte-identical to repository fixture; first18-results.log.
- Full isolated text harness: 93 tests, 83 pass, 10 intentionally failing adversarial safety assertions exposing the three findings. The 22 new focused tests account for 12 passes and 10 failures. All 71 included existing cleanup/insertion/protocol/first18 tests pass. all-text-results.log and focused-results.log.
- Independent keyboard fixtures: 3/3 pass; keyboard-results.log. Current-source fixture regenerated after concurrent inject.rs changes, which affected only GNOME formatting in the inspected Injector block. Both targeted keyboard helpers and lower branch were retained verbatim.
- Run python compile-text.py in this directory, then ./text-tests --nocapture, ./text-tests review_regressions::, and ./text-probe < frozen45-inputs.jsonl. Failing adversarial assertions are intentional evidence, not a changed expectation to make the suite green. keyboard-compile-command.json records exact rustc command for keyboard-tests; run ./keyboard-tests --test-threads=1 --nocapture.
- Full cleanup.rs, insertion.rs and llm_proto.rs copied directly from current source. Actual STT deterministic functions extracted unchanged. Cleanup config definition/defaults extracted unchanged. Compiled with rustc and existing dependency rlibs read-only; no GPU/model inference or full Cargo rebuild. Prior comparison module is the unchanged first-review extracted cleanup implementation, not a reimplementation of the policy.
- source-hashes.json records initial sources; final-source-hashes.json records completion. cleanup/stt/insertion unchanged through validation. inject.rs was concurrently edited outside the lower targeted branch; the keyboard fixture was regenerated and rerun against its final inspected version. final hashes and control-flow checks retained.
- Fixture SHA256 cleanup-frozen-results.jsonl: bcd42401823886dadc4cee1ddf65ab4407719cd4b311345c3f318c9bf228b815
- Fixture SHA256 first18 review_regressions.rs: 05b5a0ac2c047f8bb6b61ddba9d9e929a448c7d172ca01abf521c7bb68071f84

These are deterministic candidate-acceptance tests, not assertions about which responses an actual model would generate. No source edits, Git mutations, subagents, GPU/model inference, desktop input or live clipboard operations were performed. All new evidence is in this temporary directory. Already disclosed recognizer stopword/name, ambiguous numeral and other unrelated inherited heuristic limits were excluded from this re-check.
