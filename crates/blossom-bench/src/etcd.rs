//! etcd as a [`KvStore`], for the comparison: the v3 API through etcd's JSON gateway (`POST /v3/kv/put`,
//! `/v3/kv/range`, `/v3/kv/deleterange`) over a kept-alive HTTP/1.1 connection per session.
//!
//! Reads are etcd's default linearizable range (not `serializable`), so the history checker applies to etcd too.
//! The gateway translates JSON to gRPC inside etcd, which costs something; etcd's own gRPC `benchmark` tool gives
//! its best case alongside (docs/plan/notes on the comparison).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::kv::{KvSession, KvStore};

/// Sessions to an etcd cluster's client URLs; client `c` uses endpoint `c mod n`.
pub struct EtcdStore {
    pub endpoints: Vec<SocketAddr>,
    pub timeout: Duration,
}

struct EtcdSession {
    host: String,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl KvStore for EtcdStore {
    fn connect(&self, client: usize) -> Result<Box<dyn KvSession>, String> {
        let addr = self
            .endpoints
            .get(client % self.endpoints.len().max(1))
            .ok_or_else(|| "no etcd endpoints".to_string())?;
        let stream = TcpStream::connect_timeout(addr, self.timeout).map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(self.timeout)).map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(Box::new(EtcdSession {
            host: addr.to_string(),
            reader: BufReader::new(stream),
            writer,
        }))
    }
}

impl EtcdSession {
    /// One POST with a JSON body; returns the response body.
    fn post(&mut self, path: &str, body: &str) -> Result<serde_json::Value, String> {
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.host,
            body.len()
        );
        self.writer.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut status = String::new();
        self.reader.read_line(&mut status).map_err(|e| e.to_string())?;
        if status.is_empty() {
            return Err("etcd closed the connection".into());
        }
        let ok = status.split_whitespace().nth(1) == Some("200");
        let mut length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).map_err(|e| e.to_string())?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                length = v.trim().parse().ok();
            } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                self.reader.read_line(&mut size).map_err(|e| e.to_string())?;
                let n = usize::from_str_radix(size.trim(), 16).map_err(|e| format!("chunk size: {e}"))?;
                let mut chunk = vec![0u8; n + 2];
                self.reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                chunk.truncate(n);
                body.extend_from_slice(&chunk);
            }
        } else {
            let n = length.ok_or_else(|| "a response without a length".to_string())?;
            body.resize(n, 0);
            self.reader.read_exact(&mut body).map_err(|e| e.to_string())?;
        }
        let text = String::from_utf8_lossy(&body);
        if !ok {
            return Err(format!("etcd: {} {text}", status.trim_end()));
        }
        serde_json::from_slice(&body).map_err(|e| format!("etcd response {text}: {e}"))
    }
}

impl KvSession for EtcdSession {
    fn put(&mut self, key: &[u8], val: &[u8]) -> Result<(), String> {
        let body = format!(r#"{{"key":"{}","value":"{}"}}"#, b64(key), b64(val));
        self.post("/v3/kv/put", &body).map(|_| ())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let body = format!(r#"{{"key":"{}"}}"#, b64(key));
        let r = self.post("/v3/kv/range", &body)?;
        match r.get("kvs").and_then(|k| k.as_array()).and_then(|a| a.first()) {
            None => Ok(None),
            // An empty value is omitted from the JSON.
            Some(kv) => match kv.get("value").and_then(|v| v.as_str()) {
                Some(v) => unb64(v).map(Some),
                None => Ok(Some(Vec::new())),
            },
        }
    }

    fn delete(&mut self, key: &[u8]) -> Result<bool, String> {
        let body = format!(r#"{{"key":"{}"}}"#, b64(key));
        let r = self.post("/v3/kv/deleterange", &body)?;
        // int64 fields are JSON strings; zero is omitted.
        let deleted = match r.get("deleted") {
            None => 0,
            Some(serde_json::Value::String(s)) => s.parse::<u64>().map_err(|e| e.to_string())?,
            Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(other) => return Err(format!("deleted = {other}")),
        };
        Ok(deleted > 0)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let idx = (n >> (18 - 6 * i)) & 63;
                out.push(char::from(B64.get(idx as usize).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unb64(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = B64
            .iter()
            .position(|x| *x == c)
            .ok_or_else(|| format!("invalid base64 {c}"))?;
        acc = acc << 6 | u32::try_from(v).map_err(|e| e.to_string())?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).map_err(|e| e.to_string())?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0, 255, 128, 7]] {
            assert_eq!(unb64(&b64(s)).unwrap(), s);
        }
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64(b"fo"), "Zm8=");
    }
}
