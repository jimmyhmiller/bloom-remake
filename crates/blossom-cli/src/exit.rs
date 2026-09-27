//! Exit codes (ARCHITECTURE §12.5). One table for every subcommand; `blossom --help` prints it.

use std::process::ExitCode;

/// The process exit codes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// Success.
    Ok = 0,
    /// A program or user error (diagnostics).
    UserError = 1,
    /// A usage error (clap).
    Usage = 2,
    /// Verification failed: a check with `expect holds` failed, or `expect fails` held.
    VerifyFailed = 3,
    /// An internal error (a bug).
    Internal = 4,
    /// Refused to start: storage identity, corruption or configuration. Supervisors should not restart.
    Refused = 5,
    /// A runtime fault or halted node. Supervisors should restart.
    Fault = 6,
    /// An unimplemented feature.
    Unimplemented = 7,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> ExitCode {
        ExitCode::from(exit as u8)
    }
}

/// The exit-code table, printed by `blossom --help`.
pub const HELP: &str = "\
Exit codes:
  0  ok
  1  program or user error (diagnostics)
  2  usage error
  3  verification failed (a check with `expect holds` failed, or `expect fails` held)
  4  internal error (a bug)
  5  refused to start: storage identity, corruption, configuration (supervisors should not restart)
  6  runtime fault or halted node (supervisors should restart)
  7  unimplemented feature";

/// Reports a command or feature that is not implemented yet and returns exit code 7. `what` is the FEATURES id
/// (or, where FEATURES has none, a description); `wp` is the work package that implements it.
pub fn not_implemented(what: &str, wp: &str) -> ExitCode {
    eprintln!("not implemented yet: {what} (WP {wp})");
    Exit::Unimplemented.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_table() {
        let codes = [
            (Exit::Ok, 0),
            (Exit::UserError, 1),
            (Exit::Usage, 2),
            (Exit::VerifyFailed, 3),
            (Exit::Internal, 4),
            (Exit::Refused, 5),
            (Exit::Fault, 6),
            (Exit::Unimplemented, 7),
        ];
        for (exit, code) in codes {
            assert_eq!(exit as u8, code);
            assert!(
                HELP.contains(&format!("  {code}  ")),
                "{code} missing from the help table"
            );
        }
    }
}
