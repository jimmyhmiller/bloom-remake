//! A [`StateStore`] in an S3 bucket (docs/design/STATELESS.md §4.3).
//!
//! S3 has no transaction across keys, so an object's state is a **manifest** and **blobs**, under `PREFIX`:
//!
//! - `o/OBJECT/head`: the manifest: the version, and each entry's key with its value inline (at most
//!   [`INLINE_MAX`] bytes) or the name, length and SHA-256 of the blob holding it. Its version is also in the
//!   `x-amz-meta-blossom-version` header, so a `HEAD` reads it.
//! - `o/OBJECT/b/NAME`: a blob, written once (`If-None-Match: *`) under a name no other write uses (the version it
//!   was written for, this handle's random nonce and a counter), never changed.
//!
//! (`OBJECT` is the object's name percent-encoded, every byte but ASCII letters, digits, `-` and `_`.)
//!
//! **A commit** builds the new manifest from the one at the version it names (a value it does not write keeps its
//! blob), uploads the blobs of the values it writes, then writes the manifest with `If-Match` on the ETag of the one it
//! built from (`If-None-Match: *` for an object's first). S3 refuses it (`412`) when another commit came first: a
//! conflict. This is the conditional write AWS S3 (since 2024), R2, GCS and MinIO support. A manifest write that got no
//! answer is [`StateError::Unknown`].
//!
//! **Garbage.** A commit leaves blobs no manifest names (values overwritten; a commit that lost). [`StateStore::
//! maintain`] lists the blobs of the objects this handle committed to and deletes those the current manifest does not
//! name and that are older than the grace period ([`S3Config::grace`]). A commit names only blobs its base manifest
//! named or blobs it just wrote, so a collector that read any manifest no newer than the current one keeps every blob
//! the current one names, as long as a commit takes less than the grace period (a commit refuses to write its manifest
//! past half of it).
//!
//! **Wake hints** are empty objects `w/AT/OBJECT` (`AT` zero-padded to 20 digits, so a listing is in time order), and
//! **side records** `s/KEY`. `wait` polls the manifest's version with `HEAD`, every [`S3Config::poll`].
//!
//! **URLs**: `s3://BUCKET/PREFIX?region=REGION&endpoint=URL&path_style=true|false&poll_ms=N`; credentials from
//! `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`. Without an endpoint the bucket is AWS's,
//! `https://s3.REGION.amazonaws.com`, addressed by virtual host.

mod client;
pub mod sigv4;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use blossom_statestore::{Commit, Snapshot, StateError, StateStore, Write, check_commit, check_name};
use sha2::{Digest, Sha256};

use client::{Client, Endpoint, Failure, Resp, error_text, parse_time_ms, tag, tags, wall_ms};
pub use sigv4::Credentials;

/// Values of at most this many bytes are kept in the manifest.
pub const INLINE_MAX: usize = 1024;
/// The longest key S3 takes, in bytes.
const MAX_KEY: usize = 1024;
/// The manifest's format.
const MAGIC: &[u8; 4] = b"BLSM";
const FORMAT: u8 = 1;
/// The blob cache's bound.
const BLOB_CACHE_BYTES: usize = 256 << 20;
/// Concurrent blob requests in a load or a commit.
const FETCHERS: usize = 8;
/// Retries of a manifest write that S3 answered `409 ConditionalRequestConflict` (a concurrent write in flight).
const CONFLICT_RETRIES: usize = 5;

/// Where the store is.
#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: Endpoint,
    /// Every key starts with it (empty, or ending in `/`).
    pub prefix: String,
    pub credentials: Credentials,
    /// How often `wait` reads the version.
    pub poll: Duration,
    /// How old an unreferenced blob is before the collector deletes it.
    pub grace: Duration,
}

/// An entry's value, as the manifest holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Inline(Vec<u8>),
    Blob { name: String, len: u64, sha: [u8; 32] },
}

/// An object's manifest.
#[derive(Clone, Debug, Default)]
struct Manifest {
    version: u64,
    /// The manifest's ETag (`None`: the object has none).
    etag: Option<String>,
    entries: BTreeMap<String, Value>,
}

fn put_u32(out: &mut Vec<u8>, n: usize) -> Result<(), StateError> {
    let n = u32::try_from(n).map_err(|_| StateError::Invalid(format!("{n} does not fit a manifest length")))?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

impl Manifest {
    fn encode(&self) -> Result<Vec<u8>, StateError> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(FORMAT);
        out.extend_from_slice(&self.version.to_le_bytes());
        put_u32(&mut out, self.entries.len())?;
        for (k, v) in &self.entries {
            put_u32(&mut out, k.len())?;
            out.extend_from_slice(k.as_bytes());
            match v {
                Value::Inline(b) => {
                    out.push(0);
                    put_u32(&mut out, b.len())?;
                    out.extend_from_slice(b);
                }
                Value::Blob { name, len, sha } => {
                    out.push(1);
                    put_u32(&mut out, name.len())?;
                    out.extend_from_slice(name.as_bytes());
                    out.extend_from_slice(&len.to_le_bytes());
                    out.extend_from_slice(sha);
                }
            }
        }
        let sum = Sha256::digest(&out);
        out.extend_from_slice(&sum);
        Ok(out)
    }

    fn decode(bytes: &[u8], etag: Option<String>) -> Result<Manifest, StateError> {
        let corrupt = |m: &str| StateError::Corrupt(format!("a manifest: {m}"));
        let (body, sum) = bytes
            .split_at_checked(bytes.len().saturating_sub(32))
            .ok_or_else(|| corrupt("too short"))?;
        if Sha256::digest(body).as_slice() != sum {
            return Err(corrupt("its checksum does not match"));
        }
        let mut r = body;
        let mut take = |n: usize| -> Result<&[u8], StateError> {
            let (head, rest) = r.split_at_checked(n).ok_or_else(|| corrupt("it ends early"))?;
            r = rest;
            Ok(head)
        };
        if take(4)? != MAGIC {
            return Err(corrupt("not a manifest"));
        }
        let format = take(1)?.first().copied().unwrap_or(0);
        if format != FORMAT {
            return Err(StateError::Config(format!(
                "a manifest of format {format}; this build reads format {FORMAT}"
            )));
        }
        let u64_at = |b: &[u8]| -> Result<u64, StateError> {
            Ok(u64::from_le_bytes(b.try_into().map_err(|_| corrupt("a short number"))?))
        };
        let u32_at = |b: &[u8]| -> Result<usize, StateError> {
            Ok(u32::from_le_bytes(b.try_into().map_err(|_| corrupt("a short length"))?) as usize)
        };
        let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| corrupt("a name that is not UTF-8"));
        let version = u64_at(take(8)?)?;
        let count = u32_at(take(4)?)?;
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let n = u32_at(take(4)?)?;
            let key = text(take(n)?)?;
            let kind = take(1)?.first().copied().unwrap_or(u8::MAX);
            let value = match kind {
                0 => {
                    let n = u32_at(take(4)?)?;
                    Value::Inline(take(n)?.to_vec())
                }
                1 => {
                    let n = u32_at(take(4)?)?;
                    let name = text(take(n)?)?;
                    let len = u64_at(take(8)?)?;
                    let sha: [u8; 32] = take(32)?.try_into().map_err(|_| corrupt("a short hash"))?;
                    Value::Blob { name, len, sha }
                }
                k => return Err(corrupt(&format!("an entry of kind {k}"))),
            };
            entries.insert(key, value);
        }
        if !take(0)?.is_empty() || !r.is_empty() {
            return Err(corrupt("bytes after its entries"));
        }
        Ok(Manifest { version, etag, entries })
    }

    fn blob_names(&self) -> BTreeSet<&str> {
        self.entries
            .values()
            .filter_map(|v| match v {
                Value::Blob { name, .. } => Some(name.as_str()),
                Value::Inline(_) => None,
            })
            .collect()
    }
}

/// Blobs read, by name (a blob never changes), up to a bound in bytes.
#[derive(Default)]
struct BlobCache {
    blobs: BTreeMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl BlobCache {
    fn get(&self, name: &str) -> Option<Arc<Vec<u8>>> {
        self.blobs.get(name).cloned()
    }

    fn insert(&mut self, name: String, value: Arc<Vec<u8>>) {
        if self.blobs.contains_key(&name) {
            return;
        }
        self.bytes += value.len();
        self.order.push_back(name.clone());
        self.blobs.insert(name, value);
        while self.bytes > BLOB_CACHE_BYTES {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(v) = self.blobs.remove(&old) {
                self.bytes -= v.len();
            }
        }
    }
}

#[derive(Default)]
struct Cache {
    /// The last manifest this handle read or wrote, per object.
    manifests: BTreeMap<String, Arc<Manifest>>,
    blobs: BlobCache,
    /// Objects this handle committed to since the last collection.
    dirty: BTreeSet<String>,
}

/// A state store in an S3 bucket.
pub struct S3Store {
    client: Client,
    cfg: S3Config,
    cache: Mutex<Cache>,
    /// This handle's random nonce and counter, for blob names no other write uses.
    nonce: String,
    counter: AtomicU64,
}

/// An object name, or a key, as one segment of an S3 key.
fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(seg: &str) -> Option<String> {
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = seg.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 16 random bytes from the operating system, hex.
fn os_nonce() -> Result<String, StateError> {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| StateError::Unavailable(format!("reading /dev/urandom: {e}")))?;
    Ok(sigv4::hex(&buf))
}

fn lock<'a>(m: &'a Mutex<Cache>) -> Result<MutexGuard<'a, Cache>, StateError> {
    m.lock()
        .map_err(|_| StateError::Unavailable("the S3 store's cache lock is poisoned".into()))
}

/// Elapsed time for `wait` and a commit's deadline: a host's I/O, not a node's clock.
struct Elapsed(std::time::Instant);

impl Elapsed {
    #[allow(clippy::disallowed_methods)] // a store's I/O deadline, not a node's time
    fn start() -> Elapsed {
        Elapsed(std::time::Instant::now())
    }
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

impl S3Store {
    /// A handle on the store `cfg` names. Nothing is checked until the first request.
    pub fn open(cfg: S3Config) -> Result<S3Store, StateError> {
        if !(cfg.prefix.is_empty() || cfg.prefix.ends_with('/')) {
            return Err(StateError::Config(format!("a prefix ends in `/` (`{}`)", cfg.prefix)));
        }
        Ok(S3Store {
            client: Client::new(cfg.endpoint.clone(), cfg.credentials.clone()),
            cfg,
            cache: Mutex::new(Cache::default()),
            nonce: os_nonce()?,
            counter: AtomicU64::new(0),
        })
    }

    /// The store a URL names (see the crate's documentation), credentials from the environment.
    pub fn from_url(url: &str) -> Result<S3Store, StateError> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let credentials = Credentials {
            access_key: env("AWS_ACCESS_KEY_ID")
                .ok_or_else(|| StateError::Config("AWS_ACCESS_KEY_ID is not set".into()))?,
            secret_key: env("AWS_SECRET_ACCESS_KEY")
                .ok_or_else(|| StateError::Config("AWS_SECRET_ACCESS_KEY is not set".into()))?,
            session_token: env("AWS_SESSION_TOKEN"),
        };
        S3Store::open(parse_url(url, credentials)?)
    }

    /// Creates the bucket (for tests and first setups): done when it exists already and is this account's.
    pub fn create_bucket(&self) -> Result<(), StateError> {
        let region = &self.cfg.endpoint.region;
        let body = if region == "us-east-1" {
            Vec::new()
        } else {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <LocationConstraint>{region}</LocationConstraint></CreateBucketConfiguration>"
            )
            .into_bytes()
        };
        let r = self
            .client
            .send("PUT", "", &[], &[], &body)
            .map_err(Failure::unavailable)?;
        if r.status == 200 || (r.status == 409 && error_text(&r).contains("BucketAlreadyOwnedByYou")) {
            Ok(())
        } else {
            Err(StateError::Unavailable(format!(
                "s3: creating the bucket: {}",
                error_text(&r)
            )))
        }
    }

    fn key(&self, rest: &str) -> Result<String, StateError> {
        let key = format!("{}{rest}", self.cfg.prefix);
        if key.len() > MAX_KEY {
            return Err(StateError::Invalid(format!(
                "the S3 key for this name has {} bytes (at most {MAX_KEY})",
                key.len()
            )));
        }
        Ok(key)
    }

    fn head_key(&self, object: &str) -> Result<String, StateError> {
        self.key(&format!("o/{}/head", encode(object)))
    }

    fn blob_key(&self, object: &str, name: &str) -> Result<String, StateError> {
        self.key(&format!("o/{}/b/{name}", encode(object)))
    }

    fn wake_key(&self, object: &str, at: u64) -> Result<String, StateError> {
        self.key(&format!("w/{at:020}/{}", encode(object)))
    }

    fn side_key(&self, key: &str) -> Result<String, StateError> {
        self.key(&format!("s/{}", encode(key)))
    }

    fn unexpected(what: &str, r: &Resp) -> StateError {
        StateError::Unavailable(format!("s3: {what}: {}", error_text(r)))
    }

    /// The object's current manifest (an empty one at version 0 when it has none).
    fn fetch_manifest(&self, object: &str) -> Result<Manifest, StateError> {
        let r = self
            .client
            .send("GET", &self.head_key(object)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        match r.status {
            200 => Manifest::decode(&r.body, r.etag.clone()),
            404 => Ok(Manifest::default()),
            _ => Err(S3Store::unexpected("reading a manifest", &r)),
        }
    }

    /// A blob's bytes: from the cache, or read (and its hash checked).
    fn blob(&self, object: &str, name: &str, len: u64, sha: &[u8; 32]) -> Result<Arc<Vec<u8>>, StateError> {
        if let Some(b) = lock(&self.cache)?.blobs.get(name) {
            return Ok(b);
        }
        let r = self
            .client
            .send("GET", &self.blob_key(object, name)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        if r.status == 404 {
            return Err(StateError::Corrupt(format!(
                "object `{object}`'s manifest names blob {name}, which is not in the bucket"
            )));
        }
        if r.status != 200 {
            return Err(S3Store::unexpected("reading a blob", &r));
        }
        if r.body.len() as u64 != len || Sha256::digest(&r.body).as_slice() != sha {
            return Err(StateError::Corrupt(format!(
                "blob {name} of `{object}` is not what its manifest says"
            )));
        }
        let b = Arc::new(r.body);
        lock(&self.cache)?.blobs.insert(name.to_owned(), b.clone());
        Ok(b)
    }

    /// Every value of a manifest, its blobs read concurrently.
    fn values(&self, object: &str, m: &Manifest) -> Result<Vec<(String, Vec<u8>)>, StateError> {
        let wanted: Vec<(&String, &String, u64, &[u8; 32])> = m
            .entries
            .iter()
            .filter_map(|(k, v)| match v {
                Value::Blob { name, len, sha } => Some((k, name, *len, sha)),
                Value::Inline(_) => None,
            })
            .collect();
        let mut fetched: BTreeMap<&str, Arc<Vec<u8>>> = BTreeMap::new();
        for chunk in wanted.chunks(FETCHERS) {
            let got: Vec<Result<Arc<Vec<u8>>, StateError>> = std::thread::scope(|s| {
                let hs: Vec<_> = chunk
                    .iter()
                    .map(|(_, name, len, sha)| s.spawn(move || self.blob(object, name, *len, sha)))
                    .collect();
                hs.into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(StateError::Unavailable("a blob reader panicked".into())))
                    })
                    .collect()
            });
            for ((key, ..), b) in chunk.iter().zip(got) {
                fetched.insert(key.as_str(), b?);
            }
        }
        m.entries
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    match v {
                        Value::Inline(b) => b.clone(),
                        Value::Blob { .. } => fetched
                            .get(k.as_str())
                            .map(|b| b.as_ref().clone())
                            .ok_or_else(|| StateError::Unavailable("a blob was not read".into()))?,
                    },
                ))
            })
            .collect()
    }

    /// Writes a blob under a name no other write uses.
    fn put_blob(&self, object: &str, version: u64, value: &[u8]) -> Result<Value, StateError> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let name = format!("{version:020}-{}-{n}", self.nonce);
        let r = self
            .client
            .send(
                "PUT",
                &self.blob_key(object, &name)?,
                &[],
                &[("if-none-match", "*".to_string())],
                value,
            )
            .map_err(Failure::unavailable)?;
        if r.status != 200 {
            return Err(S3Store::unexpected("writing a blob", &r));
        }
        let sha: [u8; 32] = Sha256::digest(value).into();
        lock(&self.cache)?.blobs.insert(name.clone(), Arc::new(value.to_vec()));
        Ok(Value::Blob {
            name,
            len: value.len() as u64,
            sha,
        })
    }

    fn list(&self, prefix: &str, max: usize, mut keep: impl FnMut(&str, &str) -> bool) -> Result<(), StateError> {
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
                ("max-keys".to_string(), max.clamp(1, 1000).to_string()),
            ];
            if let Some(t) = &token {
                query.push(("continuation-token".to_string(), t.clone()));
            }
            let r = self
                .client
                .send("GET", "", &query, &[], &[])
                .map_err(Failure::unavailable)?;
            if r.status != 200 {
                return Err(S3Store::unexpected("listing", &r));
            }
            let xml = String::from_utf8_lossy(&r.body).into_owned();
            for content in tags(&xml, "Contents") {
                let key = tag(content, "Key").unwrap_or_default();
                let modified = tag(content, "LastModified").unwrap_or_default();
                if !keep(&key, &modified) {
                    return Ok(());
                }
            }
            if tag(&xml, "IsTruncated").as_deref() != Some("true") {
                return Ok(());
            }
            token = tag(&xml, "NextContinuationToken");
            if token.is_none() {
                return Err(StateError::Corrupt(
                    "a truncated listing with no continuation token".into(),
                ));
            }
        }
    }

    fn collect(&self, object: &str, now: u64) -> Result<(), StateError> {
        let clock = Elapsed::start();
        let grace_ms = u64::try_from(self.cfg.grace.as_millis()).unwrap_or(u64::MAX);
        // The blobs first, then the manifest: a blob written after the listing is not deleted, and one the manifest
        // names now was named by every manifest since it was written or is younger than the grace period.
        let mut old = Vec::new();
        let dir = self.key(&format!("o/{}/b/", encode(object)))?;
        self.list(&dir, 1000, |key, modified| {
            if let (Some(name), Some(at)) = (key.strip_prefix(&dir), parse_time_ms(modified))
                && now.saturating_sub(at) > grace_ms
            {
                old.push(name.to_owned());
            }
            true
        })?;
        if old.is_empty() {
            return Ok(());
        }
        let current = self.fetch_manifest(object)?;
        let named = current.blob_names();
        for name in old.iter().filter(|n| !named.contains(n.as_str())) {
            if clock.elapsed() > self.cfg.grace / 2 {
                return Err(StateError::Unavailable(format!(
                    "s3: collecting `{object}`'s blobs took past half the grace period; stopped"
                )));
            }
            let r = self
                .client
                .send("DELETE", &self.blob_key(object, name)?, &[], &[], &[])
                .map_err(Failure::unavailable)?;
            if !matches!(r.status, 200 | 204 | 404) {
                return Err(S3Store::unexpected("deleting a blob", &r));
            }
        }
        Ok(())
    }
}

/// An `s3://` URL's configuration.
pub fn parse_url(url: &str, credentials: Credentials) -> Result<S3Config, StateError> {
    let rest = url
        .strip_prefix("s3://")
        .ok_or_else(|| StateError::Config(format!("`{url}` is not an S3 store URL (s3://BUCKET/PREFIX)")))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (bucket, prefix) = path.split_once('/').unwrap_or((path, ""));
    if bucket.is_empty() {
        return Err(StateError::Config(format!("`{url}` names no bucket")));
    }
    let prefix = match prefix.trim_end_matches('/') {
        "" => String::new(),
        p => format!("{p}/"),
    };
    let mut region = "us-east-1".to_string();
    let mut endpoint: Option<String> = None;
    let mut path_style: Option<bool> = None;
    let mut poll = Duration::from_millis(100);
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "region" => region = v.to_string(),
            "endpoint" => endpoint = Some(v.to_string()),
            "path_style" => {
                path_style = Some(match v {
                    "true" => true,
                    "false" => false,
                    _ => return Err(StateError::Config(format!("path_style is true or false, not `{v}`"))),
                })
            }
            "poll_ms" => {
                poll = Duration::from_millis(
                    v.parse()
                        .map_err(|_| StateError::Config(format!("poll_ms is a number, not `{v}`")))?,
                )
            }
            other => {
                return Err(StateError::Config(format!(
                    "an S3 store URL has no parameter `{other}`"
                )));
            }
        }
    }
    let (scheme, host) = match &endpoint {
        Some(e) => {
            let (scheme, host) = e
                .split_once("://")
                .ok_or_else(|| StateError::Config(format!("the endpoint `{e}` is not http://… or https://…")))?;
            if !matches!(scheme, "http" | "https") {
                return Err(StateError::Config(format!("the endpoint `{e}` is not http or https")));
            }
            (scheme.to_string(), host.trim_end_matches('/').to_string())
        }
        None => ("https".to_string(), format!("s3.{region}.amazonaws.com")),
    };
    Ok(S3Config {
        endpoint: Endpoint {
            scheme,
            host,
            bucket: bucket.to_string(),
            region,
            path_style: path_style.unwrap_or(endpoint.is_some()),
        },
        prefix,
        credentials,
        poll,
        grace: Duration::from_secs(600),
    })
}

impl StateStore for S3Store {
    fn load(&self, object: &str) -> Result<Snapshot, StateError> {
        check_name("object name", object)?;
        let m = self.fetch_manifest(object)?;
        let entries = self.values(object, &m)?;
        let version = m.version;
        lock(&self.cache)?.manifests.insert(object.to_owned(), Arc::new(m));
        Ok(Snapshot { version, entries })
    }

    fn version(&self, object: &str) -> Result<u64, StateError> {
        check_name("object name", object)?;
        let r = self
            .client
            .send("HEAD", &self.head_key(object)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        match r.status {
            404 => Ok(0),
            200 => r
                .meta_version
                .as_deref()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| StateError::Corrupt(format!("`{object}`'s manifest carries no version header"))),
            _ => Err(S3Store::unexpected("reading a version", &r)),
        }
    }

    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StateError> {
        check_commit(object, writes)?;
        let head = self.head_key(object)?;
        let clock = Elapsed::start();
        let cached = lock(&self.cache)?.manifests.get(object).cloned();
        let base = match cached {
            Some(m) if m.version == expected => m,
            _ => Arc::new(self.fetch_manifest(object)?),
        };
        if base.version != expected {
            return Ok(Commit::Conflict { current: base.version });
        }
        let next = expected
            .checked_add(1)
            .ok_or_else(|| StateError::Invalid("the version cannot go higher".into()))?;
        // The last write to each key decides it.
        let mut last: BTreeMap<&str, Option<&[u8]>> = BTreeMap::new();
        for w in writes {
            match w {
                Write::Put(k, v) => last.insert(k, Some(v)),
                Write::Delete(k) => last.insert(k, None),
            };
        }
        let mut entries = base.entries.clone();
        let mut uploads: Vec<(&str, &[u8])> = Vec::new();
        for (k, v) in &last {
            match v {
                None => {
                    entries.remove(*k);
                }
                Some(v) if v.len() <= INLINE_MAX => {
                    entries.insert((*k).to_owned(), Value::Inline(v.to_vec()));
                }
                Some(v) => uploads.push((k, v)),
            }
        }
        for chunk in uploads.chunks(FETCHERS) {
            let put: Vec<Result<Value, StateError>> = std::thread::scope(|s| {
                let hs: Vec<_> = chunk
                    .iter()
                    .map(|(_, v)| s.spawn(move || self.put_blob(object, next, v)))
                    .collect();
                hs.into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(StateError::Unavailable("a blob writer panicked".into())))
                    })
                    .collect()
            });
            for ((k, _), v) in chunk.iter().zip(put) {
                entries.insert((*k).to_owned(), v?);
            }
        }
        if clock.elapsed() > self.cfg.grace / 2 {
            // Its blobs may already be collected: writing the manifest could name blobs that are gone.
            return Err(StateError::Unavailable(format!(
                "s3: a commit to `{object}` took past half the grace period; abandoned"
            )));
        }
        let mut manifest = Manifest {
            version: next,
            etag: None,
            entries,
        };
        let body = manifest.encode()?;
        let condition = match &base.etag {
            Some(etag) => ("if-match", etag.clone()),
            None => ("if-none-match", "*".to_string()),
        };
        let headers = [condition, ("x-amz-meta-blossom-version", next.to_string())];
        for _ in 0..=CONFLICT_RETRIES {
            let r = match self.client.send("PUT", &head, &[], &headers, &body) {
                Ok(r) => r,
                Err(Failure::NotSent(m)) => return Err(StateError::Unavailable(format!("s3: {m}"))),
                Err(Failure::NoAnswer(m)) => {
                    lock(&self.cache)?.manifests.remove(object);
                    return Err(StateError::Unknown {
                        object: object.to_owned(),
                        reason: format!("s3: {m}"),
                    });
                }
            };
            match r.status {
                200 => {
                    manifest.etag = r.etag.clone();
                    let mut cache = lock(&self.cache)?;
                    cache.manifests.insert(object.to_owned(), Arc::new(manifest));
                    cache.dirty.insert(object.to_owned());
                    return Ok(Commit::Done { version: next });
                }
                412 => {
                    lock(&self.cache)?.manifests.remove(object);
                    let current = self.version(object)?;
                    if current == expected {
                        return Err(StateError::Unavailable(format!(
                            "s3: the manifest of `{object}` changed under the same version"
                        )));
                    }
                    lock(&self.cache)?.dirty.insert(object.to_owned());
                    return Ok(Commit::Conflict { current });
                }
                // Another conditional write to the manifest is in flight: try again.
                409 => std::thread::sleep(Duration::from_millis(20)),
                s if s >= 500 => {
                    lock(&self.cache)?.manifests.remove(object);
                    return Err(StateError::Unknown {
                        object: object.to_owned(),
                        reason: format!("s3: {}", error_text(&r)),
                    });
                }
                _ => return Err(S3Store::unexpected("writing a manifest", &r)),
            }
        }
        Err(StateError::Unavailable(format!(
            "s3: the manifest of `{object}` stayed in conflict {CONFLICT_RETRIES} times"
        )))
    }

    fn wait(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError> {
        check_name("object name", object)?;
        let clock = Elapsed::start();
        loop {
            let v = self.version(object)?;
            let spent = clock.elapsed();
            if v > since || spent >= timeout {
                return Ok(v);
            }
            std::thread::sleep(self.cfg.poll.min(timeout.saturating_sub(spent)));
        }
    }

    fn schedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let r = self
            .client
            .send("PUT", &self.wake_key(object, at)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        if r.status == 200 {
            Ok(())
        } else {
            Err(S3Store::unexpected("writing a wake hint", &r))
        }
    }

    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StateError> {
        let dir = self.key("w/")?;
        let mut out = Vec::new();
        let mut bad = None;
        self.list(&dir, limit.min(1000), |key, _| {
            let Some(rest) = key.strip_prefix(&dir) else {
                return true;
            };
            let Some((at, obj)) = rest.split_once('/') else {
                return true;
            };
            match (at.parse::<u64>(), decode(obj)) {
                (Ok(at), Some(obj)) => {
                    if at > now || out.len() >= limit {
                        return false;
                    }
                    out.push((obj, at));
                    true
                }
                _ => {
                    bad = Some(key.to_owned());
                    false
                }
            }
        })?;
        match bad {
            Some(k) => Err(StateError::Corrupt(format!("a wake hint the store cannot read: {k}"))),
            None => Ok(out),
        }
    }

    fn unschedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        let r = self
            .client
            .send("DELETE", &self.wake_key(object, at)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        if matches!(r.status, 200 | 204 | 404) {
            Ok(())
        } else {
            Err(S3Store::unexpected("deleting a wake hint", &r))
        }
    }

    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError> {
        check_name("side key", key)?;
        let r = self
            .client
            .send("PUT", &self.side_key(key)?, &[], &[], value)
            .map_err(Failure::unavailable)?;
        if r.status == 200 {
            Ok(())
        } else {
            Err(S3Store::unexpected("writing a side record", &r))
        }
    }

    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError> {
        check_name("side key", key)?;
        let r = self
            .client
            .send("GET", &self.side_key(key)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        match r.status {
            200 => Ok(Some(r.body)),
            404 => Ok(None),
            _ => Err(S3Store::unexpected("reading a side record", &r)),
        }
    }

    fn delete_side(&self, key: &str) -> Result<(), StateError> {
        check_name("side key", key)?;
        let r = self
            .client
            .send("DELETE", &self.side_key(key)?, &[], &[], &[])
            .map_err(Failure::unavailable)?;
        if matches!(r.status, 200 | 204 | 404) {
            Ok(())
        } else {
            Err(S3Store::unexpected("deleting a side record", &r))
        }
    }

    fn maintain(&self, now: u64) -> Result<(), StateError> {
        let dirty = std::mem::take(&mut lock(&self.cache)?.dirty);
        let mut failed = None;
        for object in &dirty {
            if let Err(e) = self.collect(object, now) {
                failed = Some(e);
                lock(&self.cache)?.dirty.insert(object.clone());
            }
        }
        failed.map_or(Ok(()), Err)
    }
}

impl S3Store {
    /// Collects garbage at the wall clock's time (tests and tools).
    pub fn maintain_now(&self) -> Result<(), StateError> {
        self.maintain(wall_ms())
    }

    /// The blob names under an object (tests of the collector).
    pub fn blobs_of(&self, object: &str) -> Result<Vec<String>, StateError> {
        let dir = self.key(&format!("o/{}/b/", encode(object)))?;
        let mut out = Vec::new();
        self.list(&dir, 1000, |key, _| {
            if let Some(n) = key.strip_prefix(&dir) {
                out.push(n.to_owned());
            }
            true
        })?;
        Ok(out)
    }

    /// Sets the collector's grace period (tests).
    pub fn set_grace(&mut self, grace: Duration) {
        self.cfg.grace = grace;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_round_trip_and_reject_damage() {
        let mut entries = BTreeMap::new();
        entries.insert("a".to_string(), Value::Inline(b"x".to_vec()));
        entries.insert(
            "é/b".to_string(),
            Value::Blob {
                name: "n1".into(),
                len: 5,
                sha: [7; 32],
            },
        );
        let m = Manifest {
            version: 9,
            etag: None,
            entries,
        };
        let bytes = m.encode().unwrap();
        let back = Manifest::decode(&bytes, Some("e".into())).unwrap();
        assert_eq!(back.version, 9);
        assert_eq!(back.entries, m.entries);
        let mut damaged = bytes.clone();
        damaged[10] ^= 1;
        assert!(matches!(Manifest::decode(&damaged, None), Err(StateError::Corrupt(_))));
        assert!(matches!(
            Manifest::decode(&bytes[..10], None),
            Err(StateError::Corrupt(_))
        ));
    }

    #[test]
    fn names_encode_to_one_segment() {
        for name in ["a", "a/b", "..", "%2F", "é", "a b", "member/Room/lunch"] {
            let e = encode(name);
            assert!(
                e.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'%'),
                "{e}"
            );
            assert_eq!(decode(&e).as_deref(), Some(name));
        }
        assert_ne!(encode("a/b"), encode("a%2Fb"));
    }

    #[test]
    fn urls() {
        let c = || Credentials {
            access_key: "k".into(),
            secret_key: "s".into(),
            session_token: None,
        };
        let cfg = parse_url("s3://b/p/q?endpoint=http://127.0.0.1:9000&region=r&poll_ms=50", c()).unwrap();
        assert_eq!(
            (
                cfg.endpoint.scheme.as_str(),
                cfg.endpoint.host.as_str(),
                cfg.endpoint.bucket.as_str()
            ),
            ("http", "127.0.0.1:9000", "b")
        );
        assert_eq!((cfg.prefix.as_str(), cfg.endpoint.path_style), ("p/q/", true));
        assert_eq!(cfg.poll, Duration::from_millis(50));
        let aws = parse_url("s3://b", c()).unwrap();
        assert_eq!(aws.endpoint.host, "s3.us-east-1.amazonaws.com");
        assert!(!aws.endpoint.path_style);
        assert_eq!(aws.prefix, "");
        for bad in [
            "s3://",
            "postgres://x",
            "s3://b?nope=1",
            "s3://b?endpoint=ftp://x",
            "s3://b?path_style=yes",
        ] {
            assert!(parse_url(bad, c()).is_err(), "{bad}");
        }
    }
}
