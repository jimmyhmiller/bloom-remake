# Plan bookkeeping

Working files of the delivery plan (`docs/design/PLAN.md`, `docs/design/plan.json`):

| File | What | Written by |
|---|---|---|
| `MILESTONE` | the milestone currently being built (`M1` … `M15`) | M1.1; advanced by `scripts/milestone-gate.sh` |
| `BUGS.md` | numbered open bugs found in code a WP did not own | `scripts/collect-notes.sh` at each gate |
| `notes/<WP>.md` | each WP's report: what was built, deviations, bugs, new dependencies, follow-ups | each WP (format: `notes/README.md`) |
| `coverage.md` | FEATURES coverage (from M5) | `cargo run -p xtask -- coverage` at each gate |

The agent protocol is PLAN §2; the gate procedure is PLAN §3; the coding rules are `docs/dev/CONVENTIONS.md`.
