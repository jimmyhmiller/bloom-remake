//! `blossom node`: create node identities and report node status (DIST-040).
//!
//! `blossom node init --deploy deploy.toml --node s1` creates the node's store: its directory and a `META` holding
//! the identity the deployment expects, with a fresh store id. `blossom run` then opens it (without `--init-fresh`).
//! `blossom node status` needs the ops listener (DIST-044), a later slice.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use blossom_node::env::Entropy;
use blossom_node::recovery;
use blossom_runtime::clock::{OsEntropy, wall_now};
use blossom_runtime::server::store_identity;
use blossom_store::RealFs;

use crate::cmd::run::load;
use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom node`.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: NodeCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum NodeCommand {
    /// Create a node's store for a deployment.
    Init {
        /// The deployment spec (`deploy.toml`).
        #[arg(long = "deploy", value_name = "FILE")]
        deploy: PathBuf,
        /// The node.
        #[arg(long)]
        node: String,
        /// The store directory (default: `<storage.data_dir>/<node>`).
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Report a running node's status.
    Status {
        /// The command's arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<std::ffi::OsString>,
    },
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    match args.command {
        NodeCommand::Init { deploy, node, store } => init(&deploy, &node, store),
        NodeCommand::Status { .. } => crate::exit::not_implemented("DIST-044", "M7.4"),
    }
}

fn init(deploy: &std::path::Path, node: &str, store: Option<PathBuf>) -> ExitCode {
    let (spec, artifact) = match load(deploy) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let result = (|| -> Result<PathBuf, String> {
        let mut identity = store_identity(&spec, &artifact, node).map_err(|e| e.to_string())?;
        let (_, entry) = spec.node(node).map_err(|e| e.to_string())?;
        let dir = store.unwrap_or_else(|| spec.data_dir.join(&entry.name));
        identity.store_uuid = recovery::fresh_uuid(&identity, OsEntropy.boot_nonce()?, wall_now()?);
        recovery::init(Arc::new(RealFs), &dir, &identity).map_err(|e| e.to_string())?;
        Ok(dir)
    })();
    match result {
        Ok(dir) => {
            println!("blossom: created the store of node {node} at {}", dir.display());
            Exit::Ok.into()
        }
        Err(e) => {
            eprintln!("blossom node init: {e}");
            Exit::Refused.into()
        }
    }
}
