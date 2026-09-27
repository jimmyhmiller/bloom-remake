# Open bugs

Bugs a work package found in code it does not own (PLAN §2.7). At each milestone gate `scripts/collect-notes.sh`
appends every new `## Bugs` item of `docs/plan/notes/*.md` as a numbered row. The next WP that owns the crate
fixes the bug first or defers it with a written reason in its notes (PLAN §2.1), and updates the row's status
(`open`, `fixed in <WP>`, `deferred to <WP>: <reason>`). Corpus manifests cite a row as `issue = "BUGS.md#<n>"`.

| # | Source | Crate | Summary | Status |
|---|---|---|---|---|
