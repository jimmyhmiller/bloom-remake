//! The admin listener (`blossom run --admin ADDR`, docs/design/DATABASE.md §5): `POST /query` with an encoded
//! [`QueryRequest`] answers JSON (an [`Answer`]), or a 400 with the reason. One request per connection. The admin
//! plane has no authentication yet (DIST-066), so the server refuses it outside `insecure-dev`.

use std::io::{BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use blossom_artifact::bls::BlsArtifact;

use crate::RuntimeError;
use crate::db::Database;
use crate::query::{Answer, MAX_REQUEST, QueryRequest, answer};
use crate::web;

/// What the admin connections share.
#[derive(Clone)]
pub(crate) struct AdminCtx {
    pub db: Arc<Database>,
    pub artifact: Arc<BlsArtifact>,
    pub names: Arc<[Arc<str>]>,
    pub externs: Arc<blossom_value::ExternRegistry>,
}

/// Accepts admin connections until `stop`.
pub(crate) fn accept_loop(listener: TcpListener, ctx: AdminCtx, stop: Arc<AtomicBool>) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let configured = stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(Duration::from_secs(30))))
                    .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(30))));
                if configured.is_err() {
                    continue;
                }
                let ctx = ctx.clone();
                // A connection whose thread cannot start is dropped, which closes it; a failed one closes too.
                let _ = std::thread::Builder::new().name("admin".into()).spawn(move || {
                    let _ = serve(stream, &ctx);
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn serve(stream: TcpStream, ctx: &AdminCtx) -> Result<(), RuntimeError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(RuntimeError::Io)?);
    let req = web::read_request(&mut reader)?;
    let mut w = stream;
    if req.method != "POST" || req.path != "/query" {
        return web::respond(&mut w, 404, "Not Found", "text/plain", b"POST /query");
    }
    let len: usize = req
        .header("content-length")
        .and_then(|l| l.parse().ok())
        .ok_or_else(|| RuntimeError::Net("a query without its length".into()))?;
    if len > MAX_REQUEST {
        return web::respond(&mut w, 413, "Payload Too Large", "text/plain", b"a query over 16 MiB");
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(RuntimeError::Io)?;
    match run(&body, ctx) {
        Ok(a) => {
            let json =
                serde_json::to_vec(&a).map_err(|e| RuntimeError::Internal(blossom_base::internal_error!("{e}")))?;
            web::respond(&mut w, 200, "OK", "application/json", &json)
        }
        Err(e) => web::respond(&mut w, 400, "Bad Request", "text/plain", e.to_string().as_bytes()),
    }
}

fn run(body: &[u8], ctx: &AdminCtx) -> Result<Answer, RuntimeError> {
    let req = QueryRequest::decode(body)?;
    let now = crate::clock::wall_now().map_err(RuntimeError::Config)?;
    answer(
        req,
        &ctx.db,
        &ctx.artifact.program,
        &ctx.names,
        ctx.externs.clone(),
        now,
    )
}
