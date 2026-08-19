//! Explicit, minimal facade for the `h12tiny-*` component crates.
//!
//! Select the smallest component crate directly for the strictest dependency
//! graph, or use this package's forwarding features for a concise dependency.
//! The facade intentionally owns no endpoint implementation.

#[cfg(feature = "core")]
/// Runtime-neutral I/O adapters reexported from [`h12tiny_core`].
pub mod io {
    pub use h12tiny_core::io::*;
}

#[cfg(feature = "core")]
/// Runtime-neutral executor and timer adapters reexported from
/// [`h12tiny_core`].
pub mod runtime {
    pub use h12tiny_core::runtime::*;
}

#[cfg(feature = "client")]
/// Direct-origin client and per-origin pooling reexported from
/// [`h12tiny_client`].
pub mod client {
    pub use h12tiny_client::*;
}

#[cfg(feature = "server")]
/// Server connection and lifecycle primitives reexported from
/// [`h12tiny_server`].
pub mod server {
    pub use h12tiny_server::*;
}

#[cfg(feature = "util")]
/// Protocol-neutral body conveniences reexported from [`h12tiny_util`].
pub use h12tiny_util as util;

#[cfg(feature = "web")]
/// Optional application adapter reexported from [`h12tiny_web`].
pub use h12tiny_web as web;
