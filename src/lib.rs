//! A small, runtime-neutral HTTP endpoint layer built on Hyper and h2.
//!
//! The crate's public modules follow the ownership boundary in `plan.md`:
//! protocol machinery remains in Hyper/h2, while this crate owns endpoint
//! policy, transport, pooling, and futures-I/O adaptation.

/// Adapts futures I/O streams to Hyper's runtime-neutral I/O traits.
pub mod io;

/// Runtime-neutral executor and timer integration.
pub mod runtime;

#[cfg(feature = "client")]
/// Client endpoint and per-origin pooling.
pub mod client;

#[cfg(feature = "server")]
/// Server endpoint protocol selection.
pub mod server;
