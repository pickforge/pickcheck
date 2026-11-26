`docs/spec.md` is the contract. Explain deviations in the PR.

Do not lower metric limits, coverage floors or clippy thresholds to get green. Coverage is a ratchet: raise the floor to actual coverage rounded down. Changes to language tables or metric rules need a golden fixture in `tests/fixtures/<language>/expected.json`, including a negative case. They must also regenerate `tests/corpus/snapshot.json` and show its diff in the PR. From the workspace root, run `scripts/fetch-corpus.sh target/corpus`, then `PICKCHECK_CORPUS_UPDATE=1 PICKCHECK_CORPUS_DIR=target/corpus cargo test -p pickcheck-core --test corpus --release --locked -- --ignored` (see Repository gates in `docs/spec.md`).

`--changed` uses Git diff post-images plus untracked files, resolved against the repo root. Do not walk the current directory or let `.gitignore` filter that set. Classify syntax by tree-sitter node kinds and fields, never text prefixes or operator substrings.

Loop-guard state lives in `~/.pickforge/pickcheck/`; `PICKCHECK_HOME` overrides it. A workspace version bump also needs the exact `pickcheck-core` dependency pin in `crates/cli/Cargo.toml`.
