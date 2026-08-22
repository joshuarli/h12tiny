//! Direct DNS, TCP, and TLS connection establishment.
//!
//! This is intentionally a small replacement for Hyper-util's Tokio
//! `HttpConnector`: no proxy discovery, socket tuning, interface binding, or
//! platform TLS. The default path resolves names with the standard library and
//! connects sockets with `async-io`; Rustls authenticates HTTPS and selects the
//! application protocol with ALPN.
//!
//! The standard library resolver is synchronous, so name resolution can block
//! the task that first polls a default connection. This is deliberate: the
//! client does not create or depend on a process-global blocking worker pool.
//! Applications that need asynchronous or otherwise specialized DNS should use
//! [`Dialer`] to replace the complete establishment sequence.

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{TcpStream as StdTcpStream, ToSocketAddrs};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_io::Async;
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
            Self::UnexpectedAlpn(protocol) => {
                write!(f, "server selected unsupported ALPN {protocol:?}")
            }
            #[cfg(feature = "tls")]
            Self::RequiredHttp2NotNegotiated => {
                f.write_str("HTTP/2 was required but ALPN did not select h2")
            }
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
            Self::MissingHost | Self::Connect(_) | Self::Custom(_) | Self::Timeout => {
                super::ErrorKind::Connect
            }
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

/// Builds a Rustls client configuration without relying on Rustls' process
/// global provider selection.
///
/// [`ClientTlsConfigBuilder::new`] uses h12tiny's Graviola provider, the
/// WebPKI roots, no client certificate, and ALPN offerings for HTTP/2 then
/// HTTP/1.1. Callers can replace the root store, ALPN list, or client
/// authentication while retaining that explicit provider. Applications with a
/// different Rustls provider can use [`ClientTlsConfigBuilder::with_provider`]
/// instead; this is useful when several Rustls consumers share one process.
///
/// For policies requiring a custom verifier or other Rustls-only settings,
/// construct a [`rustls::ClientConfig`] directly and pass it to
/// [`Connector::with_tls_config`] or [`ConnectorBuilder::tls_config`].
#[cfg(feature = "tls")]
pub struct ClientTlsConfigBuilder {
    provider: Arc<rustls::crypto::CryptoProvider>,
    roots: rustls::RootCertStore,
    alpn_protocols: Vec<Vec<u8>>,
    client_auth: ClientAuthentication,
}

#[cfg(feature = "tls")]
enum ClientAuthentication {
    None,
    Certificates {
        certificate_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    },
}

#[cfg(feature = "tls")]
impl ClientTlsConfigBuilder {
    /// Starts with h12tiny's explicit Graviola provider and standard HTTPS
    /// client defaults.
    pub fn new() -> Self {
        Self::with_provider(Arc::new(rustls_graviola::default_provider()))
    }

    /// Starts with an explicitly supplied Rustls provider.
    ///
    /// [`Self::build`] returns any compatibility error, for example when the
    /// provider cannot support Rustls' safe default protocol versions. This
    /// method never installs or reads Rustls' process-global provider.
    pub fn with_provider(provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self {
            provider,
            roots: rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            ),
            alpn_protocols: vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            client_auth: ClientAuthentication::None,
        }
    }

    /// Replaces the trust store used for server certificate validation.
    pub fn root_certificates(mut self, roots: rustls::RootCertStore) -> Self {
        self.roots = roots;
        self
    }

    /// Adds one trust anchor to the existing trust store.
    pub fn add_root_certificate(
        mut self,
        certificate: rustls::pki_types::CertificateDer<'static>,
    ) -> Result<Self, rustls::Error> {
        self.roots.add(certificate)?;
        Ok(self)
    }

    /// Replaces the ALPN protocol offerings sent during TLS negotiation.
    ///
    /// The default offers HTTP/2 followed by HTTP/1.1. Supplying an empty
    /// list deliberately disables ALPN negotiation.
    pub fn alpn_protocols(
        mut self,
        protocols: impl IntoIterator<Item = Vec<u8>>,
    ) -> Self {
        self.alpn_protocols = protocols.into_iter().collect();
        self
    }

    /// Configures no TLS client authentication, which is the default.
    pub fn no_client_auth(mut self) -> Self {
        self.client_auth = ClientAuthentication::None;
        self
    }

    /// Configures a certificate chain and private key for mutual TLS.
    pub fn client_auth(
        mut self,
        certificate_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> Self {
        self.client_auth = ClientAuthentication::Certificates {
            certificate_chain,
            private_key,
        };
        self
    }

    /// Finishes the Rustls configuration.
    pub fn build(self) -> Result<rustls::ClientConfig, rustls::Error> {
        let builder = rustls::ClientConfig::builder_with_provider(self.provider)
            .with_safe_default_protocol_versions()?;
        let mut config = match self.client_auth {
            ClientAuthentication::None => builder
                .with_root_certificates(self.roots)
                .with_no_client_auth(),
            ClientAuthentication::Certificates {
                certificate_chain,
                private_key,
            } => builder
                .with_root_certificates(self.roots)
                .with_client_auth_cert(certificate_chain, private_key)?,
        };
        config.alpn_protocols = self.alpn_protocols;
        Ok(config)
    }
}

#[cfg(feature = "tls")]
impl Default for ClientTlsConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
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

    /// Uses h12tiny's standard TLS policy with an explicitly supplied Rustls
    /// provider. This does not install or consult Rustls' process-global
    /// provider selection.
    #[cfg(feature = "tls")]
    pub fn with_tls_provider(
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .map(Self::with_tls_config)
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

    async fn connect_without_timeout(
        &self,
        uri: Uri,
        require_h2: bool,
    ) -> Result<Connected, Error> {
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
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| Error::UnsupportedScheme("".to_owned()))?;
        match scheme {
            "http" => {
                let stream = connect_tcp(&host, uri.port_u16().unwrap_or(80))
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
            "https" => {
                self.connect_tls(host, uri.port_u16().unwrap_or(443), require_h2)
                    .await
            }
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
        let stream = connect_tcp(&host, port).await.map_err(Error::Connect)?;
        let server_name = futures_rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::InvalidServerName(host))?;
        let tls = self
            .tls
            .connect(server_name, stream)
            .await
            .map_err(Error::Tls)?;
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
    async fn connect_tls(&self, _: String, _: u16, _: bool) -> Result<Connected, Error> {
        Err(Error::TlsDisabled)
    }
}

/// Resolves an origin synchronously, then establishes each candidate socket
/// asynchronously until one succeeds. Keeping resolution here, rather than
/// using `async-net`, avoids its process-global `blocking` worker pool while
/// preserving async TCP connect and cancellation after resolution completes.
async fn connect_tcp(host: &str, port: u16) -> io::Result<Async<StdTcpStream>> {
    let mut last_error = None;
    let mut addresses = (host, port).to_socket_addrs()?;

    while let Some(address) = addresses.next() {
        match Async::<StdTcpStream>::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not connect to any of the addresses",
        )
    }))
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

    /// Limits connection establishment after the default resolver yields,
    /// including TCP and TLS. The default resolver uses synchronous
    /// `ToSocketAddrs`, so this timer cannot preempt a DNS lookup that blocks
    /// while its future is first polled. Use [`ConnectorBuilder::dialer`] for
    /// asynchronous DNS that must be covered by cancellation and this timer.
    /// This does not limit a request, response headers, or a response body.
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

    /// Uses h12tiny's standard TLS policy with an explicitly supplied Rustls
    /// provider. This does not install or consult Rustls' process-global
    /// provider selection.
    #[cfg(feature = "tls")]
    pub fn tls_provider(
        self,
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .map(|config| self.tls_config(config))
    }

    /// Finalizes the connector policy.
    pub fn build(self) -> Connector {
        self.connector
    }
}

#[cfg(feature = "tls")]
fn default_tls_config() -> rustls::ClientConfig {
    ClientTlsConfigBuilder::new()
        .build()
        .expect("Graviola supports Rustls' safe default protocol versions")
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{Connector, DialFuture, Dialer, Error};
    #[cfg(feature = "tls")]
    use super::ClientTlsConfigBuilder;
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
        let result =
            smol::block_on(connector.connect("http://example.test:8080/".parse().unwrap(), true));
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
        let result =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));
        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[test]
    fn default_tcp_connector_resolves_and_connects_without_async_net() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let connector = Connector::new();
        let connected = smol::block_on(
            connector.connect(format!("http://127.0.0.1:{port}/").parse().unwrap(), false),
        )
        .unwrap();

        assert_eq!(connected.protocol, super::super::ConnectionProtocol::Http1);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn default_config_offers_h2_then_http11() {
        assert_eq!(
            super::default_tls_config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_builder_constructs_an_explicit_graviola_config() {
        let config = ClientTlsConfigBuilder::new()
            .alpn_protocols([b"http/1.1".to_vec()])
            .build()
            .unwrap();

        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_builder_accepts_an_explicit_provider() {
        let provider = Arc::new(rustls_graviola::default_provider());
        let config = ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .unwrap();

        assert_eq!(config.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }
}
