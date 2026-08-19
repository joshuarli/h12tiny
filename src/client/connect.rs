//! Direct DNS, TCP, and TLS connection establishment.
//!
//! This is intentionally a small replacement for Hyper-util's Tokio
//! `HttpConnector`: no proxy discovery, socket tuning, interface binding, or
//! platform TLS. `async-net` resolves and connects the origin, while Rustls
//! authenticates HTTPS and selects the application protocol with ALPN.

use std::error::Error as StdError;
use std::fmt;

use async_net::TcpStream;
use http::Uri;

use crate::io::FuturesIo;

/// The negotiated protocol capability attached to a successful transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Http1,
    Http2,
}

pub(crate) trait HyperIo: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}
impl<T> HyperIo for T where T: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}

pub(crate) type BoxedIo = Box<dyn HyperIo>;

pub(crate) struct Connected {
    pub(crate) io: BoxedIo,
    pub(crate) protocol: Protocol,
}

#[derive(Debug)]
pub(crate) enum Error {
    MissingHost,
    UnsupportedScheme(String),
    #[cfg(not(feature = "tls"))]
    TlsDisabled,
    #[cfg(feature = "tls")]
    InvalidServerName(String),
    #[cfg(feature = "tls")]
    UnexpectedAlpn(Vec<u8>),
    #[cfg(feature = "tls")]
    RequiredHttp2NotNegotiated,
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHost => f.write_str("URI has no host"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported URI scheme {scheme:?}"),
            #[cfg(not(feature = "tls"))]
            Self::TlsDisabled => f.write_str("HTTPS requires the `tls` feature"),
            #[cfg(feature = "tls")]
            Self::InvalidServerName(host) => write!(f, "invalid TLS server name {host:?}"),
            #[cfg(feature = "tls")]
            Self::UnexpectedAlpn(protocol) => write!(f, "server selected unsupported ALPN {protocol:?}"),
            #[cfg(feature = "tls")]
            Self::RequiredHttp2NotNegotiated => f.write_str("HTTP/2 was required but ALPN did not select h2"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// A cloneable direct-origin connector. One client has one TLS policy, which
/// is intentionally not included in the pool key.
#[derive(Clone)]
pub struct Connector {
    #[cfg(feature = "tls")]
    tls: futures_rustls::TlsConnector,
}

impl Default for Connector {
    fn default() -> Self {
        #[cfg(feature = "tls")]
        {
            return Self::with_tls_config(default_tls_config());
        }
        #[cfg(not(feature = "tls"))]
        {
            Self {}
        }
    }
}

impl Connector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses an explicit Rustls client policy. This is the intended hook for
    /// test root stores and private PKI; certificate validation remains owned
    /// by Rustls.
    #[cfg(feature = "tls")]
    pub fn with_tls_config(config: rustls::ClientConfig) -> Self {
        Self {
            tls: futures_rustls::TlsConnector::from(std::sync::Arc::new(config)),
        }
    }

    pub(crate) async fn connect(&self, uri: Uri, require_h2: bool) -> Result<Connected, Error> {
        let host = uri.host().ok_or(Error::MissingHost)?.to_owned();
        let scheme = uri.scheme_str().ok_or_else(|| Error::UnsupportedScheme("".to_owned()))?;
        match scheme {
            "http" => {
                let stream = TcpStream::connect((host.as_str(), uri.port_u16().unwrap_or(80))).await?;
                Ok(Connected {
                    io: Box::new(FuturesIo(stream)),
                    protocol: if require_h2 { Protocol::Http2 } else { Protocol::Http1 },
                })
            }
            "https" => self.connect_tls(host, uri.port_u16().unwrap_or(443), require_h2).await,
            other => Err(Error::UnsupportedScheme(other.to_owned())),
        }
    }

    #[cfg(feature = "tls")]
    async fn connect_tls(
        &self,
        host: String,
        port: u16,
        require_h2: bool,
    ) -> Result<Connected, Error> {
        let stream = TcpStream::connect((host.as_str(), port)).await?;
        let server_name = futures_rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::InvalidServerName(host))?;
        let tls = self.tls.connect(server_name, stream).await?;
        let protocol = match tls.get_ref().1.alpn_protocol() {
            Some(b"h2") => Protocol::Http2,
            Some(b"http/1.1") | None if !require_h2 => Protocol::Http1,
            None => return Err(Error::RequiredHttp2NotNegotiated),
            Some(other) => return Err(Error::UnexpectedAlpn(other.to_vec())),
        };
        Ok(Connected {
            io: Box::new(FuturesIo(tls)),
            protocol,
        })
    }

    #[cfg(not(feature = "tls"))]
    async fn connect_tls(
        &self,
        _: String,
        _: u16,
        _: bool,
    ) -> Result<Connected, Error> {
        Err(Error::TlsDisabled)
    }
}

#[cfg(feature = "tls")]
fn default_tls_config() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

#[cfg(all(test, feature = "tls"))]
mod tests {
    use super::default_tls_config;

    #[test]
    fn default_config_offers_h2_then_http11() {
        assert_eq!(
            default_tls_config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }
}
