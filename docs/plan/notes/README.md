# Work-package notes

Every WP finishes by writing `docs/plan/notes/<WP-id>.md` (PLAN §2.7). The milestone gate reads the `## Bugs` and
`## New dependencies` sections mechanically (`scripts/collect-notes.sh`), so keep their format:

```markdown
# M3.3 — <title>

## What was built
…

## Deviations
Every deviation from the spec and why.

## Bugs
- `blossom-ir`: one-line summary of the bug in code you do not own. Reproducer: … (continuation lines are
  indented by two spaces). One item per bug; write `- None.` when there are none.

## New dependencies
| Crate | Version | License | Used by | Reason |
|---|---|---|---|---|
| roaring | 0.10.6 | MIT OR Apache-2.0 | blossom-lattice | tombstone sets (LANG-133) |

## Out-of-scope fixes
Minimal fixes to paths no WP of the milestone owns, each with its regression test (PLAN §2.7).

## Follow-ups
Anything deferred, with the FEATURES id and the reason.
```

Each `## Bugs` item becomes a row of `docs/plan/BUGS.md` (the crate is the leading backticked name); each
`## New dependencies` row is appended to `docs/design/DEPENDENCIES.md`.
