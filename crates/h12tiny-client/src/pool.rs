//! Per-origin connection-pool primitives.
//!
//! This is a deliberately close, dependency-amputated port of
//! `hyper-util` 0.1.20 `client/legacy/pool.rs`. The important distinction is
//! structural: HTTP/1 reservations are unique while HTTP/2 reservations are
//! shared. Do not collapse that distinction into response-body completion.

use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::fmt::{self, Debug};
use std::future::Future;
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_channel::oneshot;
use futures_util::future;

use super::{DebugEvent, DebugEventLog};
use h12tiny_core::runtime::BoxExecutor;

/// An item that can participate in a pool. The pool knows no protocol
/// details; the client supplies the H1/H2 reservation semantics here.
pub(crate) trait Poolable: Unpin + Send + Sized + 'static {
    fn is_open(&self) -> bool;
    fn reserve(self) -> Reservation<Self>;
    fn can_share(&self) -> bool;
}

pub(crate) trait Key: Eq + Hash + Clone + Debug + Unpin + Send + 'static {
    /// Produces the endpoint identity shown by a development event. This is a
    /// pool boundary rather than a `Debug` rendering so events remain useful
    /// when a key's internal representation changes.
    fn origin(&self) -> String;
}

impl Key for super::normalize::PoolKey {
    fn origin(&self) -> String {
        super::normalize::pool_key_origin(self)
    }
}

impl Key for String {
    fn origin(&self) -> String {
        self.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Auto,
    Http1,
    Http2,
}

/// The pool's only protocol-specific state transition.
pub(crate) enum Reservation<T> {
    #[cfg(feature = "http2")]
    Shared(T, T),
    #[cfg_attr(not(feature = "http1"), allow(dead_code))]
    Unique(T),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub(crate) idle_timeout: Option<Duration>,
    pub(crate) max_idle_per_host: usize,
    /// Bounds every open H1 socket for an origin, including an in-progress
    /// establishment and an idle socket retained for later reuse.
    pub(crate) max_h1_connections_per_host: usize,
}

impl Config {
    pub(crate) fn enabled(self) -> bool {
        self.max_idle_per_host > 0
    }
}

/// A pool is disabled rather than partially active when `max_idle_per_host`
/// is zero. That is the legacy client's observable contract.
pub(crate) struct Pool<T, K: Key> {
    inner: Option<Arc<Mutex<Inner<T, K>>>>,
    debug_events: Option<DebugEventLog>,
}

struct Inner<T, K: Key> {
    /// H2 has at most one establishment owner per origin. H1 reservations are
    /// counted separately because each H1 socket is unique to one exchange.
    connecting: HashSet<K>,
    h1_connections: HashMap<K, usize>,
    idle: HashMap<K, Vec<Idle<T>>>,
    max_idle_per_host: usize,
    max_h1_connections_per_host: usize,
    waiters: HashMap<K, VecDeque<oneshot::Sender<T>>>,
    idle_interval_ref: Option<oneshot::Sender<Infallible>>,
    executor: BoxExecutor,
    timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
    timeout: Option<Duration>,
    debug_events: Option<DebugEventLog>,
}

impl<T, K: Key> Pool<T, K> {
    pub(crate) fn new(
        config: Config,
        executor: BoxExecutor,
        timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
        debug_events: Option<DebugEventLog>,
    ) -> Self {
        let inner = config.enabled().then(|| {
            Arc::new(Mutex::new(Inner {
                connecting: HashSet::new(),
                h1_connections: HashMap::new(),
                idle: HashMap::new(),
                max_idle_per_host: config.max_idle_per_host,
                max_h1_connections_per_host: config.max_h1_connections_per_host,
                waiters: HashMap::new(),
                idle_interval_ref: None,
                executor,
                timer,
                timeout: config.idle_timeout,
                debug_events: debug_events.clone(),
            }))
        });
        Self {
            inner,
            debug_events,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }
}

impl<T: Poolable, K: Key> Pool<T, K> {
    pub(crate) fn checkout(&self, key: K) -> Checkout<T, K> {
        if let Some(events) = &self.debug_events {
            events.record(DebugEvent::PoolCheckout {
                origin: key.origin(),
            });
        }
        Checkout {
            key,
            pool: self.clone(),
            waiter: None,
        }
    }

    /// Acquires a connection-establishment reservation. H2 has one shared
    /// establishment owner per origin; H1 consumes one bounded socket slot
    /// that is retained until that connection is closed.
    ///
    /// A reservation is represented by a `Connecting` drop guard, so a
    /// cancelled establishment cannot permanently consume a slot or strand
    /// waiters.
    pub(crate) fn connecting(&self, key: &K, protocol: Protocol) -> Option<Connecting<T, K>> {
        if let Some(inner) = &self.inner {
            let mut locked = inner.lock().expect("pool mutex poisoned");
            if protocol == Protocol::Http2 {
                if !locked.connecting.insert(key.clone()) {
                    return None;
                }
                return Some(Connecting {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(inner),
                    h1_slot: false,
                });
            }
            if !locked.reserve_h1_slot(key) {
                return None;
            }
            return Some(Connecting {
                key: key.clone(),
                pool: WeakOpt::downgrade(inner),
                h1_slot: true,
            });
        }
        Some(Connecting {
            key: key.clone(),
            pool: WeakOpt::none(),
            h1_slot: false,
        })
    }

    pub(crate) fn pooled(
        &self,
        #[cfg_attr(not(feature = "http2"), allow(unused_mut))] mut connecting: Connecting<T, K>,
        value: T,
    ) -> Pooled<T, K> {
        let (value, pool, h1_slot) = match &self.inner {
            Some(inner) => match value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_insert, to_return) => {
                    let mut locked = inner.lock().expect("pool mutex poisoned");
                    let pooled = locked.put(connecting.key.clone(), to_insert, inner);
                    locked.connected(&connecting.key);
                    if pooled {
                        if let Some(events) = &self.debug_events {
                            events.record(DebugEvent::ConnectionPooled {
                                origin: connecting.key.origin(),
                            });
                        }
                    }
                    connecting.pool = WeakOpt::none();
                    connecting.h1_slot = false;
                    (to_return, WeakOpt::none(), false)
                }
                Reservation::Unique(value) => {
                    let h1_slot = connecting.h1_slot;
                    connecting.h1_slot = false;
                    // The returned `Pooled` value now owns the H1 slot. Its
                    // eventual drop will either hand the socket to a waiter
                    // or release the slot; this establishment guard must not
                    // wake those waiters as though the dial had failed.
                    connecting.pool = WeakOpt::none();
                    (value, WeakOpt::downgrade(inner), h1_slot)
                }
            },
            None => (value, WeakOpt::none(), false),
        };
        Pooled {
            value: Some(value),
            is_reused: false,
            key: connecting.key.clone(),
            pool,
            h1_slot,
            debug_events: self.debug_events.clone(),
        }
    }

    fn reuse(&self, key: &K, value: T) -> Pooled<T, K> {
        let h1_slot = !value.can_share();
        let pool = if !h1_slot {
            WeakOpt::none()
        } else {
            self.inner
                .as_ref()
                .map_or_else(WeakOpt::none, WeakOpt::downgrade)
        };
        Pooled {
            value: Some(value),
            is_reused: true,
            key: key.clone(),
            pool,
            h1_slot,
            debug_events: self.debug_events.clone(),
        }
    }
}

impl<T, K: Key> Clone for Pool<T, K> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            debug_events: self.debug_events.clone(),
        }
    }
}

struct Idle<T> {
    value: T,
    idle_at: Instant,
}

struct IdlePopper<'a, T, K> {
    _key: &'a K,
    list: &'a mut Vec<Idle<T>>,
}

impl<T: Poolable, K: Key> IdlePopper<'_, T, K> {
    fn pop(
        self,
        expiration: Expiration,
        now: Instant,
        debug_events: Option<&DebugEventLog>,
    ) -> Option<Idle<T>> {
        while let Some(entry) = self.list.pop() {
            if !entry.value.is_open() || expiration.expires(entry.idle_at, now) {
                if let Some(events) = debug_events {
                    events.record(DebugEvent::PoolEvicted {
                        origin: self._key.origin(),
                    });
                }
                continue;
            }
            let value = match entry.value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_reinsert, to_checkout) => {
                    self.list.push(Idle {
                        idle_at: now,
                        value: to_reinsert,
                    });
                    to_checkout
                }
                Reservation::Unique(value) => value,
            };
            return Some(Idle {
                value,
                idle_at: entry.idle_at,
            });
        }
        None
    }
}

impl<T: Poolable, K: Key> Inner<T, K> {
    fn now(&self) -> Instant {
        self.timer
            .as_ref()
            .map_or_else(Instant::now, |timer| timer.now())
    }

    fn put(&mut self, key: K, value: T, pool: &Arc<Mutex<Self>>) -> bool {
        if value.can_share() && self.idle.contains_key(&key) {
            return false;
        }

        let mut value = Some(value);
        let mut remove_waiters = false;
        if let Some(waiters) = self.waiters.get_mut(&key) {
            while let Some(sender) = waiters.pop_front() {
                if sender.is_canceled() {
                    continue;
                }
                let candidate = value.take().expect("connection was consumed twice");
                let reserved = match candidate.reserve() {
                    #[cfg(feature = "http2")]
                    Reservation::Shared(to_keep, to_send) => {
                        value = Some(to_keep);
                        to_send
                    }
                    Reservation::Unique(value) => value,
                };
                match sender.send(reserved) {
                    Ok(()) if value.is_none() => break,
                    Ok(()) => continue,
                    Err(returned) => value = Some(returned),
                }
            }
            remove_waiters = waiters.is_empty();
        }
        if remove_waiters {
            self.waiters.remove(&key);
        }

        if let Some(value) = value {
            let now = self.now();
            let list = self.idle.entry(key).or_default();
            if list.len() >= self.max_idle_per_host {
                return false;
            }
            list.push(Idle {
                value,
                idle_at: now,
            });
            self.spawn_idle_interval(pool);
            true
        } else {
            true
        }
    }

    fn reserve_h1_slot(&mut self, key: &K) -> bool {
        let connections = self.h1_connections.entry(key.clone()).or_default();
        if *connections >= self.max_h1_connections_per_host {
            return false;
        }
        *connections += 1;
        true
    }

    fn release_h1_slot(&mut self, key: &K) {
        let remove = self
            .h1_connections
            .get_mut(key)
            .map(|connections| {
                debug_assert!(*connections > 0, "H1 slot was released twice");
                *connections = connections.saturating_sub(1);
                *connections == 0
            })
            .unwrap_or(false);
        if remove {
            self.h1_connections.remove(key);
        }
    }

    /// Removes an establishment marker and wakes ownership waiters by
    /// dropping their senders. Their parent client futures then retry rather
    /// than remaining parked behind a cancelled connecting task.
    #[cfg_attr(not(feature = "http2"), allow(dead_code))]
    fn connected(&mut self, key: &K) {
        self.connecting.remove(key);
        self.waiters.remove(key);
    }

    fn connecting_cancelled(&mut self, key: &K, h1_slot: bool) {
        if h1_slot {
            self.release_h1_slot(key);
        } else {
            self.connecting.remove(key);
        }
        self.waiters.remove(key);
    }

    fn clean_waiters(&mut self, key: &K) {
        let remove = self
            .waiters
            .get_mut(key)
            .map(|waiters| {
                waiters.retain(|sender| !sender.is_canceled());
                waiters.is_empty()
            })
            .unwrap_or(false);
        if remove {
            self.waiters.remove(key);
        }
    }

    fn clear_expired(&mut self) {
        let timeout = self.timeout.expect("idle task requires a timeout");
        let now = self.now();
        let debug_events = self.debug_events.clone();
        self.idle.retain(|key, entries| {
            entries.retain(|entry| {
                let keep = entry.value.is_open()
                    && now.saturating_duration_since(entry.idle_at) <= timeout;
                if !keep {
                    if let Some(events) = &debug_events {
                        events.record(DebugEvent::PoolEvicted {
                            origin: key.origin(),
                        });
                    }
                }
                keep
            });
            !entries.is_empty()
        });
    }

    fn spawn_idle_interval(&mut self, pool: &Arc<Mutex<Self>>) {
        if self.idle_interval_ref.is_some() {
            return;
        }
        let Some(timeout) = self.timeout.filter(|timeout| *timeout > Duration::ZERO) else {
            return;
        };
        let Some(timer) = self.timer.clone() else {
            return;
        };
        let (sender, receiver) = oneshot::channel();
        self.idle_interval_ref = Some(sender);
        let task = IdleTask {
            timer,
            duration: timeout.max(Duration::from_millis(90)),
            pool: WeakOpt::downgrade(pool),
            pool_drop_notifier: receiver,
        };
        self.executor.execute(task.run());
    }
}

/// A checked-out connection. Dropping a unique H1 reservation returns it to
/// the pool only when Hyper reports it still usable; shared H2 values already
/// have their pool copy before being checked out.
pub(crate) struct Pooled<T: Poolable, K: Key> {
    value: Option<T>,
    is_reused: bool,
    key: K,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    h1_slot: bool,
    debug_events: Option<DebugEventLog>,
}

impl<T: Poolable, K: Key> Pooled<T, K> {
    pub(crate) fn is_reused(&self) -> bool {
        self.is_reused
    }

    pub(crate) fn is_pool_enabled(&self) -> bool {
        self.pool.0.is_some()
    }
}

impl<T: Poolable, K: Key> Deref for Pooled<T, K> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("pooled value was taken")
    }
}

impl<T: Poolable, K: Key> DerefMut for Pooled<T, K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("pooled value was taken")
    }
}

impl<T: Poolable, K: Key> Drop for Pooled<T, K> {
    fn drop(&mut self) {
        let Some(value) = self.value.take() else {
            return;
        };
        if !value.is_open() {
            self.release_h1_slot();
            if let Some(events) = &self.debug_events {
                events.record(DebugEvent::ConnectionClosed {
                    origin: self.key.origin(),
                });
            }
            return;
        }
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                if inner.put(self.key.clone(), value, &pool) {
                    if let Some(events) = &self.debug_events {
                        events.record(DebugEvent::ConnectionPooled {
                            origin: self.key.origin(),
                        });
                    }
                } else if self.h1_slot {
                    inner.release_h1_slot(&self.key);
                }
            }
        } else {
            self.release_h1_slot();
        }
    }
}

impl<T: Poolable, K: Key> Pooled<T, K> {
    fn release_h1_slot(&self) {
        if self.h1_slot {
            if let Some(pool) = self.pool.upgrade() {
                if let Ok(mut inner) = pool.lock() {
                    inner.release_h1_slot(&self.key);
                }
            }
        }
    }
}

pub(crate) struct Checkout<T: Poolable, K: Key> {
    key: K,
    pool: Pool<T, K>,
    waiter: Option<oneshot::Receiver<T>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutError {
    PoolDisabled,
    NoLongerWanted,
    ClosedValue,
}

impl CheckoutError {
    pub(crate) fn is_cancellation(self) -> bool {
        matches!(self, Self::ClosedValue)
    }
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PoolDisabled => "connection pool is disabled",
            Self::NoLongerWanted => "pool checkout was cancelled",
            Self::ClosedValue => "checked-out connection was closed",
        })
    }
}

impl std::error::Error for CheckoutError {}

impl<T: Poolable, K: Key> Checkout<T, K> {
    fn poll_waiter(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Pooled<T, K>, CheckoutError>>> {
        let Some(mut receiver) = self.waiter.take() else {
            return Poll::Ready(None);
        };
        match Pin::new(&mut receiver).poll(cx) {
            Poll::Ready(Ok(value)) if value.is_open() => {
                Poll::Ready(Some(Ok(self.pool.reuse(&self.key, value))))
            }
            Poll::Ready(Ok(_)) => Poll::Ready(Some(Err(CheckoutError::ClosedValue))),
            Poll::Ready(Err(_)) => Poll::Ready(Some(Err(CheckoutError::NoLongerWanted))),
            Poll::Pending => {
                self.waiter = Some(receiver);
                Poll::Pending
            }
        }
    }

    fn checkout(&mut self, cx: &mut Context<'_>) -> Option<Pooled<T, K>> {
        let entry = {
            let inner = self.pool.inner.as_ref()?;
            let mut inner = inner.lock().expect("pool mutex poisoned");
            let now = inner.now();
            let expiration = Expiration(inner.timeout);
            let debug_events = inner.debug_events.clone();
            let maybe_entry = inner.idle.get_mut(&self.key).and_then(|entries| {
                let entry = IdlePopper {
                    _key: &self.key,
                    list: entries,
                }
                .pop(expiration, now, debug_events.as_ref());
                entry.map(|entry| (entry, entries.is_empty()))
            });
            let (entry, remove) =
                maybe_entry.map_or((None, true), |(entry, empty)| (Some(entry), empty));
            if remove {
                inner.idle.remove(&self.key);
            }
            if entry.is_none() && self.waiter.is_none() {
                let (sender, mut receiver) = oneshot::channel();
                inner
                    .waiters
                    .entry(self.key.clone())
                    .or_default()
                    .push_back(sender);
                assert!(Pin::new(&mut receiver).poll(cx).is_pending());
                self.waiter = Some(receiver);
            }
            entry
        };
        entry.map(|entry| self.pool.reuse(&self.key, entry.value))
    }
}

impl<T: Poolable, K: Key> Future for Checkout<T, K> {
    type Output = Result<Pooled<T, K>, CheckoutError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(Some(result)) = self.poll_waiter(cx) {
            return Poll::Ready(result);
        }
        if let Some(pooled) = self.checkout(cx) {
            Poll::Ready(Ok(pooled))
        } else if !self.pool.is_enabled() {
            Poll::Ready(Err(CheckoutError::PoolDisabled))
        } else {
            Poll::Pending
        }
    }
}

impl<T: Poolable, K: Key> Drop for Checkout<T, K> {
    fn drop(&mut self) {
        if self.waiter.take().is_some() {
            if let Some(inner) = &self.pool.inner {
                if let Ok(mut inner) = inner.lock() {
                    inner.clean_waiters(&self.key);
                }
            }
        }
    }
}

/// Drop guard for an in-progress H2 connection establishment.
pub(crate) struct Connecting<T: Poolable, K: Key> {
    key: K,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    h1_slot: bool,
}

impl<T: Poolable, K: Key> Connecting<T, K> {
    /// An auto protocol connection that negotiated H2 releases its temporary
    /// H1 slot and converts to the shared H2 marker atomically. If another
    /// connection won, `None` asks the caller to wait for the winner rather
    /// than stampeding another H2 session.
    pub(crate) fn alpn_h2(mut self, pool: &Pool<T, K>) -> Option<Self> {
        if self.h1_slot {
            if let Some(inner) = self.pool.upgrade() {
                if let Ok(mut inner) = inner.lock() {
                    inner.release_h1_slot(&self.key);
                }
            }
            self.h1_slot = false;
        }
        self.pool = WeakOpt::none();
        pool.connecting(&self.key, Protocol::Http2)
    }
}

impl<T: Poolable, K: Key> Drop for Connecting<T, K> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                inner.connecting_cancelled(&self.key, self.h1_slot);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Expiration(Option<Duration>);

impl Expiration {
    fn expires(self, idle_at: Instant, now: Instant) -> bool {
        self.0
            .map(|timeout| now.saturating_duration_since(idle_at) > timeout)
            .unwrap_or(false)
    }
}

struct IdleTask<T: Poolable, K: Key> {
    timer: Arc<dyn hyper::rt::Timer + Send + Sync>,
    duration: Duration,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    pool_drop_notifier: oneshot::Receiver<Infallible>,
}

impl<T: Poolable, K: Key> IdleTask<T, K> {
    async fn run(self) {
        let mut sleep = self.timer.sleep_until(self.timer.now() + self.duration);
        let mut pool_drop = self.pool_drop_notifier;
        loop {
            match future::select(&mut pool_drop, &mut sleep).await {
                future::Either::Left(_) => break,
                future::Either::Right(((), _)) => {
                    if let Some(pool) = self.pool.upgrade() {
                        if let Ok(mut inner) = pool.lock() {
                            inner.clear_expired();
                        }
                    }
                    self.timer
                        .reset(&mut sleep, self.timer.now() + self.duration);
                }
            }
        }
    }
}

struct WeakOpt<T>(Option<Weak<T>>);

impl<T> WeakOpt<T> {
    fn none() -> Self {
        Self(None)
    }

    fn downgrade(value: &Arc<T>) -> Self {
        Self(Some(Arc::downgrade(value)))
    }

    fn upgrade(&self) -> Option<Arc<T>> {
        self.0.as_ref().and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use futures_lite::future::block_on;

    use super::{
        Checkout, CheckoutError, Config, Connecting, Pool, Poolable, Protocol, Reservation, WeakOpt,
    };
    use h12tiny_core::runtime::{AsyncIoTimer, BoxExecutor, BoxSendFuture};

    #[derive(Debug, PartialEq, Eq)]
    struct Unique(u8);

    impl Poolable for Unique {
        fn is_open(&self) -> bool {
            true
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    #[cfg(feature = "http2")]
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Shared(u8);

    #[cfg(feature = "http2")]
    impl Poolable for Shared {
        fn is_open(&self) -> bool {
            true
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Shared(self.clone(), self)
        }

        fn can_share(&self) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct Noop;

    impl<F> hyper::rt::Executor<F> for Noop
    where
        F: Future<Output = ()> + Send + 'static,
    {
        fn execute(&self, _: F) {}
    }

    #[derive(Clone)]
    struct SmolExecutor;

    impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
        fn execute(&self, future: BoxSendFuture) {
            smol::spawn(future).detach();
        }
    }

    fn pool() -> Pool<Unique, String> {
        Pool::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        )
    }

    fn capped_h1_pool(max_h1_connections_per_host: usize) -> Pool<Unique, String> {
        Pool::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        )
    }

    fn connecting(key: String) -> Connecting<Unique, String> {
        Connecting {
            key,
            pool: WeakOpt::none(),
            h1_slot: false,
        }
    }

    fn poll_once<T: Poolable>(
        checkout: &mut Checkout<T, String>,
    ) -> Poll<Result<super::Pooled<T, String>, CheckoutError>> {
        let mut cx = Context::from_waker(Waker::noop());
        Pin::new(checkout).poll(&mut cx)
    }

    #[test]
    fn unique_reservation_returns_to_idle_pool_on_drop() {
        let pool = pool();
        let key = "example.test".to_owned();
        drop(pool.pooled(connecting(key.clone()), Unique(7)));
        let checked_out = block_on(pool.checkout(key)).unwrap();
        assert_eq!(*checked_out, Unique(7));
    }

    #[test]
    fn h2_connecting_marker_is_released_when_owner_is_cancelled() {
        let pool = pool();
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http2).unwrap();
        assert!(pool.connecting(&key, Protocol::Http2).is_none());
        drop(owner);
        assert!(pool.connecting(&key, Protocol::Http2).is_some());
    }

    #[test]
    fn dropped_checkout_removes_its_waiter() {
        let pool = pool();
        let key = "example.test".to_owned();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());
        assert_eq!(
            pool.inner.as_ref().unwrap().lock().unwrap().waiters[&key].len(),
            1
        );
        drop(checkout);
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .waiters
            .contains_key(&key));
    }

    #[test]
    fn cancelled_h2_establishment_wakes_waiter_and_allows_retry() {
        let pool = pool();
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http2).unwrap();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());

        // Dropping the sole establishment owner clears the marker and drops
        // parked senders. A later client attempt owns a fresh marker instead
        // of inheriting a stranded waiter.
        drop(owner);
        assert!(matches!(
            poll_once(&mut checkout),
            Poll::Ready(Err(CheckoutError::NoLongerWanted))
        ));
        assert!(pool.connecting(&key, Protocol::Http2).is_some());
    }

    #[test]
    fn max_idle_per_host_caps_unique_h1_connections() {
        let pool = pool();
        let key = "example.test".to_owned();
        for value in [1, 2, 3] {
            drop(pool.pooled(connecting(key.clone()), Unique(value)));
        }
        assert_eq!(
            pool.inner.as_ref().unwrap().lock().unwrap().idle[&key].len(),
            2
        );
    }

    #[test]
    fn h1_connection_cap_includes_connecting_and_idle_connections() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        assert!(pool.connecting(&key, Protocol::Http1).is_none());

        drop(pool.pooled(connecting, Unique(7)));
        assert!(pool.connecting(&key, Protocol::Http1).is_none());

        let checked_out = block_on(pool.checkout(key.clone())).unwrap();
        assert_eq!(*checked_out, Unique(7));
        drop(checked_out);
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
    }

    #[test]
    fn established_h1_connection_is_returned_to_a_waiter_at_the_connection_cap() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        let mut checkout = pool.checkout(key);
        assert!(poll_once(&mut checkout).is_pending());

        drop(pool.pooled(connecting, Unique(7)));
        let checked_out = match poll_once(&mut checkout) {
            Poll::Ready(Ok(checked_out)) => checked_out,
            _ => panic!("established H1 connection did not wake its waiter"),
        };
        assert_eq!(*checked_out, Unique(7));
    }

    #[test]
    fn cancelled_h1_establishment_wakes_waiters_and_releases_its_slot() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());

        drop(owner);
        assert!(matches!(
            poll_once(&mut checkout),
            Poll::Ready(Err(CheckoutError::NoLongerWanted))
        ));
        assert!(pool.connecting(&key, Protocol::Http1).is_some());
    }

    #[derive(Debug)]
    struct Closed;

    impl Poolable for Closed {
        fn is_open(&self) -> bool {
            false
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    #[test]
    fn closed_values_are_not_reinserted() {
        let pool = Pool::<Closed, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: 1,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(pool.pooled(connecting, Closed));
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .idle
            .contains_key(&key));
        assert!(pool.connecting(&key, Protocol::Http1).is_some());
    }

    #[test]
    fn checkout_evicts_expired_idle_connections() {
        let pool = Pool::<Unique, String>::new(
            Config {
                idle_timeout: Some(Duration::from_millis(1)),
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        drop(pool.pooled(connecting(key.clone()), Unique(7)));
        std::thread::sleep(Duration::from_millis(5));
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .idle
            .contains_key(&key));
    }

    #[test]
    fn idle_timer_evicts_connections_without_a_later_checkout() {
        smol::block_on(async {
            let pool = Pool::<Unique, String>::new(
                Config {
                    idle_timeout: Some(Duration::from_millis(1)),
                    max_idle_per_host: 2,
                    max_h1_connections_per_host: usize::MAX,
                },
                BoxExecutor::new(SmolExecutor),
                Some(std::sync::Arc::new(AsyncIoTimer)),
                None,
            );
            let key = "example.test".to_owned();
            drop(pool.pooled(connecting(key.clone()), Unique(7)));
            assert!(pool
                .inner
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .idle
                .contains_key(&key));

            // The recurring task intentionally uses a conservative 90 ms
            // minimum interval to avoid a busy timer for tiny user-supplied
            // values. No checkout is performed after this wait.
            async_io::Timer::after(Duration::from_millis(120)).await;
            assert!(!pool
                .inner
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .idle
                .contains_key(&key));
        });
    }

    #[cfg(feature = "http2")]
    #[test]
    fn auto_connection_releases_its_h1_slot_when_alpn_selects_h2() {
        let pool = Pool::<Shared, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: 1,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let h2_connecting = pool
            .connecting(&key, Protocol::Auto)
            .unwrap()
            .alpn_h2(&pool)
            .unwrap();

        assert!(pool.connecting(&key, Protocol::Http1).is_some());
        drop(h2_connecting);
    }

    #[cfg(feature = "http2")]
    #[test]
    fn shared_reservation_remains_available_while_an_h2_stream_is_checked_out() {
        let pool = Pool::<Shared, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let owner = Connecting {
            key: key.clone(),
            pool: WeakOpt::none(),
            h1_slot: false,
        };
        let first = pool.pooled(owner, Shared(9));
        let second = block_on(pool.checkout(key)).unwrap();
        assert_eq!(*first, Shared(9));
        assert_eq!(*second, Shared(9));
    }
}
