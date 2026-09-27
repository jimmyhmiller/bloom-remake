//! WAL and checkpoint byte recovery must either return a typed error or a valid whole-record prefix.
#![no_main]

use blossom_store::{OpenOpts, SimFs, Vfs, WalScan};
use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let fs = SimFs::default();
    let dir = Path::new("/wal");
    if fs.create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join("00000000000000000001.seg");
    if let Ok(mut file) = fs.open(
        &path,
        OpenOpts {
            create_new: true,
            ..OpenOpts::default()
        },
    ) {
        if file.append(data).is_ok() && file.sync_data().is_ok() && fs.sync_dir(dir).is_ok() {
            let _ = WalScan::scan(&fs, dir, [0; 16], true);
        }
    }
});
