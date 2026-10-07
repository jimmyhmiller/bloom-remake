//! `blossom store`: inspect, verify, dump, back up and restore node stores.
//!
//! `blossom store db DIR [--verify]` describes the database of the stopped node whose store is `DIR`
//! (docs/design/DATABASE.md): its tables, the tick they cover, its history, its key format; `--verify` reads every
//! block of every table and checks them. It takes the store's lock: a running node's store is refused. The other forms
//! are WP M5.4's (DIST-021) and exit with code 7 (ARCHITECTURE §12.5), naming the feature and the WP.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use blossom_store::lsm::{Lsm, LsmOptions};
use blossom_store::{RealFs, StoreError, StoreLock, Vfs};

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom store`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// `db DIR [--verify]`; the other forms are not implemented yet.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<OsString>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    let mut it = args.args.iter();
    if it.next().is_none_or(|a| a != "db") {
        return crate::exit::not_implemented("DIST-021", "M5.4");
    }
    let mut dir: Option<PathBuf> = None;
    let mut verify = false;
    for a in it {
        if a == "--verify" {
            verify = true;
        } else if dir.is_none() {
            dir = Some(PathBuf::from(a));
        } else {
            eprintln!("blossom store db: one store directory, then `--verify` if wanted");
            return Exit::Usage.into();
        }
    }
    let Some(dir) = dir else {
        eprintln!("blossom store db: which store? (`blossom store db DIR [--verify]`)");
        return Exit::Usage.into();
    };
    match describe(&dir, verify) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, e)) => {
            eprintln!("blossom store db: {e}");
            code.into()
        }
    }
}

fn describe(dir: &std::path::Path, verify: bool) -> Result<(), (Exit, String)> {
    let fs: Arc<dyn Vfs> = Arc::new(RealFs);
    let _lock = match StoreLock::acquire(&*fs, dir) {
        Ok(l) => l,
        Err(StoreError::Locked { pid, .. }) => {
            return Err((Exit::Refused, format!("the node is running (process {})", pid.trim())));
        }
        Err(e) => return Err((Exit::Refused, e.to_string())),
    };
    let db = dir.join("db");
    let lsm = Lsm::open_read_only(
        fs,
        &db,
        LsmOptions {
            format: blossom_runtime::db::KEY_FORMAT,
            ..LsmOptions::default()
        },
    )
    .map_err(|e| (Exit::Refused, e.to_string()))?;
    let info = lsm.info().map_err(|e| (Exit::Internal, e.to_string()))?;
    let tick = |t: Option<u64>| t.map_or_else(|| "none".to_owned(), |t| t.to_string());
    println!("database {}", db.display());
    println!("  key format {}", info.key_format);
    println!("  tables cover ticks up to {}", tick(info.flushed));
    println!("  as-of reads from tick {}", info.floor);
    println!(
        "  {} table{}, {} bytes",
        info.tables.len(),
        if info.tables.len() == 1 { "" } else { "s" },
        info.tables.iter().map(|t| t.bytes).sum::<u64>()
    );
    for t in &info.tables {
        println!(
            "  table {}: {} entries in {} blocks, {} bytes, ticks {} to {}{}",
            t.id,
            t.entries,
            t.blocks,
            t.bytes,
            t.min_version,
            t.max_version,
            if t.filtered { "" } else { " (no filter: format 1)" }
        );
    }
    if verify {
        let n = lsm.verify().map_err(|e| (Exit::Refused, format!("damaged: {e}")))?;
        println!("  verified {n} entries");
    }
    Ok(())
}
