//! Global options, start-up and shared helpers of the CLI. Owned by WP M6.3 and M12.4 (which add global flags,
//! logging set-up with `tracing-subscriber`, and diagnostic rendering here). `main.rs` relies on
//! [`GlobalArgs`], [`Context`], [`init`] and [`long_version`].

use std::process::ExitCode;

/// The process's allocator. The engine allocates and frees many small values per tick (rows, keys, frames); glibc's
/// allocator spent about 14% of a broker's CPU on them (S10 notes), mimalloc much less.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// The standard library's host functions (FOREIGN-PROTOCOLS §4), bound by every command that runs a program.
/// Building the registry fails only if two areas register one path, a bug the host library's tests rule out.
pub fn std_externs() -> Result<std::sync::Arc<blossom_value::ExternRegistry>, String> {
    blossom_std_host::registry()
        .map(std::sync::Arc::new)
        .map_err(|e| format!("the standard host functions: {e}"))
}

/// Compiling `.ded` programs for the commands that run them (`sim`, `ldfi`).
pub mod stopwatch;

pub mod bls {
    use std::process::ExitCode;

    use blossom_driver::{bls::compile_spec_file, render::render};
    use blossom_front::api::BlsError;
    use blossom_front::spec::CompiledSpec;

    use crate::exit::Exit;

    /// Compiles the program rooted at `file` for `nodes`, printing diagnostics; on failure, the exit code to return.
    pub fn compile(
        file: &str,
        nodes: &[blossom_front::api::NodeSpec],
    ) -> Result<blossom_artifact::bls::BlsArtifact, ExitCode> {
        compile_with(file, nodes, &std::collections::BTreeMap::new())
    }

    /// [`compile`] with the deployment's values of deploy-time parameters.
    pub fn compile_with(
        file: &str,
        nodes: &[blossom_front::api::NodeSpec],
        params: &std::collections::BTreeMap<String, blossom_front::api::ParamBinding>,
    ) -> Result<blossom_artifact::bls::BlsArtifact, ExitCode> {
        let (result, sources) = blossom_driver::bls::compile_file_with(file, nodes, params);
        match result {
            Ok((a, warnings)) => {
                for d in warnings.iter() {
                    eprint!("{}", render(d, &sources));
                }
                Ok(a)
            }
            Err(BlsError::Rejected(diags)) => {
                for d in diags.iter() {
                    eprint!("{}", render(d, &sources));
                }
                let unimplemented = diags.iter().any(blossom_driver::render::is_not_implemented);
                Err(if unimplemented {
                    Exit::Unimplemented
                } else {
                    Exit::UserError
                }
                .into())
            }
            Err(BlsError::Internal(e)) => {
                eprintln!("{e}");
                Err(Exit::Internal.into())
            }
        }
    }

    /// A duration written like a Blossom literal: `500ms`, `1s`, `2m`.
    pub fn parse_duration(text: &str) -> Option<blossom_value::time::Duration> {
        let split = text.find(|c: char| c.is_ascii_alphabetic())?;
        let (num, unit) = text.split_at(split);
        let n: i64 = num.parse().ok()?;
        let scale: i64 = match unit {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return None,
        };
        n.checked_mul(scale)
            .filter(|n| *n > 0)
            .map(blossom_value::time::Duration::from_nanos)
    }

    /// Compiles the spec `name` of `file`, printing diagnostics; on failure, the exit code to return.
    pub fn compile_spec(file: &str, name: &str) -> Result<CompiledSpec, ExitCode> {
        let (result, sources) = compile_spec_file(file, name);
        match result {
            Ok((spec, warnings)) => {
                for d in warnings.iter() {
                    eprint!("{}", render(d, &sources));
                }
                Ok(spec)
            }
            Err(BlsError::Rejected(diags)) => {
                for d in diags.iter() {
                    eprint!("{}", render(d, &sources));
                }
                let unimplemented = diags.iter().any(blossom_driver::render::is_not_implemented);
                Err(if unimplemented {
                    Exit::Unimplemented
                } else {
                    Exit::UserError
                }
                .into())
            }
            Err(BlsError::Internal(e)) => {
                eprintln!("{e}");
                Err(Exit::Internal.into())
            }
        }
    }
}

pub mod ded {
    use std::process::ExitCode;

    use blossom_artifact::sim::SimArtifact;
    use blossom_driver::{ded::compile_files, render::render};
    use blossom_front::ded::DedError;

    use crate::exit::Exit;

    /// Compiles `files` for `nodes`, printing diagnostics; on failure, the exit code to return.
    pub fn compile(files: &[String], nodes: &[String]) -> Result<SimArtifact, ExitCode> {
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let nodes: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let (result, sources) = compile_files(&files, &nodes);
        match result {
            Ok(a) => Ok(a),
            Err(DedError::Rejected(diags)) => {
                for d in diags.iter() {
                    eprint!("{}", render(d, &sources));
                }
                let unimplemented = diags.iter().any(blossom_driver::render::is_not_implemented);
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
