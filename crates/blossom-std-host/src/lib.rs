#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-std-host`: the Rust implementations behind the standard library's `extern fn`s and
//! `extern table fn`s, as an [`ExternRegistry`] (ARCHITECTURE §1.2, §1.5).
//!
//! One module per standard-library area; each is owned by the WP that implements that area, and until then its
//! `register` adds nothing. This file is a dispatch file, frozen after M1 (PLAN §2.4): adding an area is a plan
//! change. The implementing WPs (plan.json):
//!
//! | WP | Areas |
//! |---|---|
//! | M8.3 | `delivery`, `bcast`, `membership`, `fd`, `timers`, `vote`, `commit`, `quorum`, `coord` |
//! | M8.4 | `ids`, `queue`, `seq`, `clock`, `seal`, `kvs`, `crdt` |
//! | M9.1 | `consensus` |
//! | M9.6 | `election`, `lease`, `lock` |
//! | M9.7 | `gc`, `zset`, `authz` |
//! | M10.7 | `examples` |
//! | M11.3 | `push` |
//! | M12.3 | `upgrade` |
//! | M15.4 | `crypto`, `oncetree` |
//! | M15.7 | `pipeline` |
//! | S6 | `checksum`, `compress`, `hash` (the Kafka goal's standard library, FOREIGN-PROTOCOLS §4, decision K3) |

use blossom_value::error::ValueError;
use blossom_value::externs::ExternRegistry;

pub mod authz;
pub mod bcast;
pub mod checksum;
pub mod clock;
pub mod commit;
pub mod compress;
pub mod consensus;
pub mod coord;
pub mod crdt;
pub mod crypto;
pub mod delivery;
pub mod election;
pub mod examples;
pub mod fd;
pub mod gc;
pub mod hash;
mod host;
pub mod ids;
pub mod kvs;
pub mod lease;
pub mod lock;
pub mod membership;
pub mod oncetree;
pub mod pipeline;
pub mod push;
pub mod queue;
pub mod quorum;
pub mod seal;
pub mod seq;
pub mod timers;
pub mod upgrade;
pub mod vote;
pub mod zset;

/// Registers every standard-library host function into `reg`.
pub fn register_all(reg: &mut ExternRegistry) -> Result<(), ValueError> {
    delivery::register(reg)?;
    bcast::register(reg)?;
    membership::register(reg)?;
    fd::register(reg)?;
    timers::register(reg)?;
    election::register(reg)?;
    lease::register(reg)?;
    vote::register(reg)?;
    commit::register(reg)?;
    lock::register(reg)?;
    quorum::register(reg)?;
    coord::register(reg)?;
    consensus::register(reg)?;
    ids::register(reg)?;
    queue::register(reg)?;
    seq::register(reg)?;
    clock::register(reg)?;
    seal::register(reg)?;
    kvs::register(reg)?;
    crdt::register(reg)?;
    gc::register(reg)?;
    zset::register(reg)?;
    examples::register(reg)?;
    authz::register(reg)?;
    upgrade::register(reg)?;
    crypto::register(reg)?;
    push::register(reg)?;
    oncetree::register(reg)?;
    pipeline::register(reg)?;
    checksum::register(reg)?;
    compress::register(reg)?;
    hash::register(reg)?;
    Ok(())
}

/// The registry of every standard-library host function. Fails only if two modules register the same path.
pub fn registry() -> Result<ExternRegistry, ValueError> {
    let mut reg = ExternRegistry::new();
    register_all(&mut reg)?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::Value;

    #[test]
    fn registry_registers_every_path_once() {
        // Building the registry fails with DuplicateExtern if two areas register one path.
        let reg = registry().unwrap();
        let paths = reg.paths();
        let mut distinct = paths.clone();
        distinct.dedup();
        assert_eq!(paths, distinct);
        assert_eq!(paths.len(), reg.len());
        // Registering everything again is refused exactly when something was registered: no area registers a
        // path silently twice.
        let mut again = registry().unwrap();
        assert_eq!(register_all(&mut again).is_err(), !reg.is_empty());
    }

    #[test]
    fn the_registry_implements_exactly_the_std_catalog() {
        // The compiler accepts an `extern fn` only for a catalog path with the catalog's signature; the host must
        // provide every one of them, with that signature, and nothing the compiler would refuse.
        let reg = registry().unwrap();
        let mut catalog: Vec<&str> = blossom_value::STD_EXTERNS.iter().map(|e| e.path).collect();
        catalog.sort_unstable();
        assert_eq!(reg.paths(), catalog);
        for e in blossom_value::STD_EXTERNS {
            assert_eq!(reg.signature(e.path), Some(&e.signature()), "{}", e.path);
        }
    }

    fn call(path: &str, args: &[Value]) -> Value {
        registry().unwrap().lookup_fn(path).unwrap().call(args).unwrap()
    }

    fn b(x: &[u8]) -> Value {
        Value::Bytes(x.into())
    }

    #[test]
    fn checksums_and_hashes_match_known_answers() {
        use blossom_value::value::IntValue;
        // The CRC catalogue's check value is the checksum of the ASCII digits "123456789".
        assert_eq!(
            call("blossom_std::checksum::crc32c", &[b(b"123456789")]),
            Value::Int(IntValue::U32(0xe306_9283))
        );
        assert_eq!(
            call("blossom_std::checksum::crc32", &[b(b"123456789")]),
            Value::Int(IntValue::U32(0xcbf4_3926))
        );
        assert_eq!(
            call("blossom_std::checksum::crc32c", &[b(b"")]),
            Value::Int(IntValue::U32(0))
        );
        // RFC 3720 B.4: 32 bytes of zeros, and of 0xff.
        assert_eq!(
            call("blossom_std::checksum::crc32c", &[b(&[0; 32])]),
            Value::Int(IntValue::U32(0x8a91_36aa))
        );
        assert_eq!(
            call("blossom_std::checksum::crc32c", &[b(&[0xff; 32])]),
            Value::Int(IntValue::U32(0x62a8_ab43))
        );
        // FIPS 180-2: SHA-256("abc"). BLAKE3 of the empty input, from the reference test vectors.
        let hex = |v: Value| match v {
            Value::Bytes(x) => x.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            hex(call("blossom_std::hash::sha256", &[b(b"abc")])),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(call("blossom_std::hash::blake3", &[b(b"")])),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }
}
