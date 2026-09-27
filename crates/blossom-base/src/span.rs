//! Source positions, the source database, interned identifiers, qualified names and rule labels
//! (ARCHITECTURE §2.1, §13.1; LANGUAGE §4.3).

use std::fmt;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::det::DetSet;
use crate::idx::{FileId, IdxOverflow, IndexVec};

/// A byte range `[lo, hi)` in one source file.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct Span {
    /// The file.
    pub file: FileId,
    /// Start byte offset (inclusive).
    pub lo: u32,
    /// End byte offset (exclusive); `lo <= hi` for a well-formed span.
    pub hi: u32,
}

impl Span {
    /// The span `[lo, hi)` of `file`.
    pub const fn new(file: FileId, lo: u32, hi: u32) -> Span {
        Span { file, lo, hi }
    }

    /// The empty span at `offset`.
    pub const fn point(file: FileId, offset: u32) -> Span {
        Span {
            file,
            lo: offset,
            hi: offset,
        }
    }

    /// The length in bytes (zero for an ill-formed span with `hi < lo`).
    pub const fn len(self) -> u32 {
        self.hi.saturating_sub(self.lo)
    }

    /// Whether the span covers no bytes.
    pub const fn is_empty(self) -> bool {
        self.hi <= self.lo
    }

    /// Whether `offset` lies inside the span.
    pub const fn contains(self, offset: u32) -> bool {
        self.lo <= offset && offset < self.hi
    }

    /// The smallest span covering both, if they are in the same file.
    pub fn to(self, other: Span) -> Option<Span> {
        (self.file == other.file).then(|| Span {
            file: self.file,
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        })
    }
}

/// A 1-based line and column; the column counts Unicode scalar values from the start of the line.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct LineCol {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column, in Unicode scalar values.
    pub column: u32,
}

/// Errors from the [`SourceDb`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The file is not valid UTF-8 (LANGUAGE §2.1); the frontend reports BLS0204.
    #[error("{path}: not valid UTF-8 (first invalid byte at offset {valid_up_to})")]
    InvalidUtf8 {
        /// The file's path.
        path: Arc<str>,
        /// Length of the valid prefix.
        valid_up_to: usize,
    },
    /// The file is larger than a [`Span`] can address (4 GiB).
    #[error("{path}: {len} bytes is larger than the 4 GiB a source file may have")]
    TooLarge {
        /// The file's path.
        path: Arc<str>,
        /// Its length.
        len: usize,
    },
    /// The database has more files than a [`FileId`] can number.
    #[error(transparent)]
    TooManyFiles(#[from] IdxOverflow),
    /// A [`FileId`] from another database.
    #[error("unknown source file {0:?}")]
    UnknownFile(FileId),
    /// An offset past the end of the file.
    #[error("offset {offset} is past the end of {path} ({len} bytes)")]
    OffsetOutOfRange {
        /// The file's path.
        path: Arc<str>,
        /// The offset.
        offset: u32,
        /// The file's length.
        len: u32,
    },
    /// A 1-based line number the file does not have (line 0, or past the last line).
    #[error("{path} has no line {line} (its lines are 1..={lines})")]
    LineOutOfRange {
        /// The file's path.
        path: Arc<str>,
        /// The requested line.
        line: u32,
        /// The number of lines of the file.
        lines: usize,
    },
    /// An offset inside a multi-byte character.
    #[error("offset {offset} of {path} is not on a character boundary")]
    NotCharBoundary {
        /// The file's path.
        path: Arc<str>,
        /// The offset.
        offset: u32,
    },
    /// A span whose end precedes its start.
    #[error("span {lo}..{hi} of {path} ends before it starts")]
    InvertedSpan {
        /// The file's path.
        path: Arc<str>,
        /// Start.
        lo: u32,
        /// End.
        hi: u32,
    },
}

/// One file of a [`SourceDb`].
#[derive(Debug, Clone)]
struct SourceFile {
    path: Arc<str>,
    text: Arc<str>,
    /// Byte offset of the start of every line; `line_starts[0] == 0`.
    line_starts: Vec<u32>,
}

/// The text of every source file of a compilation, with line and column mapping (ARCHITECTURE §13.1).
///
/// Files are UTF-8 checked when added and are at most 4 GiB, so every [`Span`] offset fits in `u32`.
#[derive(Debug, Clone, Default)]
pub struct SourceDb {
    files: IndexVec<FileId, SourceFile>,
}

impl SourceDb {
    /// An empty database.
    pub fn new() -> SourceDb {
        SourceDb::default()
    }

    /// Adds a file read as bytes; fails if it is not UTF-8 or too large.
    pub fn add_file(&mut self, path: impl Into<Arc<str>>, bytes: Vec<u8>) -> Result<FileId, SourceError> {
        let path = path.into();
        match String::from_utf8(bytes) {
            Ok(text) => self.add_text(path, text),
            Err(e) => Err(SourceError::InvalidUtf8 {
                path,
                valid_up_to: e.utf8_error().valid_up_to(),
            }),
        }
    }

    /// Adds a file already known to be text; fails if it is too large.
    pub fn add_text(&mut self, path: impl Into<Arc<str>>, text: impl Into<Arc<str>>) -> Result<FileId, SourceError> {
        let path = path.into();
        let text: Arc<str> = text.into();
        if u32::try_from(text.len()).is_err() {
            return Err(SourceError::TooLarge { path, len: text.len() });
        }
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                // Fits: the whole text is shorter than u32::MAX bytes (checked above).
                line_starts.push(i as u32 + 1);
            }
        }
        Ok(self.files.push(SourceFile {
            path,
            text,
            line_starts,
        })?)
    }

    /// The number of files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the database holds no file.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Every file id, in insertion order.
    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.indices()
    }

    /// The first file with the given path.
    pub fn find(&self, path: &str) -> Option<FileId> {
        self.files
            .iter_enumerated()
            .find(|(_, f)| &*f.path == path)
            .map(|(id, _)| id)
    }

    fn file(&self, file: FileId) -> Result<&SourceFile, SourceError> {
        self.files.get(file).ok_or(SourceError::UnknownFile(file))
    }

    /// The path a file was added with.
    pub fn path(&self, file: FileId) -> Result<&Arc<str>, SourceError> {
        Ok(&self.file(file)?.path)
    }

    /// The full text of a file.
    pub fn text(&self, file: FileId) -> Result<&Arc<str>, SourceError> {
        Ok(&self.file(file)?.text)
    }

    /// The number of lines (a file always has at least one, possibly empty, line).
    pub fn line_count(&self, file: FileId) -> Result<usize, SourceError> {
        Ok(self.file(file)?.line_starts.len())
    }

    /// The line and column of a byte offset. `offset` may equal the file length (the end of the file).
    pub fn line_col(&self, file: FileId, offset: u32) -> Result<LineCol, SourceError> {
        let f = self.file(file)?;
        let len = f.text.len() as u32; // fits: checked in add_text
        if offset > len {
            return Err(SourceError::OffsetOutOfRange {
                path: f.path.clone(),
                offset,
                len,
            });
        }
        if !f.text.is_char_boundary(offset as usize) {
            return Err(SourceError::NotCharBoundary {
                path: f.path.clone(),
                offset,
            });
        }
        // The last line start that is <= offset; line_starts[0] == 0 <= offset, so the index is at least 1.
        let line_index = f
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let start = f.line_starts.get(line_index).copied().unwrap_or(0);
        let prefix = f.text.get(start as usize..offset as usize).unwrap_or("");
        let column = prefix.chars().count() as u32 + 1; // fits: prefix is shorter than the file
        Ok(LineCol {
            line: line_index as u32 + 1,
            column,
        })
    }

    /// The text covered by a span.
    pub fn span_text(&self, span: Span) -> Result<&str, SourceError> {
        let f = self.file(span.file)?;
        if span.hi < span.lo {
            return Err(SourceError::InvertedSpan {
                path: f.path.clone(),
                lo: span.lo,
                hi: span.hi,
            });
        }
        let len = f.text.len() as u32; // fits: checked in add_text
        for offset in [span.lo, span.hi] {
            if offset > len {
                return Err(SourceError::OffsetOutOfRange {
                    path: f.path.clone(),
                    offset,
                    len,
                });
            }
            if !f.text.is_char_boundary(offset as usize) {
                return Err(SourceError::NotCharBoundary {
                    path: f.path.clone(),
                    offset,
                });
            }
        }
        f.text
            .get(span.lo as usize..span.hi as usize)
            .ok_or_else(|| SourceError::NotCharBoundary {
                path: f.path.clone(),
                offset: span.lo,
            })
    }

    /// The text of 1-based line `line`, without its line terminator (`\n` or `\r\n`).
    pub fn line_text(&self, file: FileId, line: u32) -> Result<&str, SourceError> {
        let f = self.file(file)?;
        let len = f.text.len() as u32; // fits: checked in add_text
        let index = (line as usize).checked_sub(1);
        let start = index.and_then(|i| f.line_starts.get(i)).copied();
        let Some(start) = start else {
            return Err(SourceError::LineOutOfRange {
                path: f.path.clone(),
                line,
                lines: f.line_starts.len(),
            });
        };
        let end = index
            .and_then(|i| f.line_starts.get(i + 1))
            .map_or(len, |next| next - 1);
        // Line starts follow a '\n' and a line ends at one, so both are character boundaries.
        let text = f
            .text
            .get(start as usize..end as usize)
            .ok_or_else(|| SourceError::NotCharBoundary {
                path: f.path.clone(),
                offset: start,
            })?;
        Ok(text.strip_suffix('\r').unwrap_or(text))
    }
}

/// An interned identifier.
///
/// Interning is global and thread-safe, and a symbol's text lives for the whole process. Symbols have no
/// observable id: they compare, order, hash and serialize by their text, so no result depends on interning order.
/// Equality is a pointer comparison, because every symbol with a given text is the same interned string.
#[derive(Copy, Clone)]
pub struct Symbol(&'static str);

fn interner() -> &'static RwLock<DetSet<&'static str>> {
    static INTERNER: OnceLock<RwLock<DetSet<&'static str>>> = OnceLock::new();
    INTERNER.get_or_init(|| RwLock::new(DetSet::new()))
}

impl Symbol {
    /// Interns `text`.
    pub fn intern(text: &str) -> Symbol {
        // The interner holds plain data and every operation leaves it consistent, so a lock poisoned by a panic in
        // another thread is safe to keep using.
        {
            let guard = interner().read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&interned) = guard.get(text) {
                return Symbol(interned);
            }
        }
        let mut guard = interner().write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&interned) = guard.get(text) {
            return Symbol(interned);
        }
        // Symbols live for the whole process, like the program text they name.
        let interned: &'static str = Box::leak(text.to_owned().into_boxed_str());
        guard.insert(interned);
        Symbol(interned)
    }

    /// The symbol's text.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool {
        // Interned: equal texts are the same allocation.
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Symbol {}

impl std::hash::Hash for Symbol {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialOrd for Symbol {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Symbol {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self == other {
            std::cmp::Ordering::Equal
        } else {
            self.0.cmp(other.0)
        }
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Symbol {
    fn from(text: &str) -> Symbol {
        Symbol::intern(text)
    }
}

impl Serialize for Symbol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Ok(Symbol::intern(&text))
    }
}

/// A path of instance segments plus a final name, e.g. `chat.data.msg`. Generated names contain `$`, which no
/// surface identifier can contain. Printed with `.` between segments; ordered segment by segment by text.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QualName(pub Arc<[Symbol]>);

impl QualName {
    /// A name from its segments.
    pub fn new(segments: impl IntoIterator<Item = Symbol>) -> QualName {
        QualName(segments.into_iter().collect())
    }

    /// A one-segment name.
    pub fn single(name: Symbol) -> QualName {
        QualName(Arc::from([name]))
    }

    /// Parses `a.b.c`; `None` if any segment is empty.
    pub fn parse_dotted(text: &str) -> Option<QualName> {
        let segments: Vec<Symbol> = text
            .split('.')
            .map(|s| (!s.is_empty()).then(|| Symbol::intern(s)))
            .collect::<Option<_>>()?;
        Some(QualName(segments.into()))
    }

    /// The segments.
    pub fn segments(&self) -> &[Symbol] {
        &self.0
    }

    /// The final segment, if any.
    pub fn last(&self) -> Option<Symbol> {
        self.0.last().copied()
    }

    /// The name without its final segment, if it has more than one.
    pub fn parent(&self) -> Option<QualName> {
        match self.0.split_last() {
            Some((_, rest)) if !rest.is_empty() => Some(QualName(rest.into())),
            _ => None,
        }
    }

    /// This name with one more segment.
    pub fn child(&self, name: Symbol) -> QualName {
        QualName(self.0.iter().copied().chain(std::iter::once(name)).collect())
    }

    /// Whether the name was generated by the compiler (some segment contains `$`).
    pub fn is_generated(&self) -> bool {
        self.0.iter().any(|s| s.as_str().contains('$'))
    }
}

impl fmt::Display for QualName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(segment.as_str())?;
        }
        Ok(())
    }
}

impl fmt::Debug for QualName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QualName({self})")
    }
}

/// SipHash-1-3 with key 0 over `bytes`: the stable hash of LANGUAGE §4.3 (rule labels, `h#XXXXXXXX` ids).
pub fn stable_hash(bytes: &[u8]) -> u64 {
    siphasher::sip::SipHasher13::new_with_keys(0, 0).hash(bytes)
}

/// The first 8 hex digits of [`stable_hash`], the `XXXXXXXX` of LANGUAGE §4.3's generated ids.
pub fn stable_hash_hex8(bytes: &[u8]) -> String {
    format!("{:08x}", stable_hash(bytes) >> 32)
}

/// A stable, source-order-independent rule identity (LANGUAGE §4.3), e.g. `raft::grant_vote/send:vote`.
///
/// `hash` is [`stable_hash`] of `text`; it is derived, so labels compare, order and serialize by their text.
#[derive(Clone)]
pub struct RuleLabel {
    /// The label text.
    pub text: Arc<str>,
    /// SipHash-1-3 (key 0) of `text`.
    pub hash: u64,
}

impl RuleLabel {
    /// The label with the given text.
    pub fn new(text: impl Into<Arc<str>>) -> RuleLabel {
        let text = text.into();
        let hash = stable_hash(text.as_bytes());
        RuleLabel { text, hash }
    }
}

impl PartialEq for RuleLabel {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for RuleLabel {}

impl PartialOrd for RuleLabel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RuleLabel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

impl std::hash::Hash for RuleLabel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl fmt::Debug for RuleLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RuleLabel({:?}, {:#018x})", self.text, self.hash)
    }
}

impl fmt::Display for RuleLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for RuleLabel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for RuleLabel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(RuleLabel::new(String::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_orders_by_text() {
        // Intern in reverse alphabetical order: ordering must follow the text, never interning order.
        let z = Symbol::intern("zeta-symbol-order-test");
        let m = Symbol::intern("mu-symbol-order-test");
        let a = Symbol::intern("alpha-symbol-order-test");
        let mut v = vec![z, a, m];
        v.sort();
        assert_eq!(v, vec![a, m, z]);
        assert!(a < m && m < z);
        assert_eq!(Symbol::intern("mu-symbol-order-test"), m);
        assert_eq!(m.as_str(), "mu-symbol-order-test");
        assert_eq!(format!("{m} {m:?}"), "mu-symbol-order-test \"mu-symbol-order-test\"");
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"mu-symbol-order-test\"");
        assert_eq!(serde_json::from_str::<Symbol>(&json).unwrap(), m);
    }

    #[test]
    fn symbol_interning_is_thread_safe() {
        let handles: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    (0..200)
                        .map(|i| Symbol::intern(&format!("sym{}", (i + t) % 50)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            for s in h.join().unwrap() {
                assert_eq!(Symbol::intern(s.as_str()), s);
            }
        }
    }

    #[test]
    fn source_db_line_col() {
        let mut db = SourceDb::new();
        let f = db.add_text("a.bls", "ab\nλx\r\n\nend").unwrap();
        assert_eq!(db.path(f).unwrap().as_ref(), "a.bls");
        assert_eq!(db.line_count(f).unwrap(), 4);
        let lc = |o| db.line_col(f, o).unwrap();
        assert_eq!(lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(lc(2), LineCol { line: 1, column: 3 }); // the '\n'
        assert_eq!(lc(3), LineCol { line: 2, column: 1 }); // 'λ' (2 bytes)
        assert_eq!(lc(5), LineCol { line: 2, column: 2 }); // 'x': columns count characters, not bytes
        assert_eq!(lc(8), LineCol { line: 3, column: 1 });
        assert_eq!(lc(12), LineCol { line: 4, column: 4 }); // end of file
        assert!(matches!(
            db.line_col(f, 4),
            Err(SourceError::NotCharBoundary { offset: 4, .. })
        ));
        assert!(matches!(
            db.line_col(f, 13),
            Err(SourceError::OffsetOutOfRange {
                offset: 13,
                len: 12,
                ..
            })
        ));
        assert_eq!(db.line_text(f, 2).unwrap(), "λx");
        assert_eq!(db.line_text(f, 3).unwrap(), "");
        assert_eq!(db.line_text(f, 4).unwrap(), "end");
        for line in [0, 5] {
            assert_eq!(
                db.line_text(f, line),
                Err(SourceError::LineOutOfRange {
                    path: "a.bls".into(),
                    line,
                    lines: 4
                })
            );
        }
        assert_eq!(
            db.line_text(f, 5).unwrap_err().to_string(),
            "a.bls has no line 5 (its lines are 1..=4)"
        );
        assert_eq!(db.span_text(Span::new(f, 3, 6)).unwrap(), "λx");
        assert!(matches!(
            db.span_text(Span::new(f, 6, 3)),
            Err(SourceError::InvertedSpan { .. })
        ));
        assert!(matches!(
            db.line_col(FileId::from_raw(9), 0),
            Err(SourceError::UnknownFile(_))
        ));
        assert_eq!(db.find("a.bls"), Some(f));
    }

    #[test]
    fn source_db_rejects_invalid_utf8() {
        let mut db = SourceDb::new();
        let err = db.add_file("bad.bls", vec![b'o', b'k', 0xff, b'!']).unwrap_err();
        assert_eq!(
            err,
            SourceError::InvalidUtf8 {
                path: "bad.bls".into(),
                valid_up_to: 2
            }
        );
        assert!(db.is_empty());
        let ok = db.add_file("good.bls", b"fine".to_vec()).unwrap();
        assert_eq!(db.text(ok).unwrap().as_ref(), "fine");
        assert_eq!(db.files().collect::<Vec<_>>(), vec![ok]);
    }

    #[test]
    fn span_helpers() {
        let f = FileId::from_raw(1);
        let s = Span::new(f, 4, 9);
        assert_eq!(
            (s.len(), s.is_empty(), s.contains(4), s.contains(9)),
            (5, false, true, false)
        );
        assert_eq!(s.to(Span::point(f, 12)), Some(Span::new(f, 4, 12)));
        assert_eq!(s.to(Span::point(FileId::from_raw(2), 0)), None);
    }

    #[test]
    fn qual_name_printing_and_order() {
        let q = QualName::parse_dotted("chat.data.msg").unwrap();
        assert_eq!(q.to_string(), "chat.data.msg");
        assert_eq!(q.last(), Some(Symbol::intern("msg")));
        assert_eq!(q.parent().unwrap().to_string(), "chat.data");
        assert_eq!(QualName::single(Symbol::intern("x")).parent(), None);
        assert_eq!(q.parent().unwrap().child(Symbol::intern("msg")), q);
        assert!(!q.is_generated());
        assert!(QualName::parse_dotted("M.receive$when").unwrap().is_generated());
        assert_eq!(QualName::parse_dotted("a..b"), None);
        assert!(QualName::parse_dotted("a.b").unwrap() < QualName::parse_dotted("a.c").unwrap());
        assert!(QualName::parse_dotted("a").unwrap() < QualName::parse_dotted("a.b").unwrap());
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(json, r#"["chat","data","msg"]"#);
        assert_eq!(serde_json::from_str::<QualName>(&json).unwrap(), q);
    }

    #[test]
    fn rule_label_hash_is_siphash13_key0() {
        // Reference vector: SipHash-1-3 with a zero key over the empty input.
        assert_eq!(stable_hash(b""), 0xd1fb_a762_150c_532c);
        let l = RuleLabel::new("raft::grant_vote/send:vote");
        // Computed with an independent SipHash-1-3 implementation (validated against the reference SipHash-2-4 vector).
        assert_eq!(l.hash, 0x9a08_f682_c16a_39f5);
        assert_eq!(stable_hash_hex8(b""), "d1fba762");
        let json = serde_json::to_string(&l).unwrap();
        assert_eq!(json, "\"raft::grant_vote/send:vote\"");
        let back: RuleLabel = serde_json::from_str(&json).unwrap();
        assert_eq!((back.text.as_ref(), back.hash), (l.text.as_ref(), l.hash));
        assert!(RuleLabel::new("a") < RuleLabel::new("b"));
    }
}
