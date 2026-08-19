//! Direct DNS, TCP, and TLS connection establishment.
//!
//! This is intentionally a small replacement for Hyper-util's Tokio
//! `HttpConnector`: no proxy discovery, socket tuning, interface binding, or
//! platform TLS. `async-net` resolves and connects the origin, while Rustls
//! authenticates HTTPS and selects the application protocol with ALPN.

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_net::TcpStream;
use futures_util::future::{self, Either};
use http::Uri;
use hyper::rt::Timer;

use h12tiny_core::io::FuturesIo;
use h12tiny_core::runtime::AsyncIoTimer;

/// A raw connection stream accepted by a custom [`Dialer`].
///
/// It is intentionally a Hyper-runtime stream rather than a futures-I/O
/// stream: custom transports may already have their own I/O adaptation. Most
/// futures-I/O callers can return `h12tiny_core::io::FuturesIo(stream)`.
pub trait ConnectionIo: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}

impl<T> ConnectionIo for T where T: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}

type BoxedIo = Box<dyn ConnectionIo>;

/// A successful custom connection establishment.
///
/// The caller declares the HTTP capability selected by its transport. A
/// custom TLS dialer must perform its own ALPN selection before returning
/// this value; h12tiny deliberately does not infer protocol from an opaque
/// custom stream.
pub struct Connected {
    pub(crate) io: BoxedIo,
    pub(crate) protocol: super::ConnectionProtocol,
}

impl Connected {
    /// Wrap a transport stream and its negotiated HTTP capability.
    pub fn new<T>(io: T, protocol: super::ConnectionProtocol) -> Self
    where
        T: ConnectionIo + 'static,
    {
        Self {
            io: Box::new(io),
            protocol,
        }
    }

    /// Returns the protocol capability selected during establishment.
    pub fn protocol(&self) -> super::ConnectionProtocol {
        self.protocol
    }
}

/// Erased error returned by a custom [`Dialer`].
pub type DialError = Box<dyn StdError + Send + Sync>;

/// Future returned by [`Dialer::connect`].
pub type DialFuture = Pin<Box<dyn Future<Output = Result<Connected, DialError>> + Send + 'static>>;

/// Replaces only connection establishment while retaining the client's origin
/// normalization, protocol handshake, and per-origin pool.
///
/// The URI is always the normalized absolute origin, and `require_http2` is
/// true only when the client is configured for HTTP/2-only operation. A
/// dialer is responsible for returning a stream whose declared protocol is
/// compatible with that request. It does not receive individual requests and
/// cannot implement hidden proxy, redirect, or retry policy.
pub trait Dialer: Send + Sync + 'static {
    fn connect(&self, origin: Uri, require_http2: bool) -> DialFuture;
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
    Connect(std::io::Error),
    Custom(DialError),
    Timeout,
    #[cfg(feature = "tls")]
    Tls(std::io::Error),
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
            Self::Connect(error) => error.fmt(f),
            Self::Custom(error) => error.fmt(f),
            Self::Timeout => f.write_str("connection establishment timed out"),
            #[cfg(feature = "tls")]
            Self::Tls(error) => error.fmt(f),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Connect(error) => Some(error),
            Self::Custom(error) => Some(error.as_ref()),
            #[cfg(feature = "tls")]
            Self::Tls(error) => Some(error),
            _ => None,
        }
    }
}

impl Error {
    pub(crate) fn client_error_kind(&self) -> super::ErrorKind {
        match self {
            Self::UnsupportedScheme(_) => super::ErrorKind::UnsupportedScheme,
            #[cfg(feature = "tls")]
            Self::UnexpectedAlpn(_) | Self::RequiredHttp2NotNegotiated => super::ErrorKind::Alpn,
            #[cfg(feature = "tls")]
            Self::InvalidServerName(_) | Self::Tls(_) => super::ErrorKind::Tls,
            #[cfg(not(feature = "tls"))]
            Self::TlsDisabled => super::ErrorKind::Tls,
            Self::MissingHost | Self::Connect(_) | Self::Custom(_) | Self::Timeout => super::ErrorKind::Connect,
        }
    }
}

/// A cloneable direct-origin connector. One client has one TLS policy, which
/// is intentionally not included in the pool key.
#[derive(Clone)]
pub struct Connector {
    kind: ConnectorKind,
    connect_timeout: Option<Duration>,
    timer: Arc<dyn Timer + Send + Sync>,
    #[cfg(feature = "tls")]
    tls: futures_rustls::TlsConnector,
}

#[derive(Clone)]
enum ConnectorKind {
    Default,
    Custom(Arc<dyn Dialer>),
}

/// Small builder for connection-establishment policy.
///
/// This builder intentionally configures only dialing and the optional
/// establishment timeout. Request deadlines and logical body idle timeouts
/// belong to higher layers.
pub struct ConnectorBuilder {
    connector: Connector,
}

impl Default for Connector {
    fn default() -> Self {
        #[cfg(feature = "tls")]
        {
            return Self::with_tls_config(default_tls_config());
        }
        #[cfg(not(feature = "tls"))]
        {
            Self {
                kind: ConnectorKind::Default,
                connect_timeout: None,
                timer: Arc::new(AsyncIoTimer),
            }
        }
    }
}

impl Connector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a small connector-policy builder.
    pub fn builder() -> ConnectorBuilder {
        ConnectorBuilder {
            connector: Self::new(),
        }
    }

    /// Uses a custom dialer while retaining client pooling and request
    /// normalization. This is equivalent to [`Connector::builder`] followed
    /// by [`ConnectorBuilder::dialer`].
    pub fn with_dialer<D>(dialer: D) -> Self
    where
        D: Dialer,
    {
        Self::builder().dialer(dialer).build()
    }

    /// Uses an explicit Rustls client policy. This is the intended hook for
    /// test root stores and private PKI; certificate validation remains owned
    /// by Rustls.
    #[cfg(feature = "tls")]
    pub fn with_tls_config(config: rustls::ClientConfig) -> Self {
        Self {
            kind: ConnectorKind::Default,
            connect_timeout: None,
            timer: Arc::new(AsyncIoTimer),
            tls: futures_rustls::TlsConnector::from(std::sync::Arc::new(config)),
        }
    }

    pub(crate) async fn connect(&self, uri: Uri, require_h2: bool) -> Result<Connected, Error> {
        let connect = Box::pin(self.connect_without_timeout(uri, require_h2));
        match self.connect_timeout {
            None => connect.await,
            Some(timeout) => match future::select(connect, self.timer.sleep(timeout)).await {
                Either::Left((result, _)) => result,
                Either::Right(_) => Err(Error::Timeout),
            },
        }
    }

    async fn connect_without_timeout(&self, uri: Uri, require_h2: bool) -> Result<Connected, Error> {
        if let ConnectorKind::Custom(dialer) = &self.kind {
            return dialer.connect(uri, require_h2).await.map_err(Error::Custom);
        }
        // `http::Uri::host()` preserves RFC authority brackets around IPv6
        // literals. DNS and Rustls `ServerName` require the bare address.
        let host = uri
            .host()
            .ok_or(Error::MissingHost)?
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or_else(|| uri.host().expect("host was checked above"))
            .to_owned();
        let scheme = uri.scheme_str().ok_or_else(|| Error::UnsupportedScheme("".to_owned()))?;
        match scheme {
            "http" => {
                let stream = TcpStream::connect((host.as_str(), uri.port_u16().unwrap_or(80)))
                    .await
                    .map_err(Error::Connect)?;
                Ok(Connected {
                    io: Box::new(FuturesIo(stream)),
                    protocol: if require_h2 {
                        super::ConnectionProtocol::Http2
                    } else {
                        super::ConnectionProtocol::Http1
                    },
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
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(Error::Connect)?;
        let server_name = futures_rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::InvalidServerName(host))?;
        let tls = self.tls.connect(server_name, stream).await.map_err(Error::Tls)?;
        let protocol = match tls.get_ref().1.alpn_protocol() {
            Some(b"h2") => super::ConnectionProtocol::Http2,
            Some(b"http/1.1") | None if !require_h2 => super::ConnectionProtocol::Http1,
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

impl ConnectorBuilder {
    /// Replaces the default DNS/TCP/TLS establishment sequence.
    pub fn dialer<D>(mut self, dialer: D) -> Self
    where
        D: Dialer,
    {
        self.connector.kind = ConnectorKind::Custom(Arc::new(dialer));
        self
    }

    /// Limits DNS, TCP, and TLS establishment. It does not limit a request,
    /// response headers, or a response body.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connector.connect_timeout = Some(timeout);
        self
    }

    /// Replaces the timer used solely for [`ConnectorBuilder::connect_timeout`].
    /// This is mainly useful for deterministic runtimes and tests.
    pub fn timer<T>(mut self, timer: T) -> Self
    where
        T: Timer + Send + Sync + 'static,
    {
        self.connector.timer = Arc::new(timer);
        self
    }

    /// Uses an explicit Rustls client policy for the default dialer.
    #[cfg(feature = "tls")]
    pub fn tls_config(mut self, config: rustls::ClientConfig) -> Self {
        self.connector.tls = futures_rustls::TlsConnector::from(Arc::new(config));
        self
    }

    /// Finalizes the connector policy.
    pub fn build(self) -> Connector {
        self.connector
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{Connector, DialFuture, Dialer, Error};
    use http::Uri;

    #[derive(Clone, Default)]
    struct RecordingDialer(Arc<Mutex<Vec<(Uri, bool)>>>);

    impl Dialer for RecordingDialer {
        fn connect(&self, origin: Uri, require_http2: bool) -> DialFuture {
            self.0.lock().unwrap().push((origin, require_http2));
            Box::pin(async { Err(Box::new(std::io::Error::other("fixture dial failure")) as _) })
        }
    }

    struct PendingDialer;

    impl Dialer for PendingDialer {
        fn connect(&self, _: Uri, _: bool) -> DialFuture {
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn custom_dialer_receives_the_normalized_origin_and_protocol_requirement() {
        let dialer = RecordingDialer::default();
        let connector = Connector::with_dialer(dialer.clone());
        let result = smol::block_on(connector.connect("http://example.test:8080/".parse().unwrap(), true));
        assert!(matches!(result, Err(Error::Custom(_))));
        assert_eq!(
            *dialer.0.lock().unwrap(),
            vec![("http://example.test:8080/".parse().unwrap(), true)]
        );
    }

    #[test]
    fn connect_timeout_cancels_a_pending_dialer() {
        let connector = Connector::builder()
            .dialer(PendingDialer)
            .connect_timeout(Duration::ZERO)
            .build();
        let result = smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));
        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[cfg(feature = "tls")]
    #[test]
    fn default_config_offers_h2_then_http11() {
        assert_eq!(
            super::default_tls_config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }
}
