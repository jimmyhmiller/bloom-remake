# The Blossom standard library

Blossom sources of the standard library (ARCHITECTURE §1.5, FEATURES LIB-xxx). `crates/blossom-std-src` embeds
every `std/**/*.bls` file at build time:

- `std/a/b.bls` and `std/a/b/mod.bls` are module `std::a::b`; defining both is an error for that module.
- Loading is lazy and per module (PLAN §4 D10): a broken `std/foo.bls` never affects a program that does not
  import `std::foo`. A file that cannot be embedded at all (not UTF-8, a file name that is not an identifier, a
  module defined twice) is reported for that module only, with its reason, and as a cargo warning.
- Every module must build under `--strict` (ODD-10 (c)).
- Host functions (`extern fn`, `extern table fn`) live in `crates/blossom-std-host`, one Rust module per area.

Each area directory is owned by the WP that implements it (see `docs/design/PLAN.md` §8): for example
`std/delivery/**` by M8.3 and M9.6, `std/consensus/**` by M9.1 and M10.1. Corpus cases that test the library
live in `tests/corpus/std/<area>/` (PLAN §4 D17).
