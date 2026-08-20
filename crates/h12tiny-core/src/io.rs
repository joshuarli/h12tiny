//! Adapters between the futures-io and Hyper I/O traits.
//!
//! [`FuturesIo`] is intentionally a very small bridge.  The futures-io read
//! trait accepts an initialized `&mut [u8]`, while Hyper gives adapters a
//! [`hyper::rt::ReadBufCursor`] that may contain uninitialized storage.  The
//! adapter initializes that storage before handing it to futures-io and only
//! advances Hyper's cursor by the number of bytes reported by the read.

use std::pin::Pin;
use std::task::{Context, Poll};

/// Wrap a futures-io stream for use with Hyper.
///
/// The wrapper implements [`hyper::rt::Read`] when `T` implements
/// [`futures_io::AsyncRead`], and [`hyper::rt::Write`] when `T` implements
/// [`futures_io::AsyncWrite`].  It does not own buffering or perform any
/// copying on writes.
#[derive(Debug)]
pub struct FuturesIo<T>(pub T);

impl<T> FuturesIo<T> {
    /// Wrap a futures-io stream.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Borrow the wrapped stream.
    pub fn inner(&self) -> &T {
        &self.0
    }

    /// Mutably borrow the wrapped stream.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Unwrap the stream.
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// Wrap a Hyper runtime stream for use with futures-I/O consumers.
///
/// This is the inverse of [`FuturesIo`]. It is useful after a raw HTTP/1
/// upgrade: Hyper returns an [`hyper::upgrade::Upgraded`] stream implementing
/// its runtime traits, while application-selected framing libraries may use
/// `futures_io::AsyncRead` and `AsyncWrite`.
///
/// The adapter has no framing policy. In particular, it does not implement a
/// WebSocket handshake or any upgraded protocol; it only translates the two
/// poll-based I/O contracts.
#[derive(Debug)]
pub struct HyperIo<T>(pub T);

impl<T> HyperIo<T> {
    /// Wrap a Hyper runtime stream.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Borrow the wrapped stream.
    pub fn inner(&self) -> &T {
        &self.0
    }

    /// Mutably borrow the wrapped stream.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Unwrap the stream.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> hyper::rt::Read for FuturesIo<T>
where
    T: futures_io::AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        // `FuturesIo<T>` has no custom drop implementation and its only field
        // is the wrapped stream.  Projecting a pin to that field therefore
        // preserves the pinning guarantee for `T`.
        let mut inner = unsafe {
            // SAFETY: `self` is pinned and `T` is structurally pinned in the
            // wrapper.  We never move the field; we only project the pinned
            // reference to it for the duration of this call.
            self.map_unchecked_mut(|this| &mut this.0)
        };

        // A zero-capacity Hyper buffer is already complete.  Avoid asking an
        // arbitrary futures-io implementation to interpret an empty read as
        // either EOF or progress.
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        // futures-io's `AsyncRead` takes `&mut [u8]`, so the storage must be
        // initialized before it is exposed to the wrapped implementation.
        // `initialize_unfilled` preserves any bytes already filled in the
        // Hyper buffer and initializes exactly the remaining region.
        let capacity = buf.remaining();
        let dst = buf.initialize_unfilled();
        let n = match futures_io::AsyncRead::poll_read(inner.as_mut(), cx, dst) {
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        };

        // The futures-io contract requires `n <= dst.len()`.  Check it before
        // calling Hyper's unsafe cursor operation so a broken implementation
        // cannot make the cursor point outside its backing allocation.
        if n > capacity {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "futures-io AsyncRead returned more bytes than requested",
            )));
        }

        // SAFETY: `dst` was returned by `ReadBufCursor::initialize_unfilled`,
        // which initializes every byte in the region.  The bounds check above
        // proves that advancing by `n` stays within that region.
        unsafe {
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T> hyper::rt::Write for FuturesIo<T>
where
    T: futures_io::AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: See the corresponding projection in the `Read`
            // implementation.  The wrapper structurally pins its sole field.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_write(inner.as_mut(), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: The wrapped field is structurally pinned and is not
            // moved while this pinned projection is alive.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_flush(inner.as_mut(), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: The wrapped field is structurally pinned and is not
            // moved while this pinned projection is alive.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_close(inner.as_mut(), cx)
    }

    fn is_write_vectored(&self) -> bool {
        // futures-io has no capability query corresponding to Hyper's
        // `is_write_vectored`.  Every implementation does expose
        // `poll_write_vectored`; reporting true lets Hyper use that operation,
        // while the trait's default implementation remains a correct scalar
        // fallback for streams that do not override it.
        true
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: The wrapped field is structurally pinned and is not
            // moved while this pinned projection is alive.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_write_vectored(inner.as_mut(), cx, bufs)
    }
}

impl<T> futures_io::AsyncRead for HyperIo<T>
where
    T: hyper::rt::Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        // `HyperIo<T>` has no custom drop implementation and its only field
        // is structurally pinned, exactly as in the reverse adapter above.
        let mut inner = unsafe {
            // SAFETY: the projection does not move `T`; the pinned reference
            // remains valid for the duration of Hyper's poll call.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        // Futures-I/O supplies initialized byte storage, so Hyper can use a
        // safe `ReadBuf::new` and report progress through its filled length.
        let mut read_buf = hyper::rt::ReadBuf::new(buf);
        match hyper::rt::Read::poll_read(inner.as_mut(), cx, read_buf.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> futures_io::AsyncWrite for HyperIo<T>
where
    T: hyper::rt::Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: see the corresponding `AsyncRead` projection.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        hyper::rt::Write::poll_write(inner.as_mut(), cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: see the corresponding `AsyncRead` projection.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        hyper::rt::Write::poll_write_vectored(inner.as_mut(), cx, bufs)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the corresponding `AsyncRead` projection.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        hyper::rt::Write::poll_flush(inner.as_mut(), cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the corresponding `AsyncRead` projection.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        hyper::rt::Write::poll_shutdown(inner.as_mut(), cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{FuturesIo, HyperIo};
    use futures_io::{AsyncRead, AsyncWrite};
    use hyper::rt::{Read, ReadBuf, Write};
    use std::io::{self, IoSlice};
    use std::pin::Pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        // SAFETY: The no-op functions never dereference or access the data
        // pointer.  The vtable is valid for a Waker with a null data pointer.
        unsafe fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        // SAFETY: `VTABLE` implements all operations for the null data
        // pointer and does not retain or dereference it.
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    #[derive(Debug)]
    struct MemoryIo {
        input: &'static [u8],
        output: Vec<u8>,
        read_calls: usize,
        vectored_calls: usize,
        flushed: bool,
        closed: bool,
    }

    impl AsyncRead for MemoryIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            self.read_calls += 1;
            let n = self.input.len().min(buf.len());
            buf[..n].copy_from_slice(&self.input[..n]);
            self.input = &self.input[n..];
            Poll::Ready(Ok(n))
        }
    }

    impl AsyncWrite for MemoryIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.output.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_write_vectored(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bufs: &[IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            self.vectored_calls += 1;
            let n = bufs.iter().map(|buf| buf.len()).sum::<usize>();
            for buf in bufs {
                self.output.extend_from_slice(buf);
            }
            Poll::Ready(Ok(n))
        }

        fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flushed = true;
            Poll::Ready(Ok(()))
        }

        fn poll_close(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.closed = true;
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn read_advances_only_bytes_reported_by_futures_io() {
        let mut stream = FuturesIo::new(MemoryIo {
            input: b"hello",
            output: Vec::new(),
            read_calls: 0,
            vectored_calls: 0,
            flushed: false,
            closed: false,
        });
        let mut storage = [0u8; 8];
        let mut read_buf = ReadBuf::new(&mut storage);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let result = Read::poll_read(Pin::new(&mut stream), &mut cx, read_buf.unfilled());
        assert!(matches!(result, Poll::Ready(Ok(()))));
        assert_eq!(read_buf.filled(), b"hello");
        assert_eq!(stream.inner().read_calls, 1);

        let result = Read::poll_read(Pin::new(&mut stream), &mut cx, read_buf.unfilled());
        assert!(matches!(result, Poll::Ready(Ok(()))));
        assert_eq!(read_buf.filled(), b"hello");
    }

    #[test]
    fn writes_flush_close_and_vectored_writes_delegate() {
        let mut stream = FuturesIo::new(MemoryIo {
            input: b"",
            output: Vec::new(),
            read_calls: 0,
            vectored_calls: 0,
            flushed: false,
            closed: false,
        });
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let result = Write::poll_write(Pin::new(&mut stream), &mut cx, b"a");
        assert!(matches!(result, Poll::Ready(Ok(1))));
        let bufs = [IoSlice::new(b"bc"), IoSlice::new(b"de")];
        let result = Write::poll_write_vectored(Pin::new(&mut stream), &mut cx, &bufs);
        assert!(matches!(result, Poll::Ready(Ok(4))));
        assert!(Write::is_write_vectored(&stream));
        assert!(matches!(
            Write::poll_flush(Pin::new(&mut stream), &mut cx),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            Write::poll_shutdown(Pin::new(&mut stream), &mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(stream.inner().output, b"abcde");
        assert_eq!(stream.inner().vectored_calls, 1);
        assert!(stream.inner().flushed);
        assert!(stream.inner().closed);
    }

    #[test]
    fn oversized_read_is_rejected_without_advancing_cursor() {
        struct BadReader;
        impl AsyncRead for BadReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                buf: &mut [u8],
            ) -> Poll<io::Result<usize>> {
                Poll::Ready(Ok(buf.len() + 1))
            }
        }

        let mut stream = FuturesIo::new(BadReader);
        let mut storage = [0u8; 2];
        let mut read_buf = ReadBuf::new(&mut storage);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let result = Read::poll_read(Pin::new(&mut stream), &mut cx, read_buf.unfilled());
        assert!(matches!(
            result,
            Poll::Ready(Err(error)) if error.kind() == io::ErrorKind::InvalidData
        ));
        assert!(read_buf.filled().is_empty());
    }

    #[test]
    fn hyper_io_adapts_runtime_streams_to_futures_io() {
        #[derive(Debug)]
        struct HyperMemoryIo {
            input: &'static [u8],
            output: Vec<u8>,
            closed: bool,
        }

        impl Read for HyperMemoryIo {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                mut buf: hyper::rt::ReadBufCursor<'_>,
            ) -> Poll<io::Result<()>> {
                let count = self.input.len().min(buf.remaining());
                buf.put_slice(&self.input[..count]);
                self.input = &self.input[count..];
                Poll::Ready(Ok(()))
            }
        }

        impl Write for HyperMemoryIo {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                self.output.extend_from_slice(buf);
                Poll::Ready(Ok(buf.len()))
            }

            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                self.closed = true;
                Poll::Ready(Ok(()))
            }
        }

        let mut stream = HyperIo::new(HyperMemoryIo {
            input: b"hello",
            output: Vec::new(),
            closed: false,
        });
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut read = [0_u8; 8];
        assert!(matches!(
            AsyncRead::poll_read(Pin::new(&mut stream), &mut cx, &mut read),
            Poll::Ready(Ok(5))
        ));
        assert_eq!(&read[..5], b"hello");
        assert!(matches!(
            AsyncWrite::poll_write(Pin::new(&mut stream), &mut cx, b"world"),
            Poll::Ready(Ok(5))
        ));
        assert!(matches!(
            AsyncWrite::poll_close(Pin::new(&mut stream), &mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(stream.inner().output, b"world");
        assert!(stream.inner().closed);
    }
}
