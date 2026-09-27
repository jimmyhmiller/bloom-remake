//! Embeds `std/**/*.bls` (ARCHITECTURE §1.5; PLAN M1.1 §5): generates `STD_SOURCES` sorted by module path, and
//! `STD_REJECTED` for modules that cannot be embedded, each with its reason. Rejection is per module, so a broken
//! file never breaks the build or a program that does not import it.

// The cargo build-script protocol is printed on stdout.
#![allow(clippy::print_stdout)]

#[path = "src/scan.rs"]
mod scan;

use std::error::Error;
use std::fmt::Write as _;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let std_dir = manifest_dir.join("..").join("..").join("std");
    println!("cargo::rerun-if-changed={}", std_dir.display());
    let result = scan::scan(&std_dir)?;
    for path in &result.watched {
        println!("cargo::rerun-if-changed={}", path.display());
    }

    let mut code = String::new();
    code.push_str("/// Every embedded standard-library module as `(module path, source)`, sorted by module path.\n");
    code.push_str("pub static STD_SOURCES: &[(&str, &str)] = &[\n");
    for module in &result.modules {
        let file = module.file.to_str().ok_or("a scanned module path is not UTF-8")?;
        writeln!(code, "    ({:?}, include_str!({:?})),", module.path, file)?;
    }
    code.push_str("];\n\n");
    code.push_str("/// Modules found under `std/` that could not be embedded, as `(module path, reason)`, sorted.\n");
    code.push_str("pub static STD_REJECTED: &[(&str, &str)] = &[\n");
    for rejected in &result.rejected {
        writeln!(code, "    ({:?}, {:?}),", rejected.path, rejected.reason)?;
        println!(
            "cargo::warning=std module {} is not embedded: {}",
            rejected.path, rejected.reason
        );
    }
    code.push_str("];\n");

    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("std_sources.rs");
    std::fs::write(out, code)?;
    Ok(())
}
