//! A versioned set of keys: what a node's database stores its rows in (docs/design/DATABASE.md §2,
//! docs/design/SQL-TABLES.md §1). [`crate::lsm::Lsm`] is one; a SQL-backed tree (blossom-runtime's `SqlTree`) is
//! another; [`MemTree`] is a reference in memory. [`tree_suite`] holds any of them to a model.
//!
//! A key is present from the version that put it until the version that deleted it. Versions are applied in
//! increasing order, each with a caller's **mark** (a position the version reaches, which a flush reports back); a
//! read names its version, at most the newest applied and no older than the floor.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use crate::lsm::{Flushed, Op, Page, TreeInfo};
use crate::{StoreError, invalid};

/// A versioned set of keys (see the module's documentation).
pub trait KeyTree: Send + Sync {
    /// The format of the keys it holds (`None`: it holds none yet, and takes the format it is given).
    fn key_format(&self) -> Result<Option<u32>, StoreError>;
    /// The newest version applied (`None`: none yet).
    fn applied(&self) -> Result<Option<u64>, StoreError>;
    /// Applies one version's changes: `version` above every one before, reaching the caller's `mark`.
    fn apply(&self, version: u64, mark: u64, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError>;
    /// Adds `changes` to the newest applied version (keys derived from the rows already there). Refused before any
    /// version.
    fn amend(&self, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError>;
    /// Whether it wants a flush now.
    fn needs_flush(&self) -> Result<bool, StoreError>;
    /// Makes every applied version durable (as the tree understands it): the version and mark it covers then.
    fn flush(&self) -> Result<Flushed, StoreError>;
    /// The version and mark the durable part covers: a recovery applies what came after.
    fn flushed(&self) -> Result<Flushed, StoreError>;
    /// Merges what it keeps apart, if anything is due: whether it did.
    fn compact(&self) -> Result<bool, StoreError>;
    /// The oldest version a read may ask for.
    fn floor(&self) -> Result<u64, StoreError>;
    /// Raises the floor to `version` (versions below it are not what they were).
    fn raise_floor(&self, version: u64) -> Result<(), StoreError>;
    /// Whether `key` is present as of `as_of`.
    fn get(&self, key: &[u8], as_of: u64) -> Result<bool, StoreError>;
    /// The keys starting with `prefix` present as of `as_of`, in order.
    fn scan(&self, prefix: &[u8], as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        let end = crate::lsm::successor(prefix);
        self.scan_range(prefix, end.as_deref(), as_of)
    }
    /// The keys from `start` (inclusive) to `end` (exclusive; `None`: to the end) present as of `as_of`, in order.
    fn scan_range(&self, start: &[u8], end: Option<&[u8]>, as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut out = Vec::new();
        let mut from = start.to_vec();
        loop {
            let page = self.scan_page(&from, end, as_of, 1024)?;
            out.extend(page.keys);
            match page.next {
                Some(n) => from = n,
                None => return Ok(out),
            }
        }
    }
    /// At most `keys` keys of the range, and where the next page starts (`None`: the range is done).
    fn scan_page(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, keys: usize) -> Result<Page, StoreError>;
    /// What it holds, for tools.
    fn info(&self) -> Result<TreeInfo, StoreError>;
}

/// A [`KeyTree`] in memory: every version of every key, kept; durable as far as a flush says (for tests).
pub struct MemTree {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    /// Each key's changes: version → present.
    keys: BTreeMap<Vec<u8>, BTreeMap<u64, bool>>,
    applied: Option<(u64, u64)>,
    flushed: Flushed,
    floor: u64,
    format: Option<u32>,
    given_format: u32,
}

impl MemTree {
    /// An empty tree that takes key format `format` with its first version.
    pub fn new(format: u32) -> MemTree {
        MemTree {
            inner: Mutex::new(MemInner {
                given_format: format,
                ..MemInner::default()
            }),
        }
    }

    fn inner(&self) -> Result<MutexGuard<'_, MemInner>, StoreError> {
        self.inner.lock().map_err(|_| invalid("the tree's lock is poisoned"))
    }
}

impl MemInner {
    /// Refuses a read past the newest version or below the floor, as the LSM does.
    fn readable(&self, as_of: u64) -> Result<(), StoreError> {
        let newest = self.applied.map(|(v, _)| v);
        if as_of < self.floor || newest.is_some_and(|n| as_of > n) {
            return Err(invalid(format!(
                "version {as_of} is outside the history kept (from version {})",
                self.floor
            )));
        }
        Ok(())
    }
}

fn present(changes: &BTreeMap<u64, bool>, as_of: u64) -> bool {
    changes.range(..=as_of).next_back().is_some_and(|(_, p)| *p)
}

impl KeyTree for MemTree {
    fn key_format(&self) -> Result<Option<u32>, StoreError> {
        Ok(self.inner()?.format)
    }

    fn applied(&self) -> Result<Option<u64>, StoreError> {
        Ok(self.inner()?.applied.map(|(v, _)| v))
    }

    fn apply(&self, version: u64, mark: u64, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        let mut s = self.inner()?;
        if let Some((a, _)) = s.applied
            && version <= a
        {
            return Err(invalid(format!("version {version} applied after version {a}")));
        }
        for (k, op) in changes {
            s.keys.entry(k).or_default().insert(version, op == Op::Put);
        }
        s.applied = Some((version, mark));
        if s.format.is_none() {
            s.format = Some(s.given_format);
        }
        Ok(())
    }

    fn amend(&self, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        let mut s = self.inner()?;
        let (version, _) = s
            .applied
            .ok_or_else(|| invalid("an amendment of a tree with no version applied"))?;
        for (k, op) in changes {
            s.keys.entry(k).or_default().insert(version, op == Op::Put);
        }
        Ok(())
    }

    fn needs_flush(&self) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn flush(&self) -> Result<Flushed, StoreError> {
        let mut s = self.inner()?;
        let f = match s.applied {
            Some((v, m)) => Flushed::at(Some(v), m),
            None => Flushed::at(None, 0),
        };
        s.flushed = f;
        Ok(f)
    }

    fn flushed(&self) -> Result<Flushed, StoreError> {
        Ok(self.inner()?.flushed)
    }

    fn compact(&self) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn floor(&self) -> Result<u64, StoreError> {
        Ok(self.inner()?.floor)
    }

    fn raise_floor(&self, version: u64) -> Result<(), StoreError> {
        let mut s = self.inner()?;
        s.floor = s.floor.max(version);
        Ok(())
    }

    fn get(&self, key: &[u8], as_of: u64) -> Result<bool, StoreError> {
        let s = self.inner()?;
        s.readable(as_of)?;
        Ok(s.keys.get(key).is_some_and(|c| present(c, as_of)))
    }

    fn scan_page(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, keys: usize) -> Result<Page, StoreError> {
        let s = self.inner()?;
        s.readable(as_of)?;
        let mut out = Vec::new();
        for (k, c) in s.keys.range(start.to_vec()..) {
            if end.is_some_and(|e| k.as_slice() >= e) {
                break;
            }
            if !present(c, as_of) {
                continue;
            }
            if out.len() == keys.max(1) {
                return Ok(Page {
                    keys: out,
                    next: Some(k.clone()),
                });
            }
            out.push(k.clone());
        }
        Ok(Page { keys: out, next: None })
    }

    fn info(&self) -> Result<TreeInfo, StoreError> {
        let s = self.inner()?;
        Ok(TreeInfo {
            tables: Vec::new(),
            flushed: s.flushed.version(),
            applied: s.applied.map(|(v, _)| v),
            floor: s.floor,
            key_format: s.format.unwrap_or(s.given_format),
            memtable_entries: s.keys.values().map(BTreeMap::len).sum(),
        })
    }
}

/// Holds a tree to a model: versions of puts and deletes of keys that share prefixes, an amendment, a floor, then
/// every key's presence as of every version, scans of every prefix and of ranges, and pages of every size, against
/// what the model says. `tree` is new and empty; `reopen`, when given, opens the same tree again (a durable tree:
/// after a flush, it must hold the same).
pub fn tree_suite(
    tree: &dyn KeyTree,
    reopen: Option<&dyn Fn() -> Result<Box<dyn KeyTree>, StoreError>>,
) -> Result<(), StoreError> {
    let fail = |m: String| Err(invalid(format!("tree conformance: {m}")));
    if tree.applied()?.is_some() {
        return fail("a new tree has a version".into());
    }
    if tree.amend(vec![(b"x".to_vec(), Op::Put)]).is_ok() {
        return fail("an amendment before any version was taken".into());
    }
    let model = MemTree::new(1);
    let keys: Vec<Vec<u8>> = [
        &b"a"[..],
        b"a\x00",
        b"a\x00b",
        b"ab",
        b"b",
        b"b\xff",
        b"b\xff\xff",
        b"c",
        b"\x00",
        b"\xff",
    ]
    .iter()
    .map(|k| k.to_vec())
    .collect();
    // A deterministic walk of versions: each puts or deletes some keys.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut step = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for version in [1u64, 2, 3, 5, 8, 9, 10, 20, 21, 30] {
        let mut changes = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..4 {
            let Some(k) = keys.get((step() % keys.len() as u64) as usize).cloned() else {
                continue;
            };
            if !seen.insert(k.clone()) {
                continue;
            }
            let op = if step() % 3 == 0 { Op::Del } else { Op::Put };
            changes.push((k, op));
        }
        tree.apply(version, version * 100, changes.clone())?;
        model.apply(version, version * 100, changes)?;
        if version == 10 {
            let amend = vec![(b"amended".to_vec(), Op::Put)];
            tree.amend(amend.clone())?;
            model.amend(amend)?;
        }
    }
    if tree.apply(30, 0, Vec::new()).is_ok() {
        return fail("a version applied twice was taken".into());
    }
    let check = |t: &dyn KeyTree, what: &str| -> Result<(), StoreError> {
        let mut all = keys.clone();
        all.push(b"amended".to_vec());
        for v in t.floor()?..=30u64 {
            for k in &all {
                if t.get(k, v)? != model.get(k, v)? {
                    return fail(format!("{what}: key {k:?} as of {v}"));
                }
            }
            for p in [&b""[..], b"a", b"a\x00", b"b", b"b\xff", b"z"] {
                if t.scan(p, v)? != model.scan(p, v)? {
                    return fail(format!("{what}: scan of {p:?} as of {v}"));
                }
            }
            for (s, e) in [
                (&b"a"[..], Some(&b"b"[..])),
                (b"a\x00", None),
                (b"", Some(b"c")),
                (b"b", Some(b"b")),
            ] {
                if t.scan_range(s, e, v)? != model.scan_range(s, e, v)? {
                    return fail(format!("{what}: range {s:?}..{e:?} as of {v}"));
                }
            }
            for n in [1usize, 2, 3, 7] {
                let mut got = Vec::new();
                let mut from = Vec::new();
                loop {
                    let page = t.scan_page(&from, None, v, n)?;
                    if page.keys.len() > n {
                        return fail(format!("{what}: a page of {} keys asked for {n}", page.keys.len()));
                    }
                    got.extend(page.keys);
                    match page.next {
                        Some(next) => from = next,
                        None => break,
                    }
                }
                if got != model.scan(b"", v)? {
                    return fail(format!("{what}: pages of {n} as of {v}"));
                }
            }
        }
        if t.applied()? != Some(30) {
            return fail(format!("{what}: applied {:?}", t.applied()?));
        }
        if t.get(b"a", 31).is_ok() {
            return fail(format!("{what}: a read past the newest version was answered"));
        }
        if t.floor()? > 0 && t.get(b"a", t.floor()? - 1).is_ok() {
            return fail(format!("{what}: a read below the floor was answered"));
        }
        Ok(())
    };
    check(tree, "the tree")?;
    let flushed = tree.flush()?;
    if flushed.version() != Some(30) || flushed.mark() != 3000 {
        return fail(format!("a flush covered {:?}/{}", flushed.version(), flushed.mark()));
    }
    tree.raise_floor(5)?;
    if tree.floor()? < 5 {
        return fail("the floor did not rise".into());
    }
    if let Some(reopen) = reopen {
        let again = reopen()?;
        check(&*again, "the tree opened again")?;
        if again.flushed()?.version() != Some(30) {
            return fail("the tree opened again forgot its flush".into());
        }
        if again.key_format()?.is_none() {
            return fail("the tree opened again forgot its key format".into());
        }
    }
    Ok(())
}
