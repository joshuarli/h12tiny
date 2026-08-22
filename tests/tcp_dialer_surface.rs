#![cfg(all(feature = "client", feature = "http1"))]

use async_net::TcpStream;
use h12tiny::client::{AsyncRead, AsyncWrite, TcpConnectionIo};

fn assert_tcp_dialer_stream<T: AsyncRead + AsyncWrite + TcpConnectionIo>() {}

#[test]
fn tcp_dialer_reexports_its_futures_io_contract() {
    assert_tcp_dialer_stream::<TcpStream>();
}
