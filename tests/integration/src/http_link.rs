//! A client of a member's link over plain HTTP requests (docs/design/CLIENTS.md §3a), for the tests: the page's side,
//! one request per connection. `recv` long-polls when it has no frame left of the last answer.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use blossom_wire::frame::Frame;

fn io(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// An answer: its status, headers (names lowercased) and body.
pub struct Answer {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// One request on a connection of its own, its answer read whole.
pub fn request(port: u16, method: &str, path: &str, body: &[u8]) -> std::io::Result<Answer> {
    let s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(40)))?;
    let mut w = s.try_clone()?;
    write!(
        w,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    w.write_all(body)?;
    read_answer(&mut BufReader::new(s))
}

/// An answer on `r`: the status line, the headers, and `Content-Length` bytes of body.
pub fn read_answer(r: &mut impl BufRead) -> std::io::Result<Answer> {
    let mut status = String::new();
    r.read_line(&mut status)?;
    let code = status
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| io(format!("a status line `{}`", status.trim())))?;
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len: usize = headers
        .get("content-length")
        .map(|l| l.parse().map_err(|_| io("a bad Content-Length")))
        .transpose()?
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Answer {
        status: code,
        headers,
        body,
    })
}

/// Frames as a body: each a 4-byte big-endian length and the frame.
pub fn encode(frames: &[&Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        let b = f.encode();
        out.extend_from_slice(&(b.len() as u32).to_be_bytes());
        out.extend_from_slice(&b);
    }
    out
}

/// A body's frames.
pub fn decode(mut body: &[u8]) -> std::io::Result<Vec<Frame>> {
    let mut out = Vec::new();
    while !body.is_empty() {
        let (len, rest) = body.split_first_chunk::<4>().ok_or_else(|| io("a truncated length"))?;
        let (bytes, rest) = rest
            .split_at_checked(u32::from_be_bytes(*len) as usize)
            .ok_or_else(|| io("a truncated frame"))?;
        match Frame::parse(bytes, &Default::default()) {
            Ok(Some((f, used))) if used == bytes.len() => out.push(f),
            other => return Err(io(format!("a part that is not one frame: {other:?}"))),
        }
        body = rest;
    }
    Ok(out)
}

/// A member's link over requests.
pub struct HttpLink {
    port: u16,
    /// The session (`None`: the open was refused).
    pub session: Option<String>,
    frames: VecDeque<Frame>,
}

impl HttpLink {
    /// Opens a session with `hello`; the open's answer frames are the first `recv`s.
    pub fn open(port: u16, hello: &Frame) -> std::io::Result<HttpLink> {
        let a = request(port, "POST", "/blossom/http/open", &encode(&[hello]))?;
        if a.status != 200 {
            return Err(io(format!("open answered {}", a.status)));
        }
        Ok(HttpLink {
            port,
            session: a.headers.get("blossom-session").cloned(),
            frames: decode(&a.body)?.into(),
        })
    }

    fn path(&self, verb: &str) -> std::io::Result<String> {
        let s = self.session.as_ref().ok_or_else(|| io("no session"))?;
        Ok(format!("/blossom/http/{s}/{verb}"))
    }

    /// Sends one frame; the answer's status.
    pub fn send_status(&mut self, f: &Frame) -> std::io::Result<u16> {
        Ok(request(self.port, "POST", &self.path("send")?, &encode(&[f]))?.status)
    }

    pub fn send(&mut self, f: &Frame) -> std::io::Result<()> {
        match self.send_status(f)? {
            204 => Ok(()),
            s => Err(io(format!("send answered {s}"))),
        }
    }

    /// The next frame: left of the last answer, else from receives (long-polled) until one brings some.
    pub fn recv(&mut self) -> std::io::Result<Frame> {
        loop {
            if let Some(f) = self.frames.pop_front() {
                return Ok(f);
            }
            let a = request(self.port, "GET", &self.path("recv")?, b"")?;
            if a.status != 200 {
                return Err(io(format!("recv answered {}", a.status)));
            }
            self.frames.extend(decode(&a.body)?);
        }
    }

    /// Closes the session (as the page's beacon does).
    pub fn close(&mut self) -> std::io::Result<u16> {
        Ok(request(self.port, "POST", &self.path("close")?, b"")?.status)
    }
}
