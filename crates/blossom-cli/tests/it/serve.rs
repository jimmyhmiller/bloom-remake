//! `blossom serve` as a platform starts and stops it (docs/design/DEPLOY.md §2): the store from `BLOSSOM_STORE`,
//! the address from `PORT`, a wake secret it insists on when only a scheduler sweeps, and a drain on `SIGTERM`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SEED: &str = "00112233445566778899aabbccddeeff";

#[cfg(test)]
fn deploy() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/keyed_chat.deploy.toml")
}

/// A page root with an index (all `serve` asks of it).
#[cfg(test)]
fn web_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-serve-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
    dir
}

/// `blossom serve` with `args`, its environment cleared of what the tests set.
#[cfg(test)]
fn serve(args: &[&str], root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blossom"));
    c.arg("serve")
        .arg("--deploy")
        .arg(deploy())
        .arg("--web-root")
        .arg(root)
        .args(args)
        .env("BLOSSOM_SEED", SEED)
        .env_remove("BLOSSOM_WAKE_SECRET")
        .env_remove("BLOSSOM_STORE")
        .env_remove("PORT");
    c
}

#[test]
fn a_host_only_a_scheduler_sweeps_needs_a_wake_secret() {
    let root = web_root("secret");
    let out = serve(&["--store", "memory:", "--web", "127.0.0.1:0", "--sweep", "off"], &root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stderr).contains("set BLOSSOM_WAKE_SECRET"));

    let out = serve(
        &["--store", "memory:", "--web", "127.0.0.1:0", "--sweep", "off", "--wake-secret-env", "CRON_SECRET"],
        &root,
    )
    .env("CRON_SECRET", "short")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stderr).contains("CRON_SECRET is shorter than 16"));
}

#[test]
fn a_host_needs_an_address() {
    let root = web_root("addr");
    let out = serve(&["--store", "memory:"], &root).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pass --web ADDR, or set PORT"));
}

/// One request on a connection of its own: the status and the body.
#[cfg(test)]
fn get(port: u16, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").unwrap();
    let mut text = String::new();
    s.read_to_string(&mut text).unwrap();
    let status = text.split(' ').nth(1).unwrap().parse().unwrap();
    let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
    (status, body)
}

#[test]
fn a_host_takes_its_port_and_store_from_the_environment_and_drains_on_sigterm() {
    let root = web_root("env");
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = serve(&["--sweep", "off", "--wake-secret-env", "CRON_SECRET"], &root)
        .env("PORT", port.to_string())
        .env("BLOSSOM_STORE", "memory:")
        .env("CRON_SECRET", "a-cron-secret-long-enough")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains(&format!("serving the page on http://0.0.0.0:{port}/")), "{line}");
    assert!(line.contains("store memory:"), "{line}");
    assert_eq!(get(port, "/blossom/health"), (200, "ok".into()));

    // An idle connection does not hold the drain up.
    let mut idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let started = Instant::now();
    let pid = child.id().to_string();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "still running after SIGTERM");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(err.contains("draining"), "{err}");
    assert!(!err.contains("still open"), "{err}");
    let mut rest = Vec::new();
    assert_eq!(idle.read_to_end(&mut rest).unwrap(), 0);
}
