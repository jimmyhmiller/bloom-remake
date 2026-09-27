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
