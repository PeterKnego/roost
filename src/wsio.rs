//! A socket whose writes can be switched off, so a connection can guarantee
//! it has exactly one writer.
//!
//! Both the workspace and terminal sockets run a reader thread and a writer
//! thread over one connection. tungstenite answers an inbound `Ping` or
//! `Close` by queueing a reply and flushing it from *whichever* `WebSocket`
//! read the frame (`protocol/mod.rs`: `set_additional` at the `OpCtl::Ping`
//! and `do_close` arms, flushed at the top of the next `read`). That is the
//! reader's object — so a reply can land on the wire while the writer thread
//! is part-way through a frame, splicing the two together. A `Pong` queues
//! nothing, which is why resh, as the pinger, does not trip this constantly.
//!
//! The fix is to leave the reader unable to write at all and let the writer
//! send the reply the reader owed. It cannot simply be built write-blind:
//! the handshake response and the early refusals go out through that same
//! object, before a writer thread exists. So the gate starts open and is
//! closed at the moment a second writer appears — from then on there is one
//! writer, structurally, rather than by convention.
//!
//! Discarding rather than erroring is deliberate: a write error would make
//! tungstenite retry the reply forever (`set_additional` restores it on
//! `WouldBlock`), and there is nothing to report — the reply is not lost,
//! it is re-sent by the writer.
use std::io::{Read, Result, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared with the thread that closes the gate. `Relaxed` is enough: the
/// flip is published by the `try_clone` and thread spawn that follow it,
/// and a reply written a moment either side of the flip is still correct —
/// before it, there is no second writer to splice with.
#[derive(Clone)]
pub struct Gate(Arc<AtomicBool>);

impl Gate {
    pub fn open() -> Gate {
        Gate(Arc::new(AtomicBool::new(true)))
    }

    /// No further writes reach the socket through this gate's stream.
    pub fn close(&self) {
        self.0.store(false, Ordering::Relaxed);
    }

    pub fn is_open(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A `TcpStream` that stops writing when its gate closes. Reads are never
/// affected — the reader goes on reading for the life of the connection.
pub struct GatedStream {
    inner: TcpStream,
    gate: Gate,
}

impl GatedStream {
    pub fn new(inner: TcpStream, gate: Gate) -> GatedStream {
        GatedStream { inner, gate }
    }

    /// A second descriptor for the same connection, for the writer thread.
    /// Taken before the gate closes; the clone is a plain `TcpStream` and is
    /// never gated.
    pub fn try_clone_inner(&self) -> Result<TcpStream> {
        self.inner.try_clone()
    }

    pub fn get_ref(&self) -> &TcpStream {
        &self.inner
    }
}

impl Read for GatedStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (&self.inner).read(buf)
    }
}

impl Write for GatedStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if self.gate.is_open() {
            (&self.inner).write(buf)
        } else {
            // Reported as written, and dropped. See the module doc: the
            // caller is tungstenite flushing a reply the writer thread is
            // about to send properly.
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.gate.is_open() {
            (&self.inner).flush()
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A connected pair, so the assertions are about real socket bytes
    /// rather than a mock that could agree with a broken implementation.
    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = l.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = l.accept().unwrap();
        (server, client)
    }

    #[test]
    fn an_open_gate_writes_through_to_the_socket() {
        let (server, mut client) = pair();
        let gate = Gate::open();
        let mut s = GatedStream::new(server, gate);
        s.write_all(b"hello").unwrap();
        s.flush().unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn a_closed_gate_writes_nothing_to_the_socket() {
        // The whole point of the type. Reverting `write` to always delegate
        // makes this the only failing test: the client's read then returns
        // 5 bytes instead of timing out.
        let (server, mut client) = pair();
        let gate = Gate::open();
        let mut s = GatedStream::new(server, gate.clone());
        gate.close();
        s.write_all(b"hello").unwrap();
        s.flush().unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_millis(250))).unwrap();
        let mut buf = [0u8; 5];
        let n = client.read(&mut buf);
        assert!(
            n.is_err(),
            "a closed gate must put nothing on the wire, but the peer read {n:?}"
        );
    }

    #[test]
    fn a_closed_gate_still_reads() {
        // A reader that stopped reading when it stopped writing would hang
        // the connection rather than fix it.
        let (server, mut client) = pair();
        let gate = Gate::open();
        let mut s = GatedStream::new(server, gate.clone());
        gate.close();
        client.write_all(b"inbound").unwrap();
        let mut buf = [0u8; 7];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"inbound");
    }

    #[test]
    fn the_writers_clone_is_not_gated() {
        // The writer thread's descriptor is taken from the same connection
        // but must keep working after the gate closes — otherwise closing
        // the gate silences the socket entirely.
        let (server, mut client) = pair();
        let gate = Gate::open();
        let s = GatedStream::new(server, gate.clone());
        let mut w = s.try_clone_inner().unwrap();
        gate.close();
        w.write_all(b"from the writer").unwrap();
        let mut buf = [0u8; 15];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"from the writer");
    }
}
