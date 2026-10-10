//! Stateless hosting (docs/design/STATELESS.md): a deployment's nodes and keyed members as objects on an external
//! state store, served by hosts that keep nothing between requests.

pub mod deployment;
pub mod objects;
pub mod serve;
pub mod sqltree;

pub use deployment::{Deployment, REGISTRY, Runs};
pub use objects::{Ctx, Done, LEASE, Objects, Opened, POLL_WAIT, ServeError, parse_session, presence_key, session_id};
pub use serve::{Report, ServeConfig, ServeStats, Serving, serve};
