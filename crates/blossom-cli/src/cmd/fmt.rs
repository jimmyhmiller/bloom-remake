//! `blossom fmt`: format Blossom sources in the one canonical format (LANGUAGE §3.5, `blossom_syntax::fmt`).
//!
//! `blossom fmt FILE|DIR…` rewrites each `.bls` file (a directory: every `.bls` file below it) whose format differs;
//! `--check` rewrites nothing, lists the files that would change, and exits 1 if there are any. With no paths it
//! formats standard input to standard output. A file that does not parse is left as it is, its errors reported, and
//! the exit code is 1.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use blossom_base::SourceDb;
use blossom_syntax::fmt::FormatError;

use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom fmt`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Rewrite nothing: list the files whose format differs, and exit 1 if there are any.
    #[arg(long)]
    pub check: bool,
    /// The files to format, or directories to format every `.bls` file below (none: standard input to standard
    /// output).
    pub paths: Vec<PathBuf>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    if args.paths.is_empty() {
        return stdin(args.check);
    }
    let mut files = Vec::new();
    for p in &args.paths {
        if let Err(e) = collect(p, &mut files) {
            eprintln!("{}: {e}", p.display());
            return Exit::UserError.into();
        }
    }
    let mut failed = false;
    let mut differ = Vec::new();
    for f in &files {
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                failed = true;
                continue;
            }
        };
        match blossom_syntax::fmt::format(&text) {
            Ok(out) if out == text => {}
            Ok(out) => {
                if args.check {
                    differ.push(f.clone());
                } else if let Err(e) = std::fs::write(f, out) {
                    eprintln!("{}: {e}", f.display());
                    failed = true;
                }
            }
            Err(e) => {
                report(&f.display().to_string(), &text, &e);
                failed = true;
            }
        }
    }
    for f in &differ {
        println!("{}", f.display());
    }
    if failed || !differ.is_empty() {
        Exit::UserError.into()
    } else {
        Exit::Ok.into()
    }
}

/// Standard input to standard output (with `--check`, only the verdict).
fn stdin(check: bool) -> ExitCode {
    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        eprintln!("standard input: {e}");
        return Exit::UserError.into();
    }
    match blossom_syntax::fmt::format(&text) {
        Ok(out) if check => {
            if out == text {
                Exit::Ok.into()
            } else {
                println!("<stdin>");
                Exit::UserError.into()
            }
        }
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            match stdout.write_all(out.as_bytes()).and_then(|()| stdout.flush()) {
                Ok(()) => Exit::Ok.into(),
                Err(e) => {
                    eprintln!("standard output: {e}");
                    Exit::UserError.into()
                }
            }
        }
        Err(e) => {
            report("<stdin>", &text, &e);
            Exit::UserError.into()
        }
    }
}

/// The `.bls` files of a path: the file itself, or every one below a directory, in order.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !path.is_dir() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    for e in entries {
        let hidden = e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if e.is_dir() {
            if !hidden && e.file_name().is_none_or(|n| n != "target" && n != "node_modules") {
                collect(&e, out)?;
            }
        } else if e.extension().is_some_and(|x| x == "bls") {
            out.push(e);
        }
    }
    Ok(())
}

/// A file's syntax errors, rendered against its text.
fn report(name: &str, text: &str, e: &FormatError) {
    let FormatError::Syntax(errors) = e;
    let mut sources = SourceDb::new();
    if sources.add_text(name, text).is_err() {
        eprintln!("{name}: {e}");
        return;
    }
    eprintln!("{name}: not formatted: {e}");
    for err in errors {
        eprintln!("{}", blossom_driver::render::render(&err.diagnostic, &sources));
    }
}
