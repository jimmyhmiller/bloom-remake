#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-wire`: the frame format, the field-numbered tuple codec, `WireLimits`, hello and schema identities.
//!
//! See ARCHITECTURE §5.4. Slice 3 (docs/design/SLICES.md) implements the codec for every type the Blossom subset
//! has, plain batches and the handshake; delta and group frame kinds, cross-version translation and the codec ABI
//! belong to later slices and are rejected loudly.

pub mod catalog;
pub mod codec;
pub mod frame;

#[cfg(test)]
mod tests;

pub use codec::{Codec, NodeEncoding, WireError, WireLimits};
pub use frame::{Batch, ChannelSchema, Frame, FrameIoError, Hello, Peer, RejectReason};
