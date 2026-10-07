//! A WebSocket client for the tests of client members' links (docs/design/CLIENTS.md §3): the page's side of the
//! transport, masked frames out and the server's unmasked frames in, one link frame per binary message.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use blossom_wire::frame::Frame;

/// A connection to a node's `/blossom/link`.
pub struct Ws {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

fn io(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

impl Ws {
    /// Opens the WebSocket at `127.0.0.1:port`, checking the server's accept key (RFC 6455 §4.2.2).
    pub fn connect(port: u16) -> std::io::Result<Ws> {
        let s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut w = s.try_clone()?;
        write!(
            w,
            "GET /blossom/link HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut r = BufReader::new(s);
        let mut status = String::new();
        r.read_line(&mut status)?;
        if !status.starts_with("HTTP/1.1 101") {
            return Err(io(format!("the server answered `{}`", status.trim())));
        }
        let mut accept = None;
        loop {
            let mut line = String::new();
            r.read_line(&mut line)?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Sec-WebSocket-Accept: ") {
                accept = Some(v.trim().to_owned());
            }
        }
        if accept.as_deref() != Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") {
            return Err(io(format!("a wrong accept key {accept:?}")));
        }
        Ok(Ws { r, w })
    }

    /// Sends the bytes of one link frame as a masked binary message.
    pub fn send_bytes(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mask = [7u8, 1, 9, 3];
        let mut out = vec![0x82];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= usize::from(u16::MAX) => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        self.w.write_all(&out)
    }

    pub fn send(&mut self, f: &Frame) -> std::io::Result<()> {
        self.send_bytes(&f.encode())
    }

    /// The next binary message's bytes (one link frame).
    pub fn recv_bytes(&mut self) -> std::io::Result<Vec<u8>> {
        let mut first = [0u8; 1];
        self.r.read_exact(&mut first)?;
        self.rest_of_message(first[0])
    }

    /// A message after its first byte.
    fn rest_of_message(&mut self, first: u8) -> std::io::Result<Vec<u8>> {
        if first != 0x82 {
            return Err(io(format!("a frame {first:#x}, not a final binary one")));
        }
        let mut len = [0u8; 1];
        self.r.read_exact(&mut len)?;
        let len = match len[0] {
            126 => {
                let mut b = [0u8; 2];
                self.r.read_exact(&mut b)?;
                u64::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                self.r.read_exact(&mut b)?;
                u64::from_be_bytes(b)
            }
            n => u64::from(n),
        };
        let mut payload = vec![0u8; usize::try_from(len).map_err(|_| io("a message too large"))?];
        self.r.read_exact(&mut payload)?;
        Ok(payload)
    }

    /// The next link frame.
    pub fn recv(&mut self) -> std::io::Result<Frame> {
        let payload = self.recv_bytes()?;
        match Frame::parse(&payload, &Default::default()) {
            Ok(Some((f, used))) if used == payload.len() => Ok(f),
            other => Err(io(format!("a message that is not one frame: {other:?}"))),
        }
    }

    /// Like [`Ws::recv_bytes`], giving up after `timeout` when no message has begun (`None` then); a message that
    /// began is read whole.
    pub fn recv_bytes_within(&mut self, timeout: Duration) -> std::io::Result<Option<Vec<u8>>> {
        self.r.get_ref().set_read_timeout(Some(timeout))?;
        let mut first = [0u8; 1];
        let began = match self.r.read(&mut first) {
            Ok(0) => return Err(io("the connection closed")),
            Ok(_) => true,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => false,
            Err(e) => return Err(e),
        };
        self.r.get_ref().set_read_timeout(Some(Duration::from_secs(10)))?;
        if began {
            self.rest_of_message(first[0]).map(Some)
        } else {
            Ok(None)
        }
    }
}
