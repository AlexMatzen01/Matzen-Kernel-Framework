//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Socket layer: one addressing and lifetime model over TCP and UDP.
//!
//! Before this module there were two incompatible ways to talk to the network,
//! and neither had a name for "the thing I opened":
//!
//! - **TCP** was identified by a bare `u16` local port. `connect` returned one,
//!   and twelve free functions took it back. Nothing distinguished a stale port
//!   number from a live one, so a caller that kept using a port after closing it
//!   silently addressed whatever connection inherited the number — or nothing,
//!   which looked the same as a silent peer.
//! - **UDP** had no identity at all. The caller chose a port number at receive
//!   time, and every reader scanned one shared queue linearly for a match.
//! - `tls::TcpSocket` was the one owned-connection type in the tree, and it was
//!   compiled only under the non-default `net_tls` feature, so the socket the TLS
//!   layer used was not the socket anything else could name.
//!
//! A [`Socket`] here is a copyable handle: a table index plus a generation.
//! The generation is what makes the difference between a handle and a number —
//!
//! ```text
//! index 0, generation 7   closed
//! index 0, generation 8   a different socket
//! ```
//!
//! and the old handle `(0, 7)` is refused. Without it, handle reuse turns a
//! double-close or a use-after-close into traffic sent to the wrong peer, which is
//! the kind of bug that shows up once a day and is blamed on the network.
//!
//! The handle is a value, not a guard: this kernel has no threads and no
//! blocking scheduler to enforce scoping against, so `Drop` cannot promise that
//! closing happens. [`Socket::close`] does the work and releases the table slot,
//! and the type is deliberately `Copy` so a handle can be passed to the shell and
//! printed by the user without a borrow checker in the way.

use alloc::vec::Vec;
use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;

pub use crate::net::pump;

/// An IPv4 address and port.
///
/// Ten-odd call sites previously passed `([u8; 4], u16)` around separately, and
/// it was a recurring slip to swap them or to pass the port where the address
/// went. One type makes that impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketAddr {
    pub ip: [u8; 4],
    pub port: u16,
}

impl SocketAddr {
    pub const fn new(ip: [u8; 4], port: u16) -> Self {
        Self { ip, port }
    }

    /// Whether this address can never be a valid host endpoint.
    ///
    /// `0.0.0.0` is "unspecified" and only meaningful as a send source, so
    /// connecting *to* it has no meaning. Treating it as a routable peer is how
    /// a configuration mistake turns into a hang instead of an error.
    pub fn is_unspecified(&self) -> bool {
        self.ip == [0, 0, 0, 0]
    }

    /// Whether this is a multicast address, for which no connection semantics
    /// exist.
    pub fn is_multicast(&self) -> bool {
        self.ip[0] & 0xF0 == 0xE0
    }
}

impl fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}:{}",
            self.ip[0], self.ip[1], self.ip[2], self.ip[3], self.port
        )
    }
}

/// Parse `a.b.c.d:port`, or a bare `a.b.c.d` with `default_port`.
///
/// Rejects anything with the wrong number of octets, an out-of-range octet, or a
/// port above 65535, rather than silently substituting zero — a typo in a host
/// string must not become a connection to `0.0.0.0`.
pub fn parse_addr(value: &str, default_port: u16) -> Option<SocketAddr> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    };
    let ip = crate::net::http::parse_ipv4(host)?;
    // A colon was typed, so a port was meant. An empty or unparsable one is a
    // mistake to report, not an invitation to fall back to the default — that
    // turns "10.0.2.2:" into a connection to port 80 that the user never asked
    // for.
    let port = match port {
        Some(text) => text.parse::<u16>().ok()?,
        None => default_port,
    };
    Some(SocketAddr::new(ip, port))
}

/// Transport a socket speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Udp => "udp",
        }
    }
}

/// What a socket currently is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    /// Connected and able to exchange data.
    Connected,
    /// The peer has closed its sending side. Data already queued can still be
    /// read; writing is not possible.
    PeerClosed,
    /// The connection is still being established.
    Connecting,
    /// Closed, reset, or refused.
    Closed,
}

impl SocketState {
    pub fn name(self) -> &'static str {
        match self {
            SocketState::Connected => "connected",
            SocketState::PeerClosed => "peer-closed",
            SocketState::Connecting => "connecting",
            SocketState::Closed => "closed",
        }
    }

    /// Whether reading or writing can still make progress.
    pub fn is_usable(self) -> bool {
        matches!(self, SocketState::Connected | SocketState::PeerClosed)
    }
}

/// Why an operation failed.
///
/// The transport-specific `&'static str` errors are mapped onto this rather than
/// forwarded, so a caller can tell "the peer went away" from "you passed
/// nonsense" without string matching on messages that are not a stable interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketError {
    /// The handle does not name a live socket — closed, or never valid.
    BadHandle,
    /// The socket is closed or reset, so the operation cannot proceed.
    NotConnected,
    /// The transport refused the address or the request.
    Refused,
    /// The operation exceeded its deadline.
    TimedOut,
    /// The user interrupted it.
    Cancelled,
    /// The protocol or the arguments were malformed.
    Invalid,
    /// The network could not carry it: no route, no ARP reply, transmit failure.
    Network,
}

impl SocketError {
    pub fn message(self) -> &'static str {
        match self {
            SocketError::BadHandle => "no such socket",
            SocketError::NotConnected => "socket is not connected",
            SocketError::Refused => "connection refused",
            SocketError::TimedOut => "operation timed out",
            SocketError::Cancelled => "cancelled",
            SocketError::Invalid => "invalid request",
            SocketError::Network => "network unreachable",
        }
    }
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// Map a transport error string onto a kind.
///
/// Anything unrecognised becomes [`SocketError::Network`], which is the honest
/// default: the operation did not succeed and the reason was not one this layer
/// knows.
fn classify(error: &'static str) -> SocketError {
    if error.contains("timed out") || error.contains("timeout") {
        SocketError::TimedOut
    } else if error.contains("refused") || error.contains("not found") {
        SocketError::BadHandle
    } else if error.contains("No route") || error.contains("Packet too large") || error.contains("not initialized")
    {
        SocketError::Network
    } else {
        SocketError::Network
    }
}

/// One open socket.
///
/// `Copy` on purpose: see the module comment. It holds no buffers, so copying one
/// cannot duplicate the data behind it — the index and generation are all that
/// travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Socket {
    index: u16,
    generation: u32,
}

impl Socket {
    /// A handle that never names a socket.
    pub const INVALID: Socket = Socket {
        index: u16::MAX,
        generation: 0,
    };

    /// Whether this handle could ever have been valid.
    ///
    /// `INVALID` is the zero value of nothing; a caller that constructs one by
    /// hand gets something every operation refuses.
    pub fn is_valid(&self) -> bool {
        *self != Socket::INVALID && (self.index as usize) < MAX_SOCKETS
    }

    /// The table slot, for diagnostics.
    pub fn index(&self) -> usize {
        self.index as usize
    }

    /// Parse a handle as `Display` prints it.
    ///
    /// Accepts `socket#3g9`, and also a bare `3` for a handle whose generation
    /// is the current one for that slot — which is what makes the shell usable:
    /// the user can type the number they were shown without the shell having to
    /// track generations.
    ///
    /// Returns `None` for anything that does not name a *live* socket, including
    /// a well-formed handle that has since been closed. Checking here rather
    /// than leaving it to the first operation is what lets the shell say "no such
    /// socket" instead of accepting the argument and failing one step later.
    pub fn parse(text: &str) -> Option<Socket> {
        let text = text.trim();
        let body = text.strip_prefix("socket#").unwrap_or(text);
        let handle = match body.split_once('g') {
            Some((index, generation)) => Socket {
                index: index.parse::<u16>().ok()?,
                generation: generation.parse::<u32>().ok()?,
            },
            // A bare index means "whichever socket currently occupies it".
            None => {
                let index = body.parse::<u16>().ok()?;
                let table = SOCKETS.lock();
                Socket {
                    index,
                    generation: table.get(index as usize)?.generation,
                }
            }
        };
        resolve(handle).map(|_| handle)
    }
}

impl fmt::Display for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Socket::INVALID {
            f.write_str("invalid")
        } else {
            write!(f, "socket#{}g{}", self.index, self.generation)
        }
    }
}

/// Maximum simultaneously open sockets.
///
/// Bounded because each TCP socket owns receive and send buffers of up to 1 MiB
/// and 256 KiB. An unbounded table would let a caller exhaust the heap by
/// opening sockets, which is exactly the failure the buffer caps prevent one
/// connection at a time from causing.
pub const MAX_SOCKETS: usize = 32;

struct Entry {
    generation: u32,
    transport: Transport,
    /// Local port. For TCP this is the connection key; for UDP it is the port the
    /// caller chose at bind time.
    local_port: u16,
    peer: SocketAddr,
    state: SocketState,
}

lazy_static! {
    static ref SOCKETS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
}

/// Insert an entry, returning its handle.
///
/// The generation is bumped on every insert into the same slot, which is what
/// makes a stale handle detectable. When the table is full the oldest *closed*
/// entry is recycled; if every entry is live, the caller gets an error rather than
/// one being taken away from underneath it.
fn insert(transport: Transport, local_port: u16, peer: SocketAddr, state: SocketState) -> Option<Socket> {
    let mut table = SOCKETS.lock();

    for index in 0..table.len() {
        if table[index].state == SocketState::Closed {
            let generation = table[index].generation.wrapping_add(1);
            // A generation that wrapped back to a value still held elsewhere
            // would alias. Practically unreachable in one boot, but skipping to
            // the next free value costs nothing.
            let generation = if table.iter().any(|e| e.generation == generation) {
                table.iter().map(|e| e.generation).max().unwrap_or(0).wrapping_add(1)
            } else {
                generation
            };
            table[index] = Entry {
                generation,
                transport,
                local_port,
                peer,
                state,
            };
            return Some(Socket {
                index: index as u16,
                generation,
            });
        }
    }

    if table.len() >= MAX_SOCKETS {
        return None;
    }
    let generation = table
        .iter()
        .map(|e| e.generation)
        .max()
        .map(|g| g.wrapping_add(1))
        .unwrap_or(1);
    table.push(Entry {
        generation,
        transport,
        local_port,
        peer,
        state,
    });
    Some(Socket {
        index: (table.len() - 1) as u16,
        generation,
    })
}

/// Look up the entry behind `handle`, if it is still live.
///
/// The generation comparison is the entire point: a handle to a socket that has
/// been closed, and whose slot has since been reused, must not resolve. So must
/// an entry that has been closed but not yet reused — otherwise a use-after-close
/// would keep addressing a transport that no longer exists.
fn resolve(handle: Socket) -> Option<usize> {
    if !handle.is_valid() {
        return None;
    }
    let table = SOCKETS.lock();
    let entry = table.get(handle.index as usize)?;
    if entry.generation != handle.generation || entry.state == SocketState::Closed {
        return None;
    }
    Some(handle.index as usize)
}

/// Open a TCP connection and return a handle to it.
pub fn connect_tcp(peer: SocketAddr, timeout_ms: u64) -> Result<Socket, SocketError> {
    if peer.is_unspecified() {
        return Err(SocketError::Invalid);
    }
    if peer.is_multicast() {
        // A multicast destination has no connection semantics at all: nothing
        // acknowledges, nothing closes. Accepting it here would produce a socket
        // that never reports failure.
        return Err(SocketError::Invalid);
    }

    let local_port =
        crate::net::tcp::connect(peer.ip, peer.port).map_err(classify)?;

    let handle = match insert(
        Transport::Tcp,
        local_port,
        peer,
        SocketState::Connecting,
    ) {
        Some(handle) => handle,
        None => {
            // Do not leave a live TCP connection with no handle: it would be
            // reachable by anyone guessing the port and impossible to close
            // through this layer.
            let _ = crate::net::tcp::close(local_port);
            crate::net::tcp::forget(local_port);
            return Err(SocketError::Refused);
        }
    };

    // Wait for the handshake, so a caller that gets a handle back has something
    // it can write to.
    let start = crate::shell::monotonic_ms();
    loop {
        pump();
        match crate::net::tcp::get_state(local_port) {
            Some(crate::net::tcp::TcpState::Established) => {
                set_state(handle, SocketState::Connected);
                return Ok(handle);
            }
            Some(crate::net::tcp::TcpState::CloseWait | crate::net::tcp::TcpState::Closed) => {
                close(handle);
                return Err(SocketError::NotConnected);
            }
            _ => {}
        }
        if crate::shell::is_interrupted() {
            close(handle);
            return Err(SocketError::Cancelled);
        }
        if crate::shell::monotonic_ms().saturating_sub(start) >= timeout_ms {
            close(handle);
            return Err(SocketError::TimedOut);
        }
    }
}

/// "Bind" a local port for UDP.
///
/// UDP has no handshake, so there is nothing to wait for. The entry starts
/// [`SocketState::Connected`] because a bound socket is immediately usable —
/// unlike a TCP socket, which is not usable until the handshake completes.
pub fn bind_udp(local_port: u16, peer: SocketAddr) -> Result<Socket, SocketError> {
    insert(Transport::Udp, local_port, peer, SocketState::Connected)
        .ok_or(SocketError::Refused)
}

fn set_state(handle: Socket, state: SocketState) {
    if let Some(index) = resolve(handle) {
        SOCKETS.lock()[index].state = state;
    }
}

/// Current state of a socket.
pub fn state(handle: Socket) -> Result<SocketState, SocketError> {
    let index = resolve(handle).ok_or(SocketError::BadHandle)?;
    let (transport, local_port, cached) = {
        let table = SOCKETS.lock();
        let entry = &table[index];
        (entry.transport, entry.local_port, entry.state)
    };
    match (transport, cached) {
        (_, SocketState::Closed) => return Err(SocketError::BadHandle),
        (Transport::Udp, _) => return Ok(cached),
        (Transport::Tcp, SocketState::Connecting) => return Ok(SocketState::Connecting),
        (Transport::Tcp, _) => {}
    }

    // Ask the transport too: the cached state can lag a peer that closed since
    // the last pump, and a stale "connected" is how a caller ends up writing into
    // a socket nobody is reading.
    let state = match crate::net::tcp::get_state(local_port) {
        Some(crate::net::tcp::TcpState::Established) => SocketState::Connected,
        // The peer stopped sending. Data already queued is still readable, so
        // this stays a usable socket rather than an error.
        Some(crate::net::tcp::TcpState::CloseWait) => SocketState::PeerClosed,
        Some(crate::net::tcp::TcpState::SynSent) => SocketState::Connecting,
        // Anything else — Closed, TimeWait, a half-closed teardown — is not a
        // socket this layer can still use.
        _ => {
            SOCKETS.lock()[index].state = SocketState::Closed;
            return Err(SocketError::NotConnected);
        }
    };
    SOCKETS.lock()[index].state = state;
    Ok(state)
}

/// The peer a socket talks to.
pub fn peer_addr(handle: Socket) -> Result<SocketAddr, SocketError> {
    let index = resolve(handle).ok_or(SocketError::BadHandle)?;
    let table = SOCKETS.lock();
    Ok(table[index].peer)
}

/// The local port a socket uses.
pub fn local_port(handle: Socket) -> Result<u16, SocketError> {
    let index = resolve(handle).ok_or(SocketError::BadHandle)?;
    let table = SOCKETS.lock();
    Ok(table[index].local_port)
}

/// Send `data`, returning how many bytes were accepted.
///
/// May be short: for TCP the peer's window bounds what may go out at once, and a
/// caller that needs all of it must loop. Reporting the count is what makes that
/// loop writable without guessing.
pub fn write(handle: Socket, data: &[u8]) -> Result<usize, SocketError> {
    let index = resolve(handle).ok_or(SocketError::BadHandle)?;
    let (transport, local_port, peer) = {
        let table = SOCKETS.lock();
        let entry = &table[index];
        (entry.transport, entry.local_port, entry.peer)
    };

    match transport {
        Transport::Tcp => {
            if state(handle)? == SocketState::PeerClosed {
                return Err(SocketError::NotConnected);
            }
            crate::net::tcp::send_data(local_port, data).map_err(classify)?;
            Ok(data.len())
        }
        Transport::Udp => {
            crate::net::udp::send_packet(peer.ip, local_port, peer.port, data).map_err(classify)?;
            Ok(data.len())
        }
    }
}

/// Write all of `data`, blocking until it has all been accepted or the deadline
/// passes.
///
/// Provided because "send this request" is almost always the intent, and the
/// short-write loop is otherwise duplicated at every call site.
pub fn write_all(handle: Socket, data: &[u8], timeout_ms: u64) -> Result<(), SocketError> {
    let start = crate::shell::monotonic_ms();
    let mut offset = 0usize;
    while offset < data.len() {
        if crate::shell::is_interrupted() {
            return Err(SocketError::Cancelled);
        }
        let written = write(handle, &data[offset..])?;
        offset += written;
        if offset < data.len() {
            if crate::shell::monotonic_ms().saturating_sub(start) >= timeout_ms {
                return Err(SocketError::TimedOut);
            }
            pump();
        }
    }
    Ok(())
}

/// Read up to `buf.len()` bytes.
///
/// Returns `Ok(0)` only at end of stream: for UDP a datagram boundary is
/// preserved, so a short read is a whole datagram; for TCP the stream is flat
/// and a short read is just what had arrived.
pub fn read(handle: Socket, buf: &mut [u8]) -> Result<usize, SocketError> {
    let index = resolve(handle).ok_or(SocketError::BadHandle)?;
    let (transport, local_port, peer) = {
        let table = SOCKETS.lock();
        let entry = &table[index];
        (entry.transport, entry.local_port, entry.peer)
    };

    match transport {
        Transport::Tcp => {
            if state(handle).is_err() {
                return Err(SocketError::NotConnected);
            }
            // `tcp::read_data` drains the whole receive buffer, so copy out only
            // what the caller asked for.
            match crate::net::tcp::read_data(local_port) {
                Some(chunk) => {
                    let count = core::cmp::min(buf.len(), chunk.len());
                    buf[..count].copy_from_slice(&chunk[..count]);
                    if count < chunk.len() {
                        // The tail has nowhere to go back to — there is no pushback
                        // on this API — so it is dropped, and the count says how
                        // much was actually delivered rather than reporting a
                        // length the caller did not get.
                        crate::net_log!(
                            "socket: dropping {} unread byte(s) from a TCP read",
                            chunk.len() - count
                        );
                    }
                    Ok(count)
                }
                // Nothing buffered. `read_timeout` distinguishes end of stream
                // from "not yet"; a bare `read` reports 0 for both, and callers
                // that care use `read_timeout`.
                None => match state(handle) {
                    Ok(_) => Ok(0),
                    Err(e) => Err(e),
                },
            }
        }
        Transport::Udp => {
            // A socket bound to a peer only accepts datagrams from that peer.
            // `udp::receive` filters on the local port alone, which would let an
            // unrelated host inject a "reply" into a socket the caller believes
            // is talking to one specific server.
            if peer.is_unspecified() {
                match crate::net::udp::receive(local_port) {
                    Some(datagram) => {
                        let count = core::cmp::min(buf.len(), datagram.payload.len());
                        buf[..count].copy_from_slice(&datagram.payload[..count]);
                        Ok(count)
                    }
                    None => Ok(0),
                }
            } else {
                match crate::net::udp::receive_from(local_port, peer.ip, peer.port) {
                    Some(payload) => {
                        let count = core::cmp::min(buf.len(), payload.len());
                        buf[..count].copy_from_slice(&payload[..count]);
                        Ok(count)
                    }
                    None => Ok(0),
                }
            }
        }
    }
}

/// Wait up to `timeout_ms` for data to arrive, then read it.
///
/// The polling read above cannot block — there is no scheduler to block on — so
/// a caller wanting "wait for a response" has to say so explicitly rather than
/// discovering it by getting an empty result.
pub fn read_timeout(handle: Socket, buf: &mut [u8], timeout_ms: u64) -> Result<usize, SocketError> {
    let start = crate::shell::monotonic_ms();
    loop {
        let read = read(handle, buf)?;
        if read > 0 {
            return Ok(read);
        }
        if crate::shell::is_interrupted() {
            return Err(SocketError::Cancelled);
        }
        match state(handle) {
            // End of stream, and nothing left to deliver.
            Ok(SocketState::PeerClosed) => return Ok(0),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
        if crate::shell::monotonic_ms().saturating_sub(start) >= timeout_ms {
            return Err(SocketError::TimedOut);
        }
        pump();
    }
}

/// Close a socket and release its table slot.
///
/// The slot is not removed but marked closed, so the next `insert` bumps its
/// generation and any handle still held for it stops resolving immediately.
pub fn close(handle: Socket) {
    let Some(index) = resolve(handle) else {
        // Closing something that is not open is not an error: `close` has always
        // been idempotent, and a caller cleaning up after a failed connect must
        // not have to know whether the failure got that far.
        return;
    };
    let (transport, local_port) = {
        let mut table = SOCKETS.lock();
        table[index].state = SocketState::Closed;
        (table[index].transport, table[index].local_port)
    };
    match transport {
        Transport::Tcp => {
            let _ = crate::net::tcp::close(local_port);
            // Let the FIN out rather than dropping the entry on top of it.
            for _ in 0..20 {
                pump();
            }
            crate::net::tcp::forget(local_port);
        }
        Transport::Udp => {}
    }
}

/// Abandon a socket without the graceful close, sending a reset.
///
/// For a peer that has gone silent: a FIN would wait for an acknowledgement that
/// is never coming.
pub fn abort(handle: Socket) {
    let Some(index) = resolve(handle) else {
        return;
    };
    let (transport, local_port) = {
        let mut table = SOCKETS.lock();
        table[index].state = SocketState::Closed;
        (table[index].transport, table[index].local_port)
    };
    if transport == Transport::Tcp {
        crate::net::tcp::abort(local_port);
    }
}

/// Forget a socket without touching the transport.
///
/// For a caller that has already torn down the transport itself — [`socket::close`]
/// must not be used, or it would send a FIN for a connection that is already
/// gone. The slot is still released, so the handle stops resolving.
pub fn detach(handle: Socket) {
    if let Some(index) = resolve(handle) {
        SOCKETS.lock()[index].state = SocketState::Closed;
    }
}

/// Mark sockets whose transport has finished as closed, so `netstat` does not
/// list them and a later `insert` can reuse the slot.
///
/// Only TCP is reaped. UDP entries live as long as their port does, because a
/// UDP "connection" has no handshake to end and a datagram may still arrive for
/// a port that has been idle since the last request.
///
/// Entries are *marked*, never removed. That is deliberate: the generation is
/// derived from what is in the slot, so dropping the entry would let the counter
/// restart and hand a later socket a generation an old handle still carries —
/// which would make a retired handle live again. Keeping closed entries costs
/// nothing, since `insert` prefers a closed slot and the table is bounded.
pub fn reap() {
    let mut table = SOCKETS.lock();
    for entry in table.iter_mut() {
        if entry.state == SocketState::Closed || entry.transport != Transport::Tcp {
            continue;
        }
        let alive = matches!(
            crate::net::tcp::get_state(entry.local_port),
            Some(
                crate::net::tcp::TcpState::Established
                    | crate::net::tcp::TcpState::CloseWait
                    | crate::net::tcp::TcpState::SynSent
            )
        );
        if !alive {
            entry.state = SocketState::Closed;
        }
    }
}

/// One row for `tcpsockets`.
pub struct SocketInfo {
    pub handle: Socket,
    pub transport: Transport,
    pub local_port: u16,
    pub peer: SocketAddr,
    pub state: SocketState,
}

/// Every open socket, for `netstat`.
pub fn sockets() -> Vec<SocketInfo> {
    reap();
    let table = SOCKETS.lock();
    let mut open = Vec::new();
    for (index, entry) in table.iter().enumerate() {
        if entry.state == SocketState::Closed {
            continue;
        }
        open.push(SocketInfo {
            // The real slot, not the position among live entries, so the handle
            // resolves back to this entry.
            handle: Socket {
                index: index as u16,
                generation: entry.generation,
            },
            transport: entry.transport,
            local_port: entry.local_port,
            peer: entry.peer,
            state: entry.state,
        });
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: SocketAddr = SocketAddr {
        ip: [10, 0, 2, 2],
        port: 80,
    };

    /// Run `body` with exclusive use of the socket table.
    ///
    /// The table is a process-wide singleton and the harness runs tests on
    /// several threads, so without this one test's sockets land in another's
    /// assertions.
    static LOCK: Mutex<()> = Mutex::new(());

    fn alone<R>(body: impl FnOnce() -> R) -> R {
        let _guard = LOCK.lock();
        SOCKETS.lock().clear();
        body()
    }

    // ── addresses ────────────────────────────────────────────────────

    #[test]
    fn addresses_display_as_dotted_quad_and_port() {
        assert_eq!(
            SocketAddr::new([10, 0, 2, 15], 8080).to_string(),
            "10.0.2.15:8080"
        );
        assert_eq!(
            SocketAddr::new([255, 255, 255, 255], 0).to_string(),
            "255.255.255.255:0"
        );
    }

    #[test]
    fn addresses_parse_with_and_without_a_port() {
        assert_eq!(parse_addr("10.0.2.2:8080", 80), Some(SocketAddr::new([10, 0, 2, 2], 8080)));
        // A bare address takes the default, which is what the shell wants.
        assert_eq!(parse_addr("10.0.2.2", 80), Some(SocketAddr::new([10, 0, 2, 2], 80)));
        // Surrounding whitespace is common when parsing shell arguments.
        assert_eq!(parse_addr("  10.0.2.2:81  ", 80), Some(SocketAddr::new([10, 0, 2, 2], 81)));
    }

    #[test]
    fn malformed_addresses_are_refused_rather_than_zeroed() {
        // Substituting zero for an unparsable octet would turn a typo into a
        // connection to 0.0.0.0, which hangs instead of erroring.
        for bad in [
            "",
            "10.0.2",
            "10.0.2.256",
            "10.0.2.2.5",
            "10.0.2.-1",
            "not.an.ip",
            "10.0.2.2:99999",
            "10.0.2.2:",
        ] {
            assert!(parse_addr(bad, 80).is_none(), "{:?} was accepted", bad);
        }
    }

    #[test]
    fn unusable_destinations_are_recognised() {
        assert!(SocketAddr::new([0, 0, 0, 0], 80).is_unspecified());
        assert!(!SocketAddr::new([10, 0, 2, 2], 80).is_unspecified());
        // 224.0.0.0/4 is the multicast range, so 223.x and 240.x bracket it.
        assert!(SocketAddr::new([224, 0, 0, 1], 80).is_multicast());
        assert!(SocketAddr::new([239, 255, 255, 255], 0).is_multicast());
        assert!(!SocketAddr::new([223, 255, 255, 255], 80).is_multicast());
        assert!(!SocketAddr::new([240, 0, 0, 1], 80).is_multicast());
    }

    #[test]
    fn error_messages_are_distinct_per_kind() {
        // A caller must be able to tell "the peer went away" from "bad handle"
        // without matching on prose, so each kind has to say something different.
        let kinds = [
            SocketError::BadHandle,
            SocketError::NotConnected,
            SocketError::Refused,
            SocketError::TimedOut,
            SocketError::Cancelled,
            SocketError::Invalid,
            SocketError::Network,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a.message(), b.message(), "{:?} and {:?} are conflated", a, b);
            }
        }
    }

    #[test]
    fn transport_errors_map_to_kinds() {
        assert_eq!(classify("Connection timed out"), SocketError::TimedOut);
        assert_eq!(classify("DNS query timed out"), SocketError::TimedOut);
        assert_eq!(classify("Connection not found"), SocketError::BadHandle);
        // An unrecognised reason is honestly reported as a network failure
        // rather than guessed at.
        assert_eq!(classify("something else entirely"), SocketError::Network);
    }

    // ── handle lifetime ──────────────────────────────────────────────

    #[test]
    fn an_inserted_socket_resolves() {
        alone(|| {
            let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            assert!(handle.is_valid());
            assert_eq!(resolve(handle), Some(0));
            assert_eq!(peer_addr(handle).unwrap(), PEER);
            assert_eq!(local_port(handle).unwrap(), 49152);
        })
    }

    #[test]
    fn the_invalid_handle_names_nothing() {
        alone(|| {
            assert!(!Socket::INVALID.is_valid());
            assert_eq!(resolve(Socket::INVALID), None);
            assert_eq!(peer_addr(Socket::INVALID), Err(SocketError::BadHandle));
            assert_eq!(read(Socket::INVALID, &mut [0u8; 4]), Err(SocketError::BadHandle));
            assert_eq!(
                write(Socket::INVALID, b"x"),
                Err(SocketError::BadHandle)
            );
            // Closing an unopened socket is not an error, and stays that way on a
            // second call — `close` has to be idempotent for cleanup after a
            // failed connect to work.
            close(Socket::INVALID);
            close(Socket::INVALID);
            assert_eq!(resolve(Socket::INVALID), None);
        })
    }

    #[test]
    fn a_handle_for_the_wrong_generation_does_not_resolve() {
        // The whole reason a handle is a pair rather than an index.
        alone(|| {
            let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            let stale = Socket {
                index: handle.index,
                generation: handle.generation.wrapping_add(1),
            };
            assert!(handle.is_valid(), "the live handle is valid");
            assert!(!stale.is_valid() || resolve(stale).is_none());
            assert_eq!(resolve(stale), None, "a stale handle must not resolve");
            assert_eq!(write(stale, b"data"), Err(SocketError::BadHandle));
        })
    }

    #[test]
    fn a_closed_socket_stops_resolving() {
        alone(|| {
            let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            assert!(resolve(handle).is_some());
            // Marking it closed is what `close` does; the transport call needs a
            // NIC, so only the table transition is exercised here.
            SOCKETS.lock()[0].state = SocketState::Closed;
            assert_eq!(resolve(handle), None);
            assert_eq!(state(handle), Err(SocketError::BadHandle));
            assert_eq!(peer_addr(handle), Err(SocketError::BadHandle));
        })
    }

    #[test]
    fn a_reused_slot_gets_a_new_generation() {
        // Handle reuse is where a missing generation turns a use-after-close
        // into traffic sent to the wrong peer.
        alone(|| {
            let first = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            SOCKETS.lock()[0].state = SocketState::Closed;

            let second = insert(Transport::Tcp, 49153, PEER, SocketState::Connected).unwrap();
            assert_eq!(first.index, second.index, "the slot should be reused");
            assert_ne!(
                first.generation, second.generation,
                "a reused slot must not repeat its generation"
            );
            assert_eq!(resolve(first), None, "the old handle must be dead");
            assert!(resolve(second).is_some(), "the new handle must be live");
            assert_eq!(local_port(second).unwrap(), 49153);
        })
    }

    #[test]
    fn the_table_is_bounded() {
        // Each TCP socket owns buffers of up to 1 MiB, so the table cannot grow
        // without limit.
        alone(|| {
            let mut handles = Vec::new();
            for i in 0..MAX_SOCKETS {
                handles.push(
                    insert(Transport::Tcp, 40000 + i as u16, PEER, SocketState::Connected)
                        .expect("a live socket slot"),
                );
            }
            assert_eq!(
                insert(Transport::Tcp, 49999, PEER, SocketState::Connected),
                None,
                "a full table must refuse rather than exceed its bound"
            );
            // Freeing one makes room, and the newcomer must not be handed a stale
            // handle's identity.
            SOCKETS.lock()[3].state = SocketState::Closed;
            let fresh = insert(Transport::Tcp, 49999, PEER, SocketState::Connected).unwrap();
            assert_eq!(fresh.index, 3);
            assert_eq!(resolve(handles[3]), None);
        })
    }

    #[test]
    fn a_closed_slot_is_reused_before_a_new_one_is_taken() {
        alone(|| {
            let mut handles = Vec::new();
            for i in 0..MAX_SOCKETS {
                handles.push(
                    insert(Transport::Tcp, 40000 + i as u16, PEER, SocketState::Connected)
                        .unwrap(),
                );
            }
            SOCKETS.lock()[0].state = SocketState::Closed;
            let fresh = insert(Transport::Tcp, 45000, PEER, SocketState::Connected).unwrap();
            assert_eq!(fresh.index, 0, "a free slot should be preferred");
            assert_eq!(SOCKETS.lock().len(), MAX_SOCKETS);
        })
    }

    #[test]
    fn many_open_and_close_cycles_keep_handles_distinct() {
        // Generations must not repeat while handles are still outstanding, or a
        // long-lived handle from an earlier cycle starts resolving again.
        alone(|| {
            let mut outstanding = Vec::new();
            for cycle in 0..64u32 {
                let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
                assert!(
                    !outstanding.contains(&handle),
                    "handle {:?} was handed out twice",
                    handle
                );
                outstanding.push(handle);
                if outstanding.len() == 4 {
                    let index = outstanding[0].index as usize;
                    SOCKETS.lock()[index].state = SocketState::Closed;
                    outstanding.remove(0);
                }
            }
        })
    }

    #[test]
    fn sockets_lists_only_live_entries() {
        alone(|| {
            // UDP entries only: `sockets` reaps first, and a TCP entry with no
            // live transport connection would be dropped before it could be
            // listed — which is the subject of the next test.
            insert(Transport::Udp, 5555, PEER, SocketState::Connected).unwrap();
            insert(Transport::Udp, 5556, PEER, SocketState::Connected).unwrap();
            let listed = sockets();
            assert_eq!(listed.len(), 2);
            assert_eq!(listed[0].local_port, 5555);
            assert_eq!(listed[0].handle.index(), 0, "handles must be the real slots");
            assert_eq!(listed[1].local_port, 5556);
            assert!(listed.iter().all(|s| s.peer == PEER));
            SOCKETS.lock()[1].state = SocketState::Closed;
            assert_eq!(sockets().len(), 1);
        })
    }

    #[test]
    fn reap_marks_a_socket_whose_transport_is_gone() {
        // A TCP connection can finish on its own — the peer closes, or a reset
        // arrives — without going through `close`. The handle would otherwise
        // stay listed and usable-looking for the rest of the session.
        alone(|| {
            let tcp = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            insert(Transport::Udp, 5555, PEER, SocketState::Connected).unwrap();
            // No TCP connection exists for port 49152 in the transport table.
            reap();
            assert_eq!(resolve(tcp), None, "the orphaned TCP socket is retired");
            assert_eq!(
                SOCKETS.lock()[1].state,
                SocketState::Connected,
                "UDP is not reaped"
            );
            assert_eq!(sockets().len(), 1);
        })
    }

    #[test]
    fn reap_keeps_udp_because_a_datagram_may_still_arrive() {
        alone(|| {
            insert(Transport::Udp, 5555, PEER, SocketState::Connected).unwrap();
            reap();
            reap();
            assert_eq!(
                SOCKETS.lock().len(),
                1,
                "a UDP socket outlives any single request"
            );
        })
    }

    #[test]
    fn reap_never_resurrects_a_retired_generation() {
        // Reaping used to *remove* the entry. Since a generation is derived from
        // what is in the slot, that restarted the counter: a slot reaped and
        // refilled came back as `socket#0g1` again, which made the first, already
        // closed handle live a second time and pointed it at a new connection.
        alone(|| {
            let first = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            assert_eq!(first.generation, 1);

            // No TCP connection exists, so the first reap marks it closed.
            reap();
            assert_eq!(resolve(first), None, "the first handle is retired");

            // Refill the slot. The newcomer must not inherit the retired
            // generation, and repeated reaping must not make the old text
            // parseable again.
            let second = insert(Transport::Tcp, 49153, PEER, SocketState::Connected).unwrap();
            assert_eq!(second.index, first.index, "the slot is reused");
            assert_ne!(
                second.generation, first.generation,
                "a reaped slot must not restart its generation"
            );
            assert_eq!(resolve(first), None);
            assert!(resolve(second).is_some());
            assert_eq!(Socket::parse(&first.to_string()), None);

            reap();
            reap();
            assert_eq!(
                Socket::parse(&first.to_string()),
                None,
                "reaping again must not revive the old handle"
            );
            assert_ne!(
                Socket::parse("0").map(|h| h.generation),
                Some(first.generation),
                "a bare index must not name the retired generation either"
            );
        })
    }

    #[test]
    fn a_reaped_socket_disappears_from_the_listing() {
        alone(|| {
            insert(Transport::Udp, 5555, PEER, SocketState::Connected).unwrap();
            insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            reap();
            let listed = sockets();
            assert_eq!(listed.len(), 1, "only the UDP socket is still open");
            assert_eq!(listed[0].transport, Transport::Udp);
        })
    }

    #[test]
    fn state_names_and_usable_agree() {
        assert!(SocketState::Connected.is_usable());
        // A peer that closed its sending side can still be read from, so it stays
        // usable for reading even though writing is not.
        assert!(SocketState::PeerClosed.is_usable());
        assert!(!SocketState::Closed.is_usable());
        assert!(!SocketState::Connecting.is_usable());
        for state in [
            SocketState::Connected,
            SocketState::PeerClosed,
            SocketState::Connecting,
            SocketState::Closed,
        ] {
            let name = state.name();
            assert!(!name.is_empty() && name.chars().all(|c| !c.is_uppercase()));
        }
    }

    #[test]
    fn invalid_handles_display_readably() {
        assert_eq!(Socket::INVALID.to_string(), "invalid");
        alone(|| {
            let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            assert!(handle.to_string().starts_with("socket#"));
        })
    }

    #[test]
    fn handles_round_trip_through_text() {
        alone(|| {
            let handle = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            assert_eq!(Socket::parse(&handle.to_string()), Some(handle));
            // The bare-index form the shell accepts resolves to the live handle.
            assert_eq!(Socket::parse("0"), Some(handle));
            assert_eq!(Socket::parse("  0  "), Some(handle));
            assert_eq!(Socket::parse("socket#0"), Some(handle));
        })
    }

    #[test]
    fn a_bare_index_follows_the_slot_not_the_handle() {
        // `Socket::parse("0")` deliberately means "whatever is in slot 0 now", so
        // after a close it must name the newcomer rather than revive the old
        // handle — the user typing the same number twice must not silently get
        // the previous connection back.
        alone(|| {
            let first = insert(Transport::Tcp, 49152, PEER, SocketState::Connected).unwrap();
            SOCKETS.lock()[0].state = SocketState::Closed;
            let second = insert(Transport::Tcp, 49153, PEER, SocketState::Connected).unwrap();

            assert_eq!(Socket::parse("0"), Some(second));
            // The full handle form is the one that refuses: the old handle is
            // dead even though the slot is occupied.
            assert_eq!(Socket::parse(&first.to_string()), None);
        })
    }

    #[test]
    fn unparsable_handles_are_refused() {
        alone(|| {
            for bad in ["", "  ", "x", "-1", "99999", "socket#0gz", "0g0g1"] {
                assert!(Socket::parse(bad).is_none(), "{:?} was accepted", bad);
            }
            // A slot that was never allocated has nothing to name.
            assert!(Socket::parse("7").is_none());
        })
    }
}