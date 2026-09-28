//! Global options, start-up and shared helpers of the CLI. Owned by WP M6.3 and M12.4 (which add global flags,
//! logging set-up with `tracing-subscriber`, and diagnostic rendering here). `main.rs` relies on
//! [`GlobalArgs`], [`Context`], [`init`] and [`long_version`].

use std::process::ExitCode;

/// Options accepted before the subcommand. None yet.
#[derive(Debug, Default, clap::Args)]
pub struct GlobalArgs {}

/// What every command receives from start-up.
#[derive(Debug)]
pub struct Context {
    _private: (),
}

/// Start-up: applies the global options. An error has been reported already; its exit code is returned.
pub fn init(global: &GlobalArgs) -> Result<Context, ExitCode> {
    let GlobalArgs {} = global;
    Ok(Context { _private: () })
}

/// The `--version` text (ARCHITECTURE §12.5): the compiler version and the version of every format and ABI that
/// exists in this build. The engine ABI (M4.8), wire ABI (M4.4), storage format (M2.6/M5.4) and trace format
/// (M4.7) are added here by WP M6.3 once they exist.
pub fn long_version() -> &'static str {
    use std::sync::OnceLock;
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        format!(
            "{}\nvalue encoding version: {}\nPRF version: {}",
            env!("CARGO_PKG_VERSION"),
            blossom_value::ENCODING_VERSION,
            blossom_value::PRF_VERSION,
        )
    })
}

/// Compiling `.ded` programs for the commands that run them (`sim`, `ldfi`).
pub mod ded {
    use std::process::ExitCode;

    use blossom_artifact::ded::DedArtifact;
    use blossom_driver::{ded::compile_files, render::render};
    use blossom_front::ded::DedError;

    use crate::exit::Exit;

    /// Compiles `files` for `nodes`, printing diagnostics; on failure, the exit code to return.
    pub fn compile(files: &[String], nodes: &[String]) -> Result<DedArtifact, ExitCode> {
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let nodes: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let (result, sources) = compile_files(&files, &nodes);
        match result {
            Ok(a) => Ok(a),
            Err(DedError::Rejected(diags)) => {
                for d in diags.iter() {
                    eprint!("{}", render(d, &sources));
                }
                let unimplemented = diags.iter().any(|d| d.code.as_str() == "BLS0908");
                Err(if unimplemented {
                    Exit::Unimplemented
                } else {
                    Exit::UserError
                }
                .into())
            }
            Err(DedError::Internal(e)) => {
                eprintln!("{e}");
                Err(Exit::Internal.into())
            }
        }
    }

    /// Whether every root is a `.ded` file.
    pub fn all_ded(files: &[String]) -> bool {
        !files.is_empty() && files.iter().all(|f| f.ends_with(".ded"))
    }

    /// Parses `a:b:1` (an omission) or `a:2` (a crash) into node names and a tick.
    pub fn parse_fault(text: &str, parts: usize) -> Result<(Vec<String>, u64), String> {
        let fields: Vec<&str> = text.split(':').collect();
        if fields.len() != parts {
            return Err(format!("`{text}`: expected {parts} fields separated by `:`"));
        }
        let (names, tick) = fields.split_at(parts - 1);
        let tick = tick
            .first()
            .and_then(|t| t.parse::<u64>().ok())
            .ok_or_else(|| format!("`{text}`: the last field is a tick"))?;
        Ok((names.iter().map(|s| (*s).to_owned()).collect(), tick))
    }
}
