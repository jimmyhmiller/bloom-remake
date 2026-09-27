# Fuzz targets

cargo-fuzz targets of ARCHITECTURE §11.8, one per file in `fuzz_targets/`, run nightly on the nightly toolchain:

```sh
cargo install cargo-fuzz --locked --root .tools    # scripts/install-dev-tools.sh from M13.3
cd fuzz && cargo +nightly fuzz run lexer_parser -- -max_total_time=60
```

| Target | Implemented by |
|---|---|
| `lexer_parser` | M2.3 |
| `formatter` | M3.7 |
| `front` | M13.3 |
| `wire_decoder` | M4.4 |
| `wal_recovery` | M2.6 |
| `trace_reader` | M4.7 |
| `artifact_decoder` | M3.4 |
| `smt_response` | M2.5 |
| `admission` | M13.3 |

A target that is not implemented yet panics with `fuzz target <name> is not implemented yet (WP <id>)`, so running
it reports an immediate finding instead of passing. The implementing WP adds the crate dependencies it needs to
`fuzz/Cargo.toml` and a stable proptest mirror to its own crate's tests.
