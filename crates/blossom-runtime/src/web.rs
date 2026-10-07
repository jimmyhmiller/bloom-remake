//! HTTP/1.1 and WebSocket (RFC 6455) for `blossom run --web` (docs/design/CLIENTS.md §4): the page's files, the
//! program and deployment the page compiles (`/blossom/app.json`), and the WebSocket a client member's link runs
//! over (`/blossom/link`, [`crate::members`]).
//!
//! One request per connection (`Connection: close`), on a thread of its own like the other listeners. A WebSocket
//! carries one link frame (blossom-wire) per binary message.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::RuntimeError;

/// The most bytes of a request's head, and of one WebSocket message.
const MAX_HEAD: usize = 16 * 1024;
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// A request's method, path (without its query) and headers (names lowercased).
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Reads a request's head.
pub fn read_request(r: &mut impl BufRead) -> Result<Request, RuntimeError> {
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        // At most what is left of the head's budget (and one byte more, to tell a head over it): a line without its
        // end cannot grow past it.
        let budget = (MAX_HEAD - total + 1) as u64;
        let n = r.by_ref().take(budget).read_line(&mut line).map_err(RuntimeError::Io)?;
        if n == 0 {
            return Err(RuntimeError::Net("the connection closed inside a request".into()));
        }
        total += n;
        if total > MAX_HEAD {
            return Err(RuntimeError::Net("a request head over 16 KiB".into()));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut it = lines.into_iter();
    let first = it.next().ok_or_else(|| RuntimeError::Net("an empty request".into()))?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(RuntimeError::Net(format!("a malformed request line `{first}`")));
    };
    let path = target.split(['?', '#']).next().unwrap_or("/").to_owned();
    let mut headers = BTreeMap::new();
    for l in it {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    Ok(Request {
        method: method.to_owned(),
        path,
        headers,
    })
}

/// Writes a whole response and closes the exchange.
pub fn respond(
    w: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), RuntimeError> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    w.write_all(head.as_bytes()).map_err(RuntimeError::Io)?;
    w.write_all(body).map_err(RuntimeError::Io)?;
    w.flush().map_err(RuntimeError::Io)
}

/// The `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key` (RFC 6455 §4.2.2).
pub fn accept_key(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.trim().as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64(&h.finalize())
}

/// Answers a WebSocket upgrade, or refuses a request that is not one (400).
pub fn upgrade(w: &mut TcpStream, req: &Request) -> Result<(), RuntimeError> {
    let is_upgrade = req
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        && req.header("sec-websocket-version") == Some("13");
    let Some(key) = req.header("sec-websocket-key").filter(|_| is_upgrade) else {
        respond(
            w,
            400,
            "Bad Request",
            "text/plain",
            b"a WebSocket upgrade (version 13) is expected here",
        )?;
        return Err(RuntimeError::Net(
            "a request for the link that is not a WebSocket upgrade".into(),
        ));
    };
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    );
    w.write_all(head.as_bytes()).map_err(RuntimeError::Io)?;
    w.flush().map_err(RuntimeError::Io)
}

/// A received WebSocket message.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Binary(Vec<u8>),
    Text(String),
    Ping(Vec<u8>),
    Pong,
    Close,
}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Reads the next message, assembling fragments; client frames must be masked (RFC 6455 §5.1).
pub fn read_message(r: &mut impl Read) -> Result<Message, RuntimeError> {
    let mut data = Vec::new();
    let mut kind: Option<u8> = None;
    loop {
        let mut head = [0u8; 2];
        r.read_exact(&mut head).map_err(RuntimeError::Io)?;
        let fin = head[0] & 0x80 != 0;
        let op = head[0] & 0x0F;
        if head[0] & 0x70 != 0 {
            return Err(RuntimeError::Net("a WebSocket frame with reserved bits set".into()));
        }
        if head[1] & 0x80 == 0 {
            return Err(RuntimeError::Net("an unmasked WebSocket frame from a client".into()));
        }
        let len = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).map_err(RuntimeError::Io)?;
                u64::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).map_err(RuntimeError::Io)?;
                u64::from_be_bytes(b)
            }
            n => u64::from(n),
        };
        let len = usize::try_from(len).map_err(|_| RuntimeError::Net("a WebSocket frame too large".into()))?;
        if data.len().saturating_add(len) > MAX_MESSAGE {
            return Err(RuntimeError::Net("a WebSocket message over 16 MiB".into()));
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask).map_err(RuntimeError::Io)?;
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).map_err(RuntimeError::Io)?;
        for (b, m) in payload.iter_mut().zip(mask.iter().cycle()) {
            *b ^= m;
        }
        match op {
            OP_PING => return Ok(Message::Ping(payload)),
            OP_PONG => return Ok(Message::Pong),
            OP_CLOSE => return Ok(Message::Close),
            OP_TEXT | OP_BINARY if kind.is_none() => kind = Some(op),
            OP_CONT if kind.is_some() => {}
            other => return Err(RuntimeError::Net(format!("an unexpected WebSocket opcode {other:#x}"))),
        }
        data.extend_from_slice(&payload);
        if fin {
            return Ok(match kind {
                Some(OP_TEXT) => Message::Text(
                    String::from_utf8(data)
                        .map_err(|_| RuntimeError::Net("a text message that is not UTF-8".into()))?,
                ),
                _ => Message::Binary(data),
            });
        }
    }
}

/// Writes one unmasked, unfragmented frame (a server's, RFC 6455 §5.1).
pub fn write_frame(w: &mut impl Write, op: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut head = vec![0x80 | op];
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n <= usize::from(u16::MAX) => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

/// A binary message.
pub fn write_binary(w: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    write_frame(w, OP_BINARY, payload)
}

/// A pong answering a ping.
pub fn write_pong(w: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    write_frame(w, OP_PONG, payload)
}

/// A close frame.
pub fn write_close(w: &mut impl Write) -> std::io::Result<()> {
    write_frame(w, OP_CLOSE, &[])
}

/// The content type of a served file, by extension.
pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("bls" | "txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file under `root` that a request path names (`/` is `index.html`), or `None` for a path that leaves `root`
/// or names no file.
pub fn file_of(root: &Path, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(request_path)?;
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let rel = Path::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let path = root.join(rel);
    path.is_file().then_some(path)
}

/// A request path with its `%xx` escapes decoded; `None` for a malformed escape or a result that is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `/blossom/app.json` (docs/design/CLIENTS.md §4): what the page compiles (the program's files, keyed by their path
/// from the root file's directory, and the deployment's parameters), the deployment it compiles for, the connection
/// identity, this node's name and where the link is.
pub fn app_json(
    spec: &crate::deploy::DeploymentSpec,
    sources: &blossom_base::SourceDb,
    node: &str,
) -> Result<String, String> {
    use serde_json::{Map, Value, json};
    let root = spec.source.to_str().ok_or("the program path is not UTF-8")?;
    // Paths compare canonically (a deployment's `source` may hold `..` segments the loader resolved).
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let base = canonical(spec.source.parent().unwrap_or(Path::new("")));
    let key = |path: &str| {
        canonical(Path::new(path))
            .strip_prefix(&base)
            .ok()
            .and_then(|p| p.to_str())
            .map_or_else(|| path.to_owned(), |p| p.replace('\\', "/"))
    };
    let mut files = Map::new();
    for f in sources.files() {
        let (path, text) = (
            sources.path(f).map_err(|e| e.to_string())?,
            sources.text(f).map_err(|e| e.to_string())?,
        );
        files.insert(key(path), Value::String(text.to_string()));
    }
    let hex = |b: [u8; 16]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let params: Map<String, Value> = spec
        .params
        .iter()
        .map(|(k, v)| {
            use crate::deploy::ParamValue as V;
            let v = match v {
                V::Int(n) => json!({ "int": n }),
                V::Bool(b) => json!({ "bool": b }),
                V::Text(t) => json!({ "text": t }),
            };
            (k.clone(), v)
        })
        .collect();
    let nodes: Vec<Value> = spec
        .nodes
        .iter()
        .map(|n| json!({ "name": n.name, "role": n.role }))
        .collect();
    let app = json!({
        "root": key(root),
        "files": files,
        "params": params,
        "nodes": nodes,
        "node": node,
        "deployment": hex(spec.deployment_id()),
        "directory": hex(spec.directory_digest()),
        "link": "/blossom/link",
    });
    serde_json::to_string(&app).map_err(|e| e.to_string())
}

/// The base64 symbol of a 6-bit value (RFC 4648 table 1).
fn b64_symbol(v: u32) -> char {
    let v = (v & 63) as u8;
    char::from(match v {
        0..=25 => b'A' + v,
        26..=51 => b'a' + (v - 26),
        52..=61 => b'0' + (v - 52),
        62 => b'+',
        _ => b'/',
    })
}

/// Standard base64 with padding (RFC 4648 §4).
pub fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        let sym = |shift: u32| b64_symbol(n >> shift);
        out.push(sym(18));
        out.push(sym(12));
        out.push(if chunk.len() > 1 { sym(6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_key_is_rfc_6455s_example() {
        // RFC 6455 §1.3.
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn masked_fragments_assemble_and_server_frames_read_back() {
        let mask = [1u8, 2, 3, 4];
        let frame = |fin: bool, op: u8, payload: &[u8]| {
            let mut f = vec![if fin { 0x80 } else { 0 } | op, 0x80 | payload.len() as u8];
            f.extend_from_slice(&mask);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            f
        };
        let mut bytes = frame(false, OP_BINARY, b"hel");
        bytes.extend(frame(true, OP_CONT, b"lo"));
        bytes.extend(frame(true, OP_PING, b"p"));
        let mut r = bytes.as_slice();
        assert_eq!(read_message(&mut r).unwrap(), Message::Binary(b"hello".to_vec()));
        assert_eq!(read_message(&mut r).unwrap(), Message::Ping(b"p".to_vec()));
        // An unmasked frame is refused.
        let mut out = Vec::new();
        write_binary(&mut out, &[7; 300]).unwrap();
        assert_eq!(out.get(..4), Some([0x82, 126, 1, 44].as_slice()));
        assert!(read_message(&mut out.as_slice()).is_err());
    }

    #[test]
    fn a_request_head_is_read_within_its_budget() {
        let req = read_request(&mut "GET /a?b=c HTTP/1.1\r\nUpgrade: WebSocket\r\n\r\n".as_bytes()).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("GET", "/a"));
        assert_eq!(req.header("upgrade"), Some("WebSocket"));
        // A line without its end is not read past the head's budget.
        let endless = vec![b'a'; 4 * MAX_HEAD];
        let mut r = endless.as_slice();
        assert!(read_request(&mut r).is_err());
        assert_eq!(r.len(), 4 * MAX_HEAD - MAX_HEAD - 1);
    }

    #[test]
    fn files_stay_under_the_root() {
        let root = std::env::temp_dir().join(format!("blossom-web-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("index.html"), "x").unwrap();
        std::fs::write(root.join("pkg/a b.js"), "x").unwrap();
        assert_eq!(file_of(&root, "/"), Some(root.join("index.html")));
        assert_eq!(file_of(&root, "/pkg/a%20b.js"), Some(root.join("pkg/a b.js")));
        assert_eq!(file_of(&root, "/../etc/passwd"), None);
        assert_eq!(file_of(&root, "/pkg/%2e%2e/index.html"), None);
        assert_eq!(file_of(&root, "/missing.js"), None);
        assert_eq!(content_type(Path::new("x.wasm")), "application/wasm");
        let _ = std::fs::remove_dir_all(&root);
    }
}
