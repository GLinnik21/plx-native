//! The one accept for a loopback test server whose listener is nonblocking.
//!
//! A fixture makes its listener nonblocking so the acceptor can poll a stop flag or a
//! deadline. Darwin hands the accepted socket the listener's O_NONBLOCK; Linux does not. A
//! reader that assumed blocking I/O then sees `WouldBlock` whenever a parallel suite accepts
//! before the request line is buffered, and a writer can drop a body on a full send buffer —
//! flakes that exist on the Mac only. Every such acceptor accepts through here, so the
//! connection behaves as on Linux and read/write timeouts mean what they say.

/// `listener.accept()`, with the accepted socket put back in blocking mode.
pub fn accept(
    listener: &std::net::TcpListener,
) -> std::io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
    let (socket, peer) = listener.accept()?;
    socket.set_nonblocking(false)?;
    Ok((socket, peer))
}
