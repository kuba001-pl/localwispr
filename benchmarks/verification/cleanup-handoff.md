Cleanup implementation handoff

Owned production file: src/cleanup.rs only; no commits/pushes, STT edits, GPU use, desktop use, or cleanup worker/client protocol changes.

Implemented:
- ordered content skeleton retains subjects, tense, occurrence counts, name/action/numeric/literal attachments;
- narrow same-tense agreement, contraction normalization, bounded typo/case-ending allowances;
- first retained action required for correction alternatives, plus conservative protection against passive nominal-subject prohibitions and quote/code markers;
- English/Polish ambiguous ten stays lexical in every check rather than using global language inference;
- numeric types distinguish cardinals, ordinals, supported hour ordinals/clocks, and written identifiers retain leading zeroes;
- shared STT quote scanner protects quoted fillers, retains inline/nested/incomplete quote topology and word boundaries, permits prose case/punctuation, and peels extra wrappers only when source quote structure survives;
- explicit quote traversal work stack, 1024-level regression;
- uppercase filler-shaped acronyms retained by deterministic fallback.

Validation against final cleanup source:
- cargo test --offline --manifest-path /tmp/localwispr-cleanup-targeted-validation/Cargo.toml: 51 passed, 0 failed. Includes all 18 byte-identical review_regressions.rs assertions and three new production test groups.
- original frozen 25 fallback +20 guard expectations:45/45 pass; fixture bytes unchanged.
- cargo test --offline --no-default-features --workspace:160 app+4 worker passed, 2 app ignored.
- cargo clippy --offline --no-default-features --workspace --all-targets -- -D warnings:passed.
- rustfmt --edition 2024 --check src/cleanup.rs:passed.
- git diff --check -- src/cleanup.rs:passed.

All original independent review cases remain unchanged and were reexecuted; results/disagreements remain alongside their frozen inputs here. Exploratory expected values intentionally include ambiguous and unsupported positive hypotheses, so they are not a correctness score. The only accepted synthetic false hypothesis is explicit same-action/name correction I know Bob, actually I know Alice -> I know Alice, which remains a deliberately supported correction.

Residual conservative costs:
- ambiguous ten->10, fragment-only corrections, unsupported general spelling/tense changes, decimal/thousands formatting, and clock->bare-cardinal formatting fall back;
- bounded Polish case endings and local discourse/article allowances are heuristics; lower-case single-token names cannot be given universal semantic guarantees;
- iterative nested-quote traversal avoids call-stack exhaustion but reparses interiors, so pathological nesting has quadratic scan cost;
- user-configured free rewrite instructions intentionally keep the existing bypass contract.

Final cleanup SHA256: a3722e21b562363c5ac3ae399eadab98c094c17102ed1d187dcd842f92f81f86
