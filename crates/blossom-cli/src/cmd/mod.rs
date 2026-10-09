//! One module per subcommand (ARCHITECTURE §12.5, PLAN §4 D7/D8). Each file is owned by the WP that implements
//! the command; this list is a dispatch file, frozen after M1.

pub mod admin;
pub mod build;
pub mod check;
pub mod compat;
pub mod completions;
pub mod config;
pub mod deploy;
pub mod explain;
pub mod fmt;
pub mod ldfi;
pub mod lsp;
pub mod node;
pub mod plan;
pub mod query;
pub mod release;
pub mod repl;
pub mod run;
pub mod self_check;
pub mod serve;
pub mod sim;
pub mod store;
pub mod trace;
pub mod upgrade;
pub mod verify;
pub mod why;
pub mod whynot;
