//! Direct-origin request normalization.
//!
//! Hyper owns HTTP framing. This module owns only the request representation
//! immediately before it crosses the direct-origin connection boundary. The
//! rules deliberately track `hyper-util` 0.1.20's legacy client so pool
//! identity, `Host`, and HTTP/1 request-target semantics remain comparable.

use std::fmt;

use http::header::{HeaderValue, HOST};
use http::uri::{Authority, Scheme};
use http::{Method, Request, Uri};

/// The POC deliberately pools only by origin. It does not coalesce H2
/// connections across authorities, even where the RFC would permit that.
pub(crate) type PoolKey = (Scheme, Authority);

/// Formats the stable, direct-origin identity used by the pool. Keeping this
/// beside [`PoolKey`] means diagnostics cannot accidentally report a request
/// target after HTTP/1 normalization has removed its scheme and authority.
pub(crate) fn pool_key_origin((scheme, authority): &PoolKey) -> String {
    format!("{scheme}://{authority}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    /// A direct-origin request needs an absolute URI to identify its peer.
    AbsoluteUriRequired,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("client requests require an absolute URI")
    }
}

impl std::error::Error for Error {}

/// Determines the connection-pool key before a request target is converted to
/// the HTTP/1 origin form. A CONNECT authority target is accepted and given a
/// conventional scheme solely to choose the transport/default port.
pub(crate) fn extract_pool_key(uri: &mut Uri, is_connect: bool) -> Result<PoolKey, Error> {
    let original = uri.clone();
    match (original.scheme(), original.authority()) {
        (Some(scheme), Some(authority)) => Ok((scheme.clone(), authority.clone())),
        (None, Some(authority)) if is_connect => {
            let scheme = match authority.port_u16() {
                Some(443) => Scheme::HTTPS,
                _ => Scheme::HTTP,
            };
            set_connect_scheme(uri, scheme.clone());
            Ok((scheme, authority.clone()))
        }
        _ => Err(Error::AbsoluteUriRequired),
    }
}

/// Inserts `Host` only when callers did not explicitly supply one. Default
/// ports are intentionally omitted, matching normal HTTP origin authority.
pub(crate) fn synthesize_host<B>(request: &mut Request<B>) {
    if request.headers().contains_key(HOST) {
        return;
    }

    let uri = request.uri();
    let host = uri
        .host()
        .expect("pool-key validation guarantees an authority host");
    let value = match non_default_port(uri) {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let value = HeaderValue::from_str(&value).expect("a URI host is a valid header value");
    request.headers_mut().insert(HOST, value);
}

/// Applies direct-origin HTTP/1 target rules after the origin was extracted.
pub(crate) fn normalize_h1_request<B>(request: &mut Request<B>, set_host: bool) {
    if set_host {
        synthesize_host(request);
    }
    if request.method() == Method::CONNECT {
        authority_form(request.uri_mut());
    } else {
        origin_form(request.uri_mut());
    }
}

/// Converts an absolute URI to origin form, preserving an empty/default path
/// as `/` and preserving a query without copying it.
pub(crate) fn origin_form(uri: &mut Uri) {
    let normalized = match uri.path_and_query() {
        Some(path) if path.as_str() != "/" => {
            let mut parts = http::uri::Parts::default();
            parts.path_and_query = Some(path.clone());
            Uri::from_parts(parts).expect("a URI path and query remain valid")
        }
        _ => Uri::default(),
    };
    *uri = normalized;
}

/// Converts a CONNECT URI to the HTTP/1 authority form. Any path is not part
/// of an HTTP CONNECT request target and is therefore deliberately discarded.
pub(crate) fn authority_form(uri: &mut Uri) {
    let authority = uri
        .authority()
        .cloned()
        .expect("CONNECT normalization follows pool-key validation");
    let mut parts = http::uri::Parts::default();
    parts.authority = Some(authority);
    *uri = Uri::from_parts(parts).expect("an existing URI authority is valid");
}

pub(crate) fn pool_key_uri((scheme, authority): PoolKey) -> Uri {
    http::uri::Builder::new()
        .scheme(scheme)
        .authority(authority)
        .path_and_query("/")
        .build()
        .expect("a pool key is a valid origin URI")
}

fn set_connect_scheme(uri: &mut Uri, scheme: Scheme) {
    debug_assert!(uri.scheme().is_none());
    let previous = std::mem::take(uri);
    let mut parts: http::uri::Parts = previous.into();
    parts.scheme = Some(scheme);
    parts.path_and_query = Some("/".parse().expect("a slash is a valid path"));
    *uri = Uri::from_parts(parts).expect("adding a scheme to CONNECT authority is valid");
}

fn non_default_port(uri: &Uri) -> Option<http::uri::Port<&str>> {
    match (uri.port().map(|port| port.as_u16()), is_secure_scheme(uri)) {
        (Some(443), true) | (Some(80), false) => None,
        _ => uri.port(),
    }
}

fn is_secure_scheme(uri: &Uri) -> bool {
    matches!(uri.scheme_str(), Some("https") | Some("wss"))
}

#[cfg(test)]
mod tests {
    use http::header::HOST;
    use http::{Method, Request, Uri};

    use super::{extract_pool_key, normalize_h1_request, Error};

    fn normalized(uri: &str) -> Request<()> {
        let mut request = Request::builder().uri(uri).body(()).unwrap();
        extract_pool_key(request.uri_mut(), false).unwrap();
        normalize_h1_request(&mut request, true);
        request
    }

    #[test]
    fn root_uses_origin_form_and_synthesizes_host() {
        let request = normalized("http://example.test");
        assert_eq!(request.uri(), &Uri::from_static("/"));
        assert_eq!(request.headers()[HOST], "example.test");
    }

    #[test]
    fn paths_and_queries_stay_in_origin_form() {
        let request = normalized("https://example.test/foo?x=1");
        assert_eq!(request.uri(), &Uri::from_static("/foo?x=1"));
        assert_eq!(request.headers()[HOST], "example.test");
    }

    #[test]
    fn explicit_host_is_preserved_and_default_ports_are_hidden() {
        let mut request = Request::builder()
            .uri("https://example.test:443/a")
            .header(HOST, "chosen.test")
            .body(())
            .unwrap();
        extract_pool_key(request.uri_mut(), false).unwrap();
        normalize_h1_request(&mut request, true);
        assert_eq!(request.headers()[HOST], "chosen.test");
        assert_eq!(request.uri(), &Uri::from_static("/a"));

        let request = normalized("http://example.test:8080/a");
        assert_eq!(request.headers()[HOST], "example.test:8080");
    }

    #[test]
    fn ipv4_and_ipv6_hosts_are_accepted() {
        let ipv4 = normalized("http://127.0.0.1:8080/a");
        assert_eq!(ipv4.headers()[HOST], "127.0.0.1:8080");

        let ipv6 = normalized("http://[::1]:8080/a");
        assert_eq!(ipv6.headers()[HOST], "[::1]:8080");
    }

    #[test]
    fn connect_uses_authority_form_and_accepts_authority_uri() {
        let mut request = Request::builder()
            .method(Method::CONNECT)
            .uri("example.test:443")
            .body(())
            .unwrap();
        let key = extract_pool_key(request.uri_mut(), true).unwrap();
        assert_eq!(key.0, http::uri::Scheme::HTTPS);
        normalize_h1_request(&mut request, true);
        assert_eq!(request.uri(), &Uri::from_static("example.test:443"));
    }

    #[test]
    fn relative_and_malformed_direct_uris_are_rejected() {
        let mut relative = Uri::from_static("/only-a-path");
        assert_eq!(
            extract_pool_key(&mut relative, false),
            Err(Error::AbsoluteUriRequired)
        );

        // `http::Uri` rejects malformed absolute forms before the public
        // request API can receive a value; this is the boundary proof for a
        // malformed scheme/authority combination.
        assert!("http:/broken".parse::<Uri>().is_err());
    }
}
