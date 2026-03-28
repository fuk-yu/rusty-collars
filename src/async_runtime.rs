//! Custom picoserve runtime backed by async-io (works on ESP-IDF).

use std::io;
use std::net::TcpStream;

/// Marker type for our async-io based runtime.
pub struct AsyncIoRuntime;

/// Timer implementation using async-io::Timer.
pub struct AsyncIoTimer;

impl picoserve::time::Timer<AsyncIoRuntime> for AsyncIoTimer {
    async fn delay(&self, duration: picoserve::time::Duration) {
        async_io::Timer::after(std::time::Duration::from_millis(duration.as_millis())).await;
    }

    async fn run_with_timeout<F: core::future::Future>(
        &self,
        duration: picoserve::time::Duration,
        future: F,
    ) -> Result<F::Output, picoserve::time::TimeoutError> {
        let timeout =
            async_io::Timer::after(std::time::Duration::from_millis(duration.as_millis()));
        futures_lite::future::or(async { Ok(future.await) }, async {
            timeout.await;
            Err(picoserve::time::TimeoutError)
        })
        .await
    }
}

/// Wrapper around async-io's async TcpStream for picoserve.
pub struct AsyncIoSocket(pub async_io::Async<TcpStream>);

impl picoserve::io::Socket<AsyncIoRuntime> for AsyncIoSocket {
    type Error = io::Error;
    type ReadHalf<'a> = AsyncIoReadHalf<'a>;
    type WriteHalf<'a> = AsyncIoWriteHalf<'a>;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        (AsyncIoReadHalf(&self.0), AsyncIoWriteHalf(&self.0))
    }

    async fn abort<T: picoserve::time::Timer<AsyncIoRuntime>>(
        self,
        _timeouts: &picoserve::Timeouts,
        _timer: &mut T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        Ok(())
    }

    async fn shutdown<T: picoserve::time::Timer<AsyncIoRuntime>>(
        self,
        _timeouts: &picoserve::Timeouts,
        _timer: &mut T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        self.0.get_ref().shutdown(std::net::Shutdown::Both).ok();
        Ok(())
    }
}

/// Read half: wraps a shared reference to Async<TcpStream>.
/// async-io implements AsyncRead for &Async<T> where T: Read, so concurrent
/// read/write on the same socket is safe (TCP is full-duplex).
pub struct AsyncIoReadHalf<'a>(pub &'a async_io::Async<TcpStream>);

impl picoserve::io::ErrorType for AsyncIoReadHalf<'_> {
    type Error = io::Error;
}

impl picoserve::io::Read for AsyncIoReadHalf<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        futures_lite::AsyncReadExt::read(&mut &*self.0, buf).await
    }
}

/// Write half: wraps a shared reference to Async<TcpStream>.
pub struct AsyncIoWriteHalf<'a>(pub &'a async_io::Async<TcpStream>);

impl picoserve::io::ErrorType for AsyncIoWriteHalf<'_> {
    type Error = io::Error;
}

impl picoserve::io::Write for AsyncIoWriteHalf<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        futures_lite::AsyncWriteExt::write(&mut &*self.0, buf).await
    }

    async fn flush(&mut self) -> Result<(), io::Error> {
        futures_lite::AsyncWriteExt::flush(&mut &*self.0).await
    }
}
