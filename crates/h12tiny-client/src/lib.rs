#![cfg(any(feature = "http1", feature = "http2"))]

//! Direct-origin HTTP client with legacy Hyper-util pooling semantics.
//!
//! The public client accepts absolute `http` and `https` URIs. It owns origin
//! routing, TLS/ALPN, pooling, and request normalization; Hyper continues to
//! own all HTTP/1 and HTTP/2 protocol machinery.

mod connect;
mod normalize;
mod pool;

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::future::{self, Either};
use http::{Method, Request, Response, Version};
use hyper::body::{Body, Incoming};

use self::normalize::PoolKey;
use self::pool::{CheckoutError, Poolable, Protocol as PoolProtocol, Reservation};
use h12tiny_core::runtime::{AsyncIoTimer, BoxExecutor, BoxSendFuture};

pub use connect::{Connected, ConnectionIo, Connector, ConnectorBuilder, DialError, DialFuture, Dialer};

/// Errors are deliberately classified by the endpoint layer rather than
/// exposing connector/protocol implementation types in the public contract.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    Canceled,
    UnsupportedScheme,
    Connect,
    Tls,
    Alpn,
    Handshake,
    SendRequest,
    UnsupportedMethod,
    UnsupportedVersion,
    AbsoluteUriRequired,
    ProtocolUnavailable,
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn with_source(kind: ErrorKind, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        Self {
            kind,
            source: Some(source.into()),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::Canceled => "request was cancelled before it could be sent",
            ErrorKind::UnsupportedScheme => "request URI scheme is unsupported",
            ErrorKind::Connect => "connection establishment failed",
            ErrorKind::Tls => "TLS negotiation or certificate validation failed",
            ErrorKind::Alpn => "TLS ALPN did not select an allowed HTTP protocol",
            ErrorKind::Handshake => "HTTP connection handshake failed",
            ErrorKind::SendRequest => "request dispatch failed",
            ErrorKind::UnsupportedMethod => "request method is unsupported for this HTTP version",
            ErrorKind::UnsupportedVersion => "request HTTP version is unsupported",
            ErrorKind::AbsoluteUriRequired => "client requests require an absolute URI",
            ErrorKind::ProtocolUnavailable => "the selected protocol was not enabled in this build",
        })
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// Protocol selected for a client connection event.
///
/// This is intentionally separate from Hyper's version type: it describes a
/// reusable endpoint session, not an individual request or response message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionProtocol {
    Http1,
    Http2,
}

/// A discrete endpoint-lifecycle observation recorded by [`DebugEventLog`].
///
/// Events are best-effort development diagnostics. They do not establish a
/// request-success guarantee and are deliberately not a metrics framework.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DebugEvent {
    PoolCheckout { origin: String },
    ConnectionEstablished { origin: String, protocol: ConnectionProtocol },
    AlpnSelected { origin: String, protocol: ConnectionProtocol },
    ConnectionPooled { origin: String },
    PoolEvicted { origin: String },
    ConnectionClosed { origin: String },
    StaleRetry { origin: String },
}

/// An opt-in, dependency-free sink for client connection lifecycle events.
///
/// Clone this before passing it to [`Builder::debug_event_log`], then call
/// [`DebugEventLog::drain`] from a test harness or debugging control path.
/// It is intentionally pull-based: event recording never runs application
/// callbacks while the connection pool mutex is held.
#[derive(Clone, Default)]
pub struct DebugEventLog(Arc<Mutex<Vec<DebugEvent>>>);

impl DebugEventLog {
    /// Returns and clears all observations recorded since the previous drain.
    pub fn drain(&self) -> Vec<DebugEvent> {
        std::mem::take(&mut *self.0.lock().expect("debug event log mutex poisoned"))
    }

    pub fn is_empty(&self) -> bool {
        self.0
            .lock()
            .expect("debug event log mutex poisoned")
            .is_empty()
    }

    pub(crate) fn record(&self, event: DebugEvent) {
        self.0
            .lock()
            .expect("debug event log mutex poisoned")
            .push(event);
    }
}

impl fmt::Debug for DebugEventLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DebugEventLog").finish_non_exhaustive()
    }
}

/// A cheaply cloneable client. Clones share one per-origin pool and one TLS
/// policy. H2 origin coalescing is intentionally not implemented.
pub struct Client<B> {
    config: ClientConfig,
    connector: Connector,
    executor: BoxExecutor,
    #[cfg(feature = "http1")]
    h1_builder: hyper::client::conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_builder: hyper::client::conn::http2::Builder<BoxExecutor>,
    debug_events: Option<DebugEventLog>,
    pool: pool::Pool<PoolClient<B>, PoolKey>,
}

#[derive(Clone, Copy)]
struct ClientConfig {
    retry_canceled_requests: bool,
    set_host: bool,
    protocol: PoolProtocol,
}

/// A future returned by [`Client::request`]. Dropping it is supported
/// cancellation: Hyper closes an affected H1 session or resets only the H2
/// stream, and the pool's drop guards clean up waiters/connecting markers.
#[must_use = "futures do nothing unless polled"]
pub struct ResponseFuture {
    inner: Pin<Box<dyn Future<Output = Result<Response<Incoming>, Error>> + Send>>,
}

impl Future for ResponseFuture {
    type Output = Result<Response<Incoming>, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

impl ResponseFuture {
    fn error(error: Error) -> Self {
        Self {
            inner: Box::pin(async move { Err(error) }),
        }
    }
}

impl fmt::Debug for ResponseFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResponseFuture")
    }
}

impl Client<()> {
    pub fn builder<E>(executor: E) -> Builder
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + 'static,
    {
        Builder::new(executor)
    }
}

impl<B> Client<B>
where
    B: Body + Send + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    pub fn get(&self, uri: http::Uri) -> ResponseFuture
    where
        B: Default,
    {
        let mut request = Request::new(B::default());
        *request.uri_mut() = uri;
        self.request(request)
    }

    pub fn request(&self, mut request: Request<B>) -> ResponseFuture {
        let is_connect = request.method() == Method::CONNECT;
        match request.version() {
            Version::HTTP_11 | Version::HTTP_2 => {}
            Version::HTTP_10 if !is_connect => {}
            Version::HTTP_10 => return ResponseFuture::error(Error::new(ErrorKind::UnsupportedMethod)),
            _ => return ResponseFuture::error(Error::new(ErrorKind::UnsupportedVersion)),
        }

        let key = match normalize::extract_pool_key(request.uri_mut(), is_connect) {
            Ok(key) => key,
            Err(error) => {
                debug_assert_eq!(error, normalize::Error::AbsoluteUriRequired);
                return ResponseFuture::error(Error::with_source(ErrorKind::AbsoluteUriRequired, error));
            }
        };
        let client = self.clone();
        ResponseFuture {
            inner: Box::pin(async move { client.send_request(request, key).await }),
        }
    }

    async fn send_request(
        self,
        mut request: Request<B>,
        key: PoolKey,
    ) -> Result<Response<Incoming>, Error> {
        let absolute_uri = request.uri().clone();
        loop {
            match self.try_send_request(request, key.clone()).await {
                Ok(response) => return Ok(response),
                Err(TrySendError::Final(error)) => return Err(error),
                Err(TrySendError::Retryable {
                    request: returned_request,
                    error,
                    reused,
                }) => {
                    if !self.config.retry_canceled_requests || !reused {
                        return Err(error);
                    }
                    if let Some(events) = &self.debug_events {
                        events.record(DebugEvent::StaleRetry {
                            origin: normalize::pool_key_origin(&key),
                        });
                    }
                    request = returned_request;
                    *request.uri_mut() = absolute_uri.clone();
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        mut request: Request<B>,
        key: PoolKey,
    ) -> Result<Response<Incoming>, TrySendError<B>> {
        let mut pooled = self.connection_for(key).await.map_err(TrySendError::Final)?;
        if pooled.is_http1() {
            if request.version() == Version::HTTP_2 {
                return Err(TrySendError::Final(Error::new(ErrorKind::UnsupportedVersion)));
            }
            normalize::normalize_h1_request(&mut request, self.config.set_host);
        }

        let response = match pooled.try_send_request(request).await {
            Ok(response) => response,
            Err(mut error) => {
                if let Some(request) = error.take_message() {
                    return Err(TrySendError::Retryable {
                        request,
                        reused: pooled.is_reused(),
                        error: Error::with_source(ErrorKind::Canceled, error.into_error()),
                    });
                }
                return Err(TrySendError::Final(Error::with_source(
                    ErrorKind::SendRequest,
                    error.into_error(),
                )));
            }
        };

        // Hyper's H1 sender becomes reusable only once its connection driver
        // says it is ready again. Retaining this `Pooled` value in a driver
        // task is the legacy lifecycle invariant; response-body completion by
        // itself is not a sufficient signal.
        if pooled.is_http2() || !pooled.is_pool_enabled() || pooled.is_ready() {
            drop(pooled);
        } else {
            let executor = self.executor.clone();
            executor.execute(async move {
                let _ = futures_util::future::poll_fn(|cx| pooled.poll_ready(cx)).await;
            });
        }
        Ok(response)
    }

    async fn connection_for(
        &self,
        key: PoolKey,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, Error> {
        loop {
            match self.one_connection_for(key.clone()).await {
                Ok(pooled) => return Ok(pooled),
                Err(ConnectionError::Final(error)) => return Err(error),
                Err(ConnectionError::Retry) if self.config.retry_canceled_requests => continue,
                Err(ConnectionError::Retry) => return Err(Error::new(ErrorKind::Canceled)),
            }
        }
    }

    async fn one_connection_for(
        &self,
        key: PoolKey,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, ConnectionError> {
        if !self.pool.is_enabled() {
            return self.connect_to(key).await.map_err(ConnectionError::Final);
        }
        let checkout = self.pool.checkout(key.clone());
        let connect = Box::pin(self.connect_to(key));
        match future::select(checkout, connect).await {
            Either::Left((Ok(pooled), _)) => Ok(pooled),
            Either::Right((Ok(pooled), _)) => Ok(pooled),
            Either::Left((Err(error), connect)) if error.is_cancellation() => {
                connect.await.map_err(ConnectionError::Final)
            }
            Either::Right((Err(error), checkout)) if error.kind() == ErrorKind::Canceled => {
                match checkout.await {
                    Ok(pooled) => Ok(pooled),
                    Err(CheckoutError::ClosedValue | CheckoutError::NoLongerWanted) => {
                        Err(ConnectionError::Retry)
                    }
                    Err(error) => Err(ConnectionError::Final(Error::with_source(ErrorKind::Connect, error))),
                }
            }
            Either::Left((Err(error), _)) => Err(ConnectionError::Final(Error::with_source(
                ErrorKind::Connect,
                error,
            ))),
            Either::Right((Err(error), _)) => Err(ConnectionError::Final(error)),
        }
    }

    async fn connect_to(
        &self,
        key: PoolKey,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, Error> {
        let origin = normalize::pool_key_origin(&key);
        let mut connecting = self
            .pool
            .connecting(&key, self.config.protocol)
            .ok_or_else(|| Error::new(ErrorKind::Canceled))?;
        let connected = match self
            .connector
            .connect(normalize::pool_key_uri(key.clone()), self.config.protocol == PoolProtocol::Http2)
            .await
        {
            Ok(connected) => connected,
            Err(error) => {
                let kind = error.client_error_kind();
                return Err(Error::with_source(kind, error));
            }
        };

        if let Some(events) = &self.debug_events {
            let protocol = connected.protocol;
            events.record(DebugEvent::ConnectionEstablished {
                origin: origin.clone(),
                protocol,
            });
            if key.0 == http::uri::Scheme::HTTPS {
                events.record(DebugEvent::AlpnSelected { origin, protocol });
            }
        }

        if connected.protocol == ConnectionProtocol::Http2 && self.config.protocol != PoolProtocol::Http2 {
            connecting = connecting
                .alpn_h2(&self.pool)
                .ok_or_else(|| Error::new(ErrorKind::Canceled))?;
        }

        let connection = self.handshake(connected).await?;
        Ok(self.pool.pooled(connecting, connection))
    }

    async fn handshake(&self, connected: Connected) -> Result<PoolClient<B>, Error> {
        match connected.protocol {
            ConnectionProtocol::Http1 => {
                #[cfg(feature = "http1")]
                {
                    let (mut sender, driver) = self
                        .h1_builder
                        .handshake(connected.io)
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    self.executor.execute(async move {
                        let _ = driver.await;
                    });
                    sender
                        .ready()
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    return Ok(PoolClient {
                        sender: Sender::Http1(sender),
                    });
                }
                #[cfg(not(feature = "http1"))]
                {
                    let _ = connected;
                    Err(Error::new(ErrorKind::ProtocolUnavailable))
                }
            }
            ConnectionProtocol::Http2 => {
                #[cfg(feature = "http2")]
                {
                    let (mut sender, driver) = self
                        .h2_builder
                        .handshake(connected.io)
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    self.executor.execute(async move {
                        let _ = driver.await;
                    });
                    sender
                        .ready()
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    return Ok(PoolClient {
                        sender: Sender::Http2(sender),
                    });
                }
                #[cfg(not(feature = "http2"))]
                {
                    let _ = connected;
                    Err(Error::new(ErrorKind::ProtocolUnavailable))
                }
            }
        }
    }
}

impl<B> Clone for Client<B> {
    fn clone(&self) -> Self {
        Self {
            config: self.config,
            connector: self.connector.clone(),
            executor: self.executor.clone(),
            #[cfg(feature = "http1")]
            h1_builder: self.h1_builder.clone(),
            #[cfg(feature = "http2")]
            h2_builder: self.h2_builder.clone(),
            debug_events: self.debug_events.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<B> fmt::Debug for Client<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client")
    }
}

enum TrySendError<B> {
    Retryable {
        request: Request<B>,
        error: Error,
        reused: bool,
    },
    Final(Error),
}

enum ConnectionError {
    Final(Error),
    Retry,
}

struct PoolClient<B> {
    sender: Sender<B>,
}

enum Sender<B> {
    #[cfg(feature = "http1")]
    Http1(hyper::client::conn::http1::SendRequest<B>),
    #[cfg(feature = "http2")]
    Http2(hyper::client::conn::http2::SendRequest<B>),
}

impl<B> PoolClient<B> {
    fn is_http2(&self) -> bool {
        match self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(_) => false,
            #[cfg(feature = "http2")]
            Sender::Http2(_) => true,
        }
    }

    fn is_http1(&self) -> bool {
        !self.is_http2()
    }

    fn is_ready(&self) -> bool {
        match &self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.is_ready(),
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => sender.is_ready(),
        }
    }

    fn poll_ready(
        &mut self,
        #[allow(unused_variables)] cx: &mut Context<'_>,
    ) -> Poll<Result<(), hyper::Error>> {
        match &mut self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.poll_ready(cx),
            #[cfg(feature = "http2")]
            Sender::Http2(_) => Poll::Ready(Ok(())),
        }
    }
}

impl<B> PoolClient<B>
where
    B: Body + Send + 'static,
{
    async fn try_send_request(
        &mut self,
        request: Request<B>,
    ) -> Result<Response<Incoming>, hyper::client::conn::TrySendError<Request<B>>> {
        match &mut self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.try_send_request(request).await,
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => sender.try_send_request(request).await,
        }
    }
}

impl<B> Poolable for PoolClient<B>
where
    B: Send + 'static,
{
    fn is_open(&self) -> bool {
        self.is_ready()
    }

    fn reserve(self) -> Reservation<Self> {
        match self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => Reservation::Unique(Self {
                sender: Sender::Http1(sender),
            }),
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => Reservation::Shared(
                Self {
                    sender: Sender::Http2(sender.clone()),
                },
                Self {
                    sender: Sender::Http2(sender),
                },
            ),
        }
    }

    fn can_share(&self) -> bool {
        self.is_http2()
    }
}

/// Configures a client without exposing Hyper's protocol builders as a second
/// public configuration surface.
pub struct Builder {
    config: ClientConfig,
    connector: Connector,
    executor: BoxExecutor,
    #[cfg(feature = "http1")]
    h1_builder: hyper::client::conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_builder: hyper::client::conn::http2::Builder<BoxExecutor>,
    pool_config: pool::Config,
    pool_timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
    debug_events: Option<DebugEventLog>,
}

impl Builder {
    pub fn new<E>(executor: E) -> Self
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + 'static,
    {
        let executor = BoxExecutor::new(executor);
        #[cfg(feature = "http1")]
        let h1_builder = {
            let mut builder = hyper::client::conn::http1::Builder::new();
            // Keep direct-origin wire output readable and stable for the
            // `Host` normalization contract exercised by raw-wire tests.
            builder.title_case_headers(true);
            builder
        };
        Self {
            config: ClientConfig {
                retry_canceled_requests: true,
                set_host: true,
                protocol: PoolProtocol::Auto,
            },
            connector: Connector::new(),
            #[cfg(feature = "http1")]
            h1_builder,
            #[cfg(feature = "http2")]
            h2_builder: hyper::client::conn::http2::Builder::new(executor.clone()),
            executor,
            pool_config: pool::Config {
                idle_timeout: Some(Duration::from_secs(90)),
                max_idle_per_host: usize::MAX,
            },
            pool_timer: Some(Arc::new(AsyncIoTimer)),
            debug_events: None,
        }
    }

    pub fn connector(&mut self, connector: Connector) -> &mut Self {
        self.connector = connector;
        self
    }

    pub fn pool_idle_timeout(&mut self, timeout: impl Into<Option<Duration>>) -> &mut Self {
        self.pool_config.idle_timeout = timeout.into();
        self
    }

    pub fn pool_max_idle_per_host(&mut self, max_idle: usize) -> &mut Self {
        self.pool_config.max_idle_per_host = max_idle;
        self
    }

    /// Replaces the timer used only for idle-pool eviction. Request deadlines
    /// remain caller-controlled future races; this builder intentionally does
    /// not define a misleading whole-request timeout.
    pub fn pool_timer<T>(&mut self, timer: T) -> &mut Self
    where
        T: hyper::rt::Timer + Send + Sync + 'static,
    {
        self.pool_timer = Some(Arc::new(timer));
        self
    }

    /// Records endpoint lifecycle transitions in `log` without adding a
    /// logging-framework dependency. The caller retains a clone and drains
    /// it when inspection is useful.
    pub fn debug_event_log(&mut self, log: DebugEventLog) -> &mut Self {
        self.debug_events = Some(log);
        self
    }

    pub fn set_host(&mut self, enabled: bool) -> &mut Self {
        self.config.set_host = enabled;
        self
    }

    pub fn retry_canceled_requests(&mut self, enabled: bool) -> &mut Self {
        self.config.retry_canceled_requests = enabled;
        self
    }

    #[cfg(feature = "http2")]
    pub fn http2_only(&mut self, enabled: bool) -> &mut Self {
        self.config.protocol = if enabled {
            PoolProtocol::Http2
        } else {
            PoolProtocol::Auto
        };
        self
    }

    pub fn build<B>(self) -> Client<B> {
        let debug_events = self.debug_events;
        Client {
            config: self.config,
            connector: self.connector,
            executor: self.executor.clone(),
            #[cfg(feature = "http1")]
            h1_builder: self.h1_builder,
            #[cfg(feature = "http2")]
            h2_builder: self.h2_builder,
            debug_events: debug_events.clone(),
            pool: pool::Pool::new(self.pool_config, self.executor, self.pool_timer, debug_events),
        }
    }
}
