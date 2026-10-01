# SL: the language slice (working notes)

Design: `docs/design/EXTENSIONS.md`. Branch `slice-lang`, worktree `.worktrees/sl`.

## Resume here

- **State (2026-10-01):** design written; no code yet. Next: item 1.
- Update this section whenever work stops.

## Baseline (S8, merged at 69b630c)

`examples/kafka`: 5,591 lines, 4,066 code lines (no blank or comment lines). Count with:
`for f in examples/kafka/*.bls; do grep -cvE '^\s*$|^\s*//' $f; done | paste -sd+ | bc`.

## Work items, in order

1. `?` on `Option` in functions, tuple patterns in function `let`s (EXTENSIONS 2.1).
2. Generic functions with named-function arguments (2.2).
3. Guarded persistence (`table … while …`) and soft tables (2.3).
4. `upsert` into resolved tables, multi-column costs, `resolve prefer(…)` (2.4).
5. Planner: infallibility-aware ordering; `top!`, `index!` with `per`, multi-alternative `per` (2.6).
6. Formats (2.5).
7. The Kafka rewrite (2.7), measured; every S6–S8 test green.
8. Review, notes, merge.

## Findings and deviations
