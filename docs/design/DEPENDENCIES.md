# External dependencies

Every external crate the workspace uses, with its reason and license (ARCHITECTURE §1.2, PLAN §2.4).

**Policy.**
- The license allowlist is ARCHITECTURE §1.2's: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib and
  Unicode-3.0; MPL-2.0 only for `imbl` and `webpki-roots`. `deny.toml` enforces it with `cargo deny check`, which
  `scripts/ci.sh gate` runs when cargo-deny is installed (`scripts/install-dev-tools.sh`).
- **No project license has been chosen** (PLAN §4 D19): every workspace package is `publish = false`, and
  cargo-deny ignores the unlicensed private workspace crates (`[licenses.private] ignore = true`).
- The light crates of ARCHITECTURE §1.2 are declared once in the root `[workspace.dependencies]`; a crate uses them
  with `dep.workspace = true`. Heavy crates (tokio, rustls, quinn, rayon, rustsat, batsat, criterion, dbsp, timely,
  differential-dataflow, dfir_rs, codespan-reporting, syn, quote, prettyplease, rcgen, x509-parser, hdrhistogram,
  metrics) are added by the WP that needs them, in its own crate's manifest, with an explicit version.
- A WP that adds a crate records it under `## New dependencies` in its notes (format: `docs/plan/notes/README.md`);
  the milestone gate appends those rows to the table at the end of this file (`scripts/collect-notes.sh`).

## Workspace dependencies (M1.1)

Declared in the root `Cargo.toml` (`[workspace.dependencies]`), versions verified by building with rustc 1.96.0.

| Crate | Version | License | Used by | Reason |
|---|---|---|---|---|
| thiserror | 2.0.21 | MIT OR Apache-2.0 | every library crate | error enums per crate (ARCH-17) |
| serde | 1.0.229 (derive, rc) | MIT OR Apache-2.0 | base, value, ir, schema, artifact, trace, … | configuration, traces and compiler artifacts (ARCH-09) |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | driver, trace, xtask; tests | JSON diagnostics and exports; cargo metadata in xtask |
| postcard | 1.1.3 (alloc) | MIT OR Apache-2.0 | ir, artifact, trace; value tests | compact artifact serialization |
| toml | 1.1.6 | MIT OR Apache-2.0 | schema, runtime, xtask | `schema.lock`, deployment files, `xtask/layers.toml` |
| smallvec | 1.16.2 (const_generics, union, serde) | MIT OR Apache-2.0 | value, kernel, lattice, ir | small inline vectors on hot paths |
| hashbrown | 0.17.1 (no default features; inline-more) | MIT OR Apache-2.0 | base (`det`), kernel | `HashTable` under `DetMap`/`DetSet`; no default hasher is linked |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | reserved for base/ir | insertion-ordered maps where order is meaningful |
| rowan | 0.16.1 | MIT OR Apache-2.0 | syntax | the lossless CST (ARCHITECTURE §13.2) |
| xxhash-rust | 0.8.18 (xxh3) | **BSL-1.0** | value (M2.1) | xxh3-64 value fingerprints (ENG-032, ARCH-18); see the note below |
| siphasher | 1.0.4 | MIT OR Apache-2.0 | base, value | SipHash-1-3: rule-label hashes (LANGUAGE §4.3) and the PRF (SEM-084) |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | value (M2.1) | program, schema and plan digests; checkpoint files (ARCH-18) |
| crc32c | 0.6.8 | Apache-2.0/MIT | store | WAL record checksums (ARCH-10) |
| bytes | 1.12.1 | MIT | wire, node | refcounted frame buffers |
| proptest | 1.11.0 | MIT OR Apache-2.0 | tests of every crate; `arbitrary` features | property tests and generators (ARCHITECTURE §11.6) |
| insta | 1.48.0 | Apache-2.0 | tests | snapshot tests |
| libtest-mimic | 0.8.2 | MIT/Apache-2.0 | testkit | the corpus and Molly-parity harnesses (`harness = false`) |
| clap | 4.6.7 (derive) | MIT OR Apache-2.0 | cli, xtask | command-line parsing |
| tracing | 0.1.44 | MIT | engine, node, runtime | structured logging (ARCHITECTURE §12.2) |

**Crate-specific additions by M1.1.**

| Crate | Version | License | Used by | Reason |
|---|---|---|---|---|
| siphasher | 1.0.4 | MIT OR Apache-2.0 | blossom-base | §1.2 lists siphasher for blossom-value only; base needs SipHash-1-3 for `RuleLabel::hash` (ARCHITECTURE §2.1), which lives in base. The standard library has no stable SipHash-1-3. |
| syn | 3.0.6 (full, visit, extra-traits) | MIT OR Apache-2.0 | xtask | syntax-aware source checks (`check-codes`, `check-sans-io`); the same major version serde_derive, thiserror and clap already build |
| proc-macro2 | 1.0.107 (span-locations) | MIT OR Apache-2.0 | xtask | line numbers in `check-codes` and `check-sans-io` reports |
| libfuzzer-sys | 0.4.13 | (MIT OR Apache-2.0) AND NCSA | fuzz/ (a separate workspace) | cargo-fuzz targets; outside the main workspace, so outside `cargo deny check` |

Development tools (not dependencies; installed into `.tools/` by `scripts/install-dev-tools.sh`): cargo-deny 0.20.2,
cargo-hack 0.6.45, cargo-nextest 0.9.146.

**Note on xxhash-rust.** ARCHITECTURE §1.2 names `xxhash-rust` as blossom-value's xxh3 implementation (ENG-032,
ARCH-18), but its license allowlist does not contain the crate's license, BSL-1.0 (the Boost Software License, a
permissive OSI-approved license comparable to MIT). The two statements contradict each other. **Decision (M1.1):**
keep the named crate and allow BSL-1.0 for `xxhash-rust` only in `deny.toml`, the way §1.2 already treats MPL-2.0
for two named crates. No other crate may use BSL-1.0 without its own entry here. It is listed as a deviation in
`docs/plan/notes/M1.1.md` so a DECISIONS.md line can be added at the M1 gate (M1.1 does not own DECISIONS.md).

## Added by work packages

Appended at each milestone gate from the `## New dependencies` sections of `docs/plan/notes/*.md`.

| Crate | Version | License | Used by | Reason | Source |
|---|---|---|---|---|---|
