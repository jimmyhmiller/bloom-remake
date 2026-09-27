# M2 gate integration

## What was built

Integrated the six M2 work packages and ran the milestone gate. Added the path dependencies needed to build the M2 lexer/parser, SMT response, and WAL recovery fuzz targets from their separate workspace. Updated BUGS.md#1 after M2.1 bounded recursive value serialization and deserialization.

## Deviations

- None.

## Bugs

- None.

## New dependencies

None. The fuzz workspace uses local path dependencies on M2 crates.

## Out-of-scope fixes

- Added local crate dependencies to `fuzz/Cargo.toml` and regenerated `fuzz/Cargo.lock`, shared files outside individual WP ownership.
- Marked the M1 value serde stack-overflow bug fixed in `docs/plan/BUGS.md` after verifying the M2.1 regression tests.

## Follow-ups

- None.
