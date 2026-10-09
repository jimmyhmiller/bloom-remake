//! The S3 store against the conformance suite, each case under a prefix of its own in the test bucket, and its
//! collector. The server is `scripts/test-services.sh start`'s MinIO (`eval "$(scripts/test-services.sh env)"` sets
//! BLOSSOM_TEST_S3, BLOSSOM_TEST_S3_KEY and BLOSSOM_TEST_S3_SECRET); the fast tier ignores these tests, and with the
//! variables unset they fail, naming them.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use blossom_statestore::conformance::Harness;
use blossom_statestore::{Commit, StateError, StateStore, Write};
use blossom_statestore_s3::{Credentials, S3Store, parse_url};

const WHY: &str = "needs S3: scripts/test-services.sh start, then eval \"$(scripts/test-services.sh env)\"";

struct S3 {
    /// The store's URL (with this case's prefix), or why there is none.
    url: Result<String, String>,
}

fn var(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{name} is not set; {WHY}"))
}

fn creds() -> Result<Credentials, String> {
    Ok(Credentials {
        access_key: var("BLOSSOM_TEST_S3_KEY")?,
        secret_key: var("BLOSSOM_TEST_S3_SECRET")?,
        session_token: None,
    })
}

fn fresh() -> S3 {
    static N: AtomicU64 = AtomicU64::new(0);
    let mut nonce = [0u8; 6];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut nonce));
    let tag: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let url = var("BLOSSOM_TEST_S3").map(|base| {
        let (path, query) = base.split_once('?').unwrap_or((&base, ""));
        let n = N.fetch_add(1, Ordering::Relaxed);
        format!("{}/t-{}-{n}-{tag}?{query}", path.trim_end_matches('/'), std::process::id())
    });
    S3 { url }
}

impl S3 {
    fn store(&self) -> Result<S3Store, StateError> {
        let url = self.url.as_ref().map_err(|w| StateError::Config(w.clone()))?;
        let store = S3Store::open(parse_url(url, creds().map_err(StateError::Config)?)?)?;
        store.create_bucket()?;
        Ok(store)
    }
}

impl Harness for S3 {
    fn open(&self) -> Result<Box<dyn StateStore>, StateError> {
        Ok(Box::new(self.store()?))
    }
}

blossom_statestore::statestore_conformance!(ignore = "needs S3: scripts/test-services.sh", fresh);

fn big(seed: u8) -> Vec<u8> {
    (0..5000u32).map(|i| (i as u8).wrapping_mul(seed)).collect()
}

#[test]
#[ignore = "needs S3: scripts/test-services.sh"]
fn the_collector_deletes_only_blobs_no_manifest_names_past_the_grace_period() {
    let h = fresh();
    let mut s = h.store().unwrap();
    s.commit("gc/o", 0, &[Write::Put("a".into(), big(3)), Write::Put("b".into(), big(5))])
        .unwrap();
    // A commit that loses leaves its blob too: `other` builds on version 1, which `s` moves past first.
    let other = h.store().unwrap();
    assert_eq!(other.load("gc/o").unwrap().version, 1);
    s.commit("gc/o", 1, &[Write::Put("a".into(), big(7))]).unwrap();
    assert!(matches!(
        other.commit("gc/o", 1, &[Write::Put("c".into(), big(9))]).unwrap(),
        Commit::Conflict { current: 2 }
    ));
    assert_eq!(s.blobs_of("gc/o").unwrap().len(), 4);
    // Within the grace period nothing goes.
    s.maintain_now().unwrap();
    assert_eq!(s.blobs_of("gc/o").unwrap().len(), 4);
    // Past it, the two no manifest names go: the overwritten value's and the lost commit's.
    s.set_grace(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(1500));
    s.commit("gc/o", 2, &[]).unwrap();
    s.maintain_now().unwrap();
    assert_eq!(s.blobs_of("gc/o").unwrap().len(), 2, "the unnamed blobs are gone");
    let fresh_handle = h.store().unwrap();
    let snap = fresh_handle.load("gc/o").unwrap();
    assert_eq!(snap.version, 3);
    assert_eq!(snap.entries, vec![("a".to_string(), big(7)), ("b".to_string(), big(5))]);
}

#[test]
#[ignore = "needs S3: scripts/test-services.sh"]
fn a_wrong_secret_is_refused() {
    let h = fresh();
    let url = h.url.clone().unwrap();
    let mut c = creds().unwrap();
    c.secret_key.push('x');
    let s = S3Store::open(parse_url(&url, c).unwrap()).unwrap();
    match s.load("o") {
        Err(StateError::Unavailable(m)) => assert!(m.contains("403"), "{m}"),
        other => panic!("a wrong secret was taken: {other:?}"),
    }
}
