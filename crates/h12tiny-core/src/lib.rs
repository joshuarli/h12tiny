//! Shared low-level facilities for the h12tiny component crates.
//!
//! This crate deliberately contains only the futures-I/O bridge and the
//! runtime-neutral executor/timer boundary. It has no client policy, server
//! accept loop, TLS, routing, JSON, or application abstractions.

/// Adapts futures-I/O streams to Hyper's runtime-neutral I/O traits.
pub mod io;

/// Runtime-neutral executor and timer integration.
pub mod runtime;
