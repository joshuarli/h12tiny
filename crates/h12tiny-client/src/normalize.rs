//! Shared direct-origin normalization used before Hyper's HTTP/1 handoff.
//!
//! The transport-neutral rules live in `h12tiny-client-normalize` so the
//! blocking HTTP/1 client cannot diverge from the futures-I/O client.

pub(crate) use h12tiny_client_normalize::{
    extract_origin as extract_pool_key, normalize_http1_request as normalize_h1_request,
    Error, Origin as PoolKey,
};

pub(crate) fn pool_key_origin(key: &PoolKey) -> String {
    key.display()
}

pub(crate) fn pool_key_uri(key: PoolKey) -> http::Uri {
    key.uri()
}
