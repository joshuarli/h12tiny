//! Server endpoint surface.
//!
//! The connection policy selects HTTP/1.1 or HTTP/2 from plaintext preface
//! detection or TLS ALPN; HTTP/1.1 Upgrade-to-h2c is intentionally out of scope.

pub mod conn;
