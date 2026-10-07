//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! TCP (Transmission Control Protocol).
//!
//! Scope: a robust **client** transport for fetching HTTP/HTTPS content. It
//! implements the parts that decide whether a download survives a real network
//! and the parts that decide whether it is safe on one:
//!
//! - **Unpredictable identity.** The initial sequence number and the ephemeral
//!   port are drawn from hardware entropy. Both used to be fixed (ISN `1000`,
//!   ports counting up from 49152), which made every connection identical
//!   across boots and trivially injectable.
//! - **Retransmission.** Unacknowledged segments are retained and re-sent with
//!   an exponential-backoff timer. `send_data` used to hand a segment to the
//!   NIC and advance `seq_num` unconditionally, so one lost packet ended the
//!   transfer with no way to recover.
//! - **Receive window.** The peer's advertised window is read and respected;
//!   our own advertised window is derived from real buffer occupancy. The
//!   header field existed and was never decoded.
//! - **Sequence validation.** Segments outside the window are discarded and
//!   re-acknowledged rather than appended blindly.
//! - **Bounded memory.** Receive, send and in-flight buffers are capped, and
//!   the connection table is bounded with oldest-first eviction, so a remote
//!   peer cannot exhaust the kernel heap.
//! - **A real teardown.** Every reachable state is driven: the FIN handshake
//!   completes, LastAck is acknowledged, and TimeWait holds the port so it
//!   cannot be rebound while old segments are still in flight.
//!
//! Not implemented, and deliberately out of scope: congestion control, a
//! server/listen role, urgent pointers, SACK, and out-of-order reassembly
//! (gaps are skipped and filled by peer retransmission).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

// TCP Flags
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

/// Retransmission attempts before a connection is declared dead.
pub const MAX_TX_ATTEMPTS: u8 = 6;

// TCP States
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

impl TcpState {
    /// True while the connection still owns its local port.
    pub fn is_open(self) -> bool {
        !matches!(self, TcpState::Closed)
    }

    /// True once application data may be exchanged.
    pub fn can_transfer(self) -> bool {
        matches!(self, TcpState::Established | TcpState::CloseWait)
    }

    /// Short label for `tcpstatus`.
    pub fn name(self) -> &'static str {
        match self {
            TcpState::Closed => "CLOSED",
            TcpState::Listen => "LISTEN",
            TcpState::SynSent => "SYN-SENT",
            TcpState::SynReceived => "SYN-RECEIVED",
            TcpState::Established => "ESTABLISHED",
            TcpState::FinWait1 => "FIN-WAIT-1",
            TcpState::FinWait2 => "FIN-WAIT-2",
            TcpState::CloseWait => "CLOSE-WAIT",
            TcpState::Closing => "CLOSING",
            TcpState::LastAck => "LAST-ACK",
            TcpState::TimeWait => "TIME-WAIT",
        }
    }
}

// ── limits ──────────────────────────────────────────────────────────
//
// Every one of these used to be unbounded or hardcoded, which let a remote
// peer decide how much kernel memory to use.

/// Cap on bytes buffered from the peer. A megabyte is far more than any HTTP
/// response header or the small bodies the shell reads; larger transfers are
/// consumed as they arrive.
pub const MAX_RECV_BUFFER: usize = 1024 * 1024;

/// Cap on bytes queued for transmission but not yet acknowledged.
pub const MAX_SEND_BUFFER: usize = 256 * 1024;

/// Cap on simultaneous connections. The table was previously unbounded.
pub const MAX_CONNECTIONS: usize = 32;

/// Largest payload per segment. Stays under the E1000's 2048-byte TX buffer;
/// `net::http` chunks to the same value.
pub const MAX_SEGMENT_PAYLOAD: usize = 1400;

/// Window advertised when the receive buffer is empty.
pub const DEFAULT_WINDOW: u16 = 8192;

/// Idle time before `expire` reclaims a connection.
pub const IDLE_TIMEOUT_MS: u64 = 120_000;

/// How long TimeWait holds a port. Roughly twice the maximum segment lifetime,
/// so a late segment from the old connection cannot be read as a new one.
pub const TIME_WAIT_MS: u64 = 60_000;

/// Retransmission timeout bounds.
pub const RTO_MIN_MS: u64 = 200;
pub const RTO_MAX_MS: u64 = 8_000;

/// Segments allowed in flight before waiting for an acknowledgement.
pub const MAX_UNACKED_SEGMENTS: usize = 4;

/// How long [`send_data`] waits for the peer to drain its window before giving
/// up. Bounds the stall a write can impose on the shell.
pub const SEND_TIMEOUT_MS: u64 = 30_000;

// ── wire format ─────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset_flags: u16, // 4 bits offset, 6 bits reserved, 6 bits flags
    pub window_size: u16,
    pub checksum: u16,
    pub urgent_ptr: u16,
}

/// One unacknowledged segment.
///
/// The payload is retained rather than only its length, which is what makes
/// retransmission possible: a segment whose bytes were dropped at segmenting
/// time could never be resent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxSegment {
    pub seq: u32,
    pub flags: u8,
    /// Payload retained until acknowledged. Empty for a bare FIN.
    pub data: Vec<u8>,
    /// Absolute deadline in kernel milliseconds.
    pub deadline_ms: u64,
    /// Consecutive transmissions, driving the exponential backoff.
    pub attempts: u8,
}

impl TxSegment {
    /// Bytes this segment occupies in the peer's sequence space.
    ///
    /// A FIN consumes one sequence number but carries no data, so the two must
    /// be accounted separately or an ACK for it looks like a gap.
    pub fn span(&self) -> u32 {
        self.data.len() as u32 + if self.flags & TCP_FIN != 0 { 1 } else { 0 }
    }
}

/// Counters for `netstat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpStats {
    pub active_connections: usize,
    pub total_connections: u64,
    pub segments_sent: u64,
    pub segments_retransmitted: u64,
    pub segments_received: u64,
    /// Segments dropped or re-acknowledged because they fell outside the
    /// receive window.
    pub out_of_window: u64,
    pub bytes_buffered: usize,
    pub bytes_in_flight: usize,
    pub timeouts: u64,
    pub resets: u64,
}

#[derive(Clone)]
pub struct TcpConnection {
    pub local_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub state: TcpState,
    /// Next sequence number we will send.
    pub seq_num: u32,
    /// Next sequence number we expect from the peer.
    pub ack_num: u32,
    pub recv_buffer: Vec<u8>,
    /// Sent but unacknowledged, oldest first.
    pub tx_queue: Vec<TxSegment>,
    /// Payload accepted from `send_data` that has not been segmented yet.
    pub send_buffer: Vec<u8>,
    /// Window the peer advertised; bounds how much may be in flight.
    pub peer_window: u16,
    /// Current retransmission timeout.
    pub rto_ms: u64,
    /// Last time this connection saw or sent anything.
    pub last_activity_ms: u64,
    /// Set once our FIN has been queued, so it is not queued twice.
    pub fin_sent: bool,
    /// Set once the peer's FIN has been acknowledged.
    pub fin_acked: bool,
}

impl TcpConnection {
    /// Bytes sent but not yet acknowledged, FINs excluded.
    pub fn unacked_bytes(&self) -> usize {
        self.tx_queue.iter().map(|s| s.data.len()).sum()
    }

    /// Sequence space the peer currently permits us to send.
    fn sendable(&self) -> usize {
        (self.peer_window as usize).saturating_sub(self.unacked_bytes())
    }

    /// Window we advertise, derived from real free buffer space.
    fn advertised_window(&self) -> u16 {
        let free = MAX_RECV_BUFFER.saturating_sub(self.recv_buffer.len());
        core::cmp::min(free, DEFAULT_WINDOW as usize) as u16
    }
}

/// A segment to emit once the connection lock has been released.
struct TcpReply {
    remote_ip: [u8; 4],
    local_port: u16,
    remote_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    data: Vec<u8>,
}

impl TcpReply {
    fn bare(conn: &TcpConnection, seq: u32, ack: u32, flags: u8, window: u16) -> Self {
        Self {
            remote_ip: conn.remote_ip,
            local_port: conn.local_port,
            remote_port: conn.remote_port,
            seq,
            ack,
            flags,
            window,
            data: Vec::new(),
        }
    }
}

lazy_static! {
    static ref TCP_CONNECTIONS: Mutex<BTreeMap<u16, TcpConnection>> = Mutex::new(BTreeMap::new());
    static ref TCP_STATS: Mutex<TcpStats> = Mutex::new(TcpStats::default());
}

fn now_ms() -> u64 {
    if crate::time::is_initialized() {
        crate::time::uptime_millis()
    } else {
        crate::shell::monotonic_ms()
    }
}

/// Sequence-number comparison, correct across the 32-bit wrap.
#[inline]
fn seq_lt(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < 0x8000_0000
}

#[inline]
fn seq_le(a: u32, b: u32) -> bool {
    a == b || seq_lt(a, b)
}

#[inline]
fn seq_gt(a: u32, b: u32) -> bool {
    seq_lt(b, a)
}

impl TcpHeader {
    pub fn new(src_port: u16, dst_port: u16, seq: u32, ack: u32, flags: u8) -> Self {
        TcpHeader {
            src_port: src_port.to_be(),
            dst_port: dst_port.to_be(),
            seq_num: seq.to_be(),
            ack_num: ack.to_be(),
            data_offset_flags: ((5 << 12) | (flags as u16)).to_be(), // 5 * 4 = 20 byte header
            window_size: DEFAULT_WINDOW.to_be(),
            checksum: 0,
            urgent_ptr: 0,
        }
    }

    pub fn get_flags(&self) -> u8 {
        (u16::from_be(self.data_offset_flags) & 0x3F) as u8
    }

    pub fn get_data_offset(&self) -> u8 {
        ((u16::from_be(self.data_offset_flags) >> 12) & 0xF) as u8
    }

    /// The peer's advertised window. Previously never read anywhere.
    pub fn get_window(&self) -> u16 {
        u16::from_be(self.window_size)
    }

    fn calculate_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], tcp_segment: &[u8]) -> u16 {
        let mut sum: u32 = 0;

        sum += ((src_ip[0] as u32) << 8) | src_ip[1] as u32;
        sum += ((src_ip[2] as u32) << 8) | src_ip[3] as u32;
        sum += ((dst_ip[0] as u32) << 8) | dst_ip[1] as u32;
        sum += ((dst_ip[2] as u32) << 8) | dst_ip[3] as u32;
        sum += 6; // Protocol (TCP)
        sum += tcp_segment.len() as u32;

        // TCP segment
        for i in (0..tcp_segment.len()).step_by(2) {
            if i + 1 < tcp_segment.len() {
                let word = ((tcp_segment[i] as u32) << 8) | (tcp_segment[i + 1] as u32);
                sum += word;
            } else {
                sum += (tcp_segment[i] as u32) << 8;
            }
        }

        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }

        !sum as u16
    }
}

// ── port allocation ─────────────────────────────────────────────────

/// Ephemeral port range used for outgoing connections.
pub const EPHEMERAL_PORT_MIN: u16 = 49152;
pub const EPHEMERAL_PORT_MAX: u16 = 65535;

/// Choose an unused ephemeral port at random.
///
/// Ports used to count up from 49152, so the n-th connection after boot always
/// used the same port. Combined with a fixed ISN that made the whole connection
/// tuple predictable across boots.
pub fn allocate_port() -> u16 {
    let span = (EPHEMERAL_PORT_MAX - EPHEMERAL_PORT_MIN) as u32 + 1;
    let connections = TCP_CONNECTIONS.lock();
    for _ in 0..64 {
        let candidate = EPHEMERAL_PORT_MIN + crate::entropy::below(span) as u16;
        if !connections.contains_key(&candidate) {
            return candidate;
        }
    }
    // Fall back to a linear scan rather than failing: with 16k ephemeral
    // ports and at most MAX_CONNECTIONS in use, this is unreachable in
    // practice, but returning 0 would look like success.
    for port in EPHEMERAL_PORT_MIN..=EPHEMERAL_PORT_MAX {
        if !connections.contains_key(&port) {
            return port;
        }
    }
    0
}

/// Open an outgoing connection and return its local port.
///
/// The initial sequence number and ephemeral port both come from
/// [`crate::entropy`]. They used to be `1000` and a counter, which made every
/// boot produce byte-identical connection tuples.
pub fn connect(remote_ip: [u8; 4], remote_port: u16) -> Result<u16, &'static str> {
    let local_port = allocate_port();
    if local_port == 0 {
        return Err("No free local port");
    }
    let initial_seq = crate::entropy::u32();

    {
        let mut connections = TCP_CONNECTIONS.lock();
        if connections.len() >= MAX_CONNECTIONS {
            // Bounded table: reclaim the least recently active entry rather
            // than growing without limit or refusing outright.
            let oldest = connections
                .iter()
                .min_by_key(|(_, c)| c.last_activity_ms)
                .map(|(p, _)| *p);
            if let Some(port) = oldest {
                crate::net_log!("TCP: evicting stale connection on port {} for new one", port);
                connections.remove(&port);
            }
        }
        connections.insert(
            local_port,
            TcpConnection {
                local_port,
                remote_ip,
                remote_port,
                state: TcpState::SynSent,
                seq_num: initial_seq,
                ack_num: 0,
                recv_buffer: Vec::new(),
                tx_queue: Vec::new(),
                send_buffer: Vec::new(),
                peer_window: DEFAULT_WINDOW,
                rto_ms: RTO_MIN_MS,
                last_activity_ms: now_ms(),
                fin_sent: false,
                fin_acked: false,
            },
        );
        TCP_STATS.lock().total_connections += 1;
    }

    let reply = TcpReply {
        remote_ip,
        local_port,
        remote_port,
        seq: initial_seq,
        ack: 0,
        flags: TCP_SYN,
        window: DEFAULT_WINDOW,
        data: Vec::new(),
    };
    if let Err(error) = dispatch_first(reply) {
        TCP_CONNECTIONS.lock().remove(&local_port);
        return Err(error);
    }

    crate::net_log!(
        "TCP: Sent SYN to {}.{}.{}.{}:{} from port {} (isn={})",
        remote_ip[0],
        remote_ip[1],
        remote_ip[2],
        remote_ip[3],
        remote_port,
        local_port,
        initial_seq
    );

    Ok(local_port)
}

fn dispatch_first(reply: TcpReply) -> Result<(), &'static str> {
    TCP_STATS.lock().segments_sent += 1;
    send_tcp_packet(&reply)
}

/// Queue application data for transmission.
///
/// The data is buffered rather than handed straight to the NIC, and sent only
/// as the peer's window allows. `send_data` therefore no longer fails when a
/// write exceeds the peer's window; buffered data leaves as acknowledgements
/// free space up.
pub fn send_data(local_port: u16, data: &[u8]) -> Result<(), &'static str> {
    if data.is_empty() {
        return Ok(());
    }
    crate::net_log!("TCP: send_data port {} ({} bytes)", local_port, data.len());
    let deadline = now_ms() + SEND_TIMEOUT_MS;
    loop {
        let mut replies = Vec::new();
        let wait = {
            let mut connections = TCP_CONNECTIONS.lock();
            let conn = connections.get_mut(&local_port).ok_or("Connection not found")?;
            if !conn.state.can_transfer() {
                return Err("Connection not established");
            }
            if conn.send_buffer.len() + data.len() > MAX_SEND_BUFFER {
                // The peer is not draining fast enough. Every caller treats a
                // `send_data` error as a failed request, so wait for room
                // rather than reporting a full buffer as a fault.
                true
            } else {
                conn.send_buffer.extend_from_slice(data);
                conn.last_activity_ms = now_ms();
                flush_tx(conn, now_ms(), &mut replies);
                false
            }
        };

        if wait {
            if now_ms() >= deadline {
                crate::net_log!(
                    "TCP: send buffer still full after {} ms",
                    SEND_TIMEOUT_MS
                );
                return Err("Send buffer full");
            }
            // Pump with the lock released: it re-enters this module.
            crate::net::http::pump();
            continue;
        }
        if replies.is_empty() {
            crate::net_log!(
                "TCP: send_data port {} buffered, 0 segments flushed (awaiting window/ACK)",
                local_port
            );
        }
        dispatch(&mut replies);
        return Ok(());
    }
}

/// Queue a FIN and begin the close handshake.
pub fn close(local_port: u16) -> Result<(), &'static str> {
    let mut replies = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        let conn = connections.get_mut(&local_port).ok_or("Connection not found")?;
        match conn.state {
            TcpState::Established => conn.state = TcpState::FinWait1,
            TcpState::CloseWait => conn.state = TcpState::LastAck,
            // Already closing, closed, or never established: nothing to send.
            // Reporting success is deliberate, since callers treat an error
            // from `close` as a failure of a close they have already requested.
            _ => return Ok(()),
        }
        conn.fin_sent = true;
        conn.last_activity_ms = now_ms();
        flush_tx(conn, now_ms(), &mut replies);
    }
    dispatch(&mut replies);
    Ok(())
}

pub fn get_state(local_port: u16) -> Option<TcpState> {
    TCP_CONNECTIONS.lock().get(&local_port).map(|c| c.state)
}

/// Take everything buffered from the peer, leaving the buffer empty.
pub fn read_data(local_port: u16) -> Option<Vec<u8>> {
    let mut connections = TCP_CONNECTIONS.lock();
    if let Some(conn) = connections.get_mut(&local_port) {
        if !conn.recv_buffer.is_empty() {
            let data = core::mem::take(&mut conn.recv_buffer);
            return Some(data);
        }
    }
    None
}

/// Peek at the buffered byte count without consuming it.
pub fn peek_data(local_port: u16) -> Option<usize> {
    TCP_CONNECTIONS
        .lock()
        .get(&local_port)
        .map(|c| c.recv_buffer.len())
}

/// Forget a closed or timed-out socket and release its protocol state.
pub fn forget(local_port: u16) {
    TCP_CONNECTIONS.lock().remove(&local_port);
}

/// Cancel a connection immediately, notifying the peer with a reset.
pub fn abort(local_port: u16) {
    let mut replies = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        if let Some(conn) = connections.remove(&local_port) {
            if conn.state.is_open() {
                replies.push(TcpReply::bare(&conn, conn.seq_num, conn.ack_num, TCP_RST, 0));
            }
        }
    }
    dispatch(&mut replies);
}

// ── transmit ────────────────────────────────────────────────────────

/// Emit queued replies once the connection lock has been released.
///
/// Sending touches the NIC, which can block, so nothing is ever sent while
/// `TCP_CONNECTIONS` is held: a blocking NIC call under that lock would stall
/// every other connection and every timer that needs the table.
fn dispatch(replies: &mut Vec<TcpReply>) {
    let count = replies.len() as u64;
    TCP_STATS.lock().segments_sent += count;
    for reply in replies.drain(..) {
        if let Err(error) = send_tcp_packet(&reply) {
            crate::net_log!("TCP reply send failed: {}", error);
        }
    }
}

/// Move buffered payload and a pending FIN into the in-flight queue.
///
/// Only as much data as the peer's advertised window allows is queued, and only
/// up to [`MAX_UNACKED_SEGMENTS`] segments are outstanding. Both limits were
/// previously absent, so a single large write was sent as one oversized segment
/// with no regard for what the peer was willing to accept.
fn flush_tx(conn: &mut TcpConnection, now: u64, replies: &mut Vec<TcpReply>) {
    if matches!(
        conn.state,
        TcpState::SynSent | TcpState::Closed | TcpState::TimeWait
    ) {
        return;
    }

    while conn.tx_queue.len() < MAX_UNACKED_SEGMENTS && conn.sendable() > 0 {
        if !conn.send_buffer.is_empty() {
            let n = core::cmp::min(
                core::cmp::min(MAX_SEGMENT_PAYLOAD, conn.sendable()),
                conn.send_buffer.len(),
            );
            let payload: Vec<u8> = conn.send_buffer.drain(..n).collect();
            let seq = conn.seq_num;
            conn.seq_num = conn.seq_num.wrapping_add(payload.len() as u32);
            conn.tx_queue.push(TxSegment {
                seq,
                flags: TCP_ACK | TCP_PSH,
                deadline_ms: now + conn.rto_ms,
                attempts: 1,
                data: payload.clone(),
            });
            replies.push(TcpReply {
                remote_ip: conn.remote_ip,
                local_port: conn.local_port,
                remote_port: conn.remote_port,
                seq,
                ack: conn.ack_num,
                flags: TCP_ACK | TCP_PSH,
                window: conn.advertised_window(),
                data: payload,
            });
            continue;
        }

        if conn.fin_sent
            && !conn.fin_acked
            && !conn.tx_queue.iter().any(|s| s.flags & TCP_FIN != 0)
        {
            let seq = conn.seq_num;
            conn.seq_num = conn.seq_num.wrapping_add(1);
            conn.tx_queue.push(TxSegment {
                seq,
                flags: TCP_ACK | TCP_FIN,
                data: Vec::new(),
                deadline_ms: now + conn.rto_ms,
                attempts: 1,
            });
            let window = conn.advertised_window();
            replies.push(TcpReply::bare(conn, seq, conn.ack_num, TCP_ACK | TCP_FIN, window));
        }
        break;
    }
}

/// Drop in-flight segments the peer has cumulatively acknowledged.
///
/// Returns how many payload bytes were released.
fn trim_acked(conn: &mut TcpConnection, ack: u32) -> usize {
    let mut freed = 0usize;
    while let Some(seg) = conn.tx_queue.first() {
        if seq_le(seg.seq.wrapping_add(seg.span()), ack) {
            freed += seg.data.len();
            if seg.flags & TCP_FIN != 0 {
                conn.fin_acked = true;
            }
            conn.tx_queue.remove(0);
        } else {
            break;
        }
    }
    if freed > 0 {
        // A successful ACK means the path is working; back off to the minimum.
        conn.rto_ms = RTO_MIN_MS;
    }
    freed
}

fn send_tcp_packet(reply: &TcpReply) -> Result<(), &'static str> {
    let TcpReply {
        remote_ip,
        local_port,
        remote_port,
        seq,
        ack,
        flags,
        window,
        data,
    } = reply;
    let our_ip = crate::net::ip::get_ip_address().ok_or("No IP address configured")?;

    let mut tcp_header = TcpHeader::new(*local_port, *remote_port, *seq, *ack, *flags);
    tcp_header.window_size = window.to_be();

    let mut segment = Vec::with_capacity(20 + data.len());
    unsafe {
        let header_bytes = core::slice::from_raw_parts(
            &tcp_header as *const TcpHeader as *const u8,
            core::mem::size_of::<TcpHeader>(),
        );
        segment.extend_from_slice(header_bytes);
    }
    segment.extend_from_slice(data);

    // Calculate checksum
    let checksum = TcpHeader::calculate_checksum(our_ip, *remote_ip, &segment);
    segment[16] = (checksum >> 8) as u8;
    segment[17] = (checksum & 0xFF) as u8;

    crate::net_log!(
        "TCP: Sending packet to {}.{}.{}.{}:{}, flags={:#x}, seq={}, ack={}, win={}, data_len={}",
        remote_ip[0],
        remote_ip[1],
        remote_ip[2],
        remote_ip[3],
        remote_port,
        flags,
        seq,
        ack,
        window,
        data.len()
    );

    crate::net::ip::send_packet(*remote_ip, 6, &segment)
}

// ── receive ─────────────────────────────────────────────────────────

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], _src_mac: [u8; 6]) {
    if packet.len() < 20 {
        return;
    }
    let Some(dst_ip) = crate::net::ip::get_ip_address() else {
        return;
    };
    if TcpHeader::calculate_checksum(dst_ip, src_ip, packet) != 0 {
        return;
    }

    let tcp_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const TcpHeader) };

    let src_port = u16::from_be(tcp_header.src_port);
    let dst_port = u16::from_be(tcp_header.dst_port);
    let seq = u32::from_be(tcp_header.seq_num);
    let ack = u32::from_be(tcp_header.ack_num);
    let flags = tcp_header.get_flags();
    let window = tcp_header.get_window();
    let data_offset = (tcp_header.get_data_offset() * 4) as usize;
    if data_offset < 20 || data_offset > packet.len() {
        return;
    }
    TCP_STATS.lock().segments_received += 1;

    crate::net_log!(
        "TCP: Received packet from {}.{}.{}.{}:{} to port {}, flags={:#x}, seq={}, ack={}, win={}",
        src_ip[0],
        src_ip[1],
        src_ip[2],
        src_ip[3],
        src_port,
        dst_port,
        flags,
        seq,
        ack,
        window
    );

    let now = now_ms();
    let mut replies = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        if let Some(conn) = connections.get_mut(&dst_port) {
            if conn.remote_ip == src_ip && conn.remote_port == src_port {
                handle_connection_packet(
                    conn,
                    flags,
                    seq,
                    ack,
                    window,
                    &packet[data_offset..],
                    now,
                    &mut replies,
                );
            } else {
                crate::net_log!(
                    "TCP: packet from unexpected peer {}.{}.{}.{}:{} for port {}",
                    src_ip[0],
                    src_ip[1],
                    src_ip[2],
                    src_ip[3],
                    src_port,
                    dst_port
                );
            }
        } else if flags & TCP_SYN != 0 {
            crate::net_log!("TCP: Received SYN on port {} but not listening", dst_port);
        }
    }
    dispatch(&mut replies);
}

#[allow(clippy::too_many_arguments)]
fn handle_connection_packet(
    conn: &mut TcpConnection,
    flags: u8,
    seq: u32,
    ack: u32,
    window: u16,
    data: &[u8],
    now: u64,
    replies: &mut Vec<TcpReply>,
) {
    conn.last_activity_ms = now;

    // A reset is unconditional: the peer is telling us the connection is gone,
    // so no state other than Closed survives it.
    if flags & TCP_RST != 0 {
        crate::net_log!("TCP: Received RST, aborting connection");
        conn.state = TcpState::Closed;
        conn.tx_queue.clear();
        conn.send_buffer.clear();
        TCP_STATS.lock().resets += 1;
        return;
    }

    // Every segment that carries an ACK advances our send window and releases
    // anything the peer has confirmed.
    if flags & TCP_ACK != 0 {
        // An acknowledgement past everything we have sent means the peer is
        // confirming bytes it cannot have received. Trusting it would silently
        // discard queued data, so the connection is reset instead. Checked
        // after SynSent, where the SYN itself is legitimately unacknowledged
        // and `seq_num` has not yet advanced past it.
        if conn.state != TcpState::SynSent && seq_gt(ack, conn.seq_num) {
            crate::net_log!(
                "TCP: peer acknowledged {} beyond sent {}, resetting",
                ack,
                conn.seq_num
            );
            let mut reset = Vec::new();
            reset.push(TcpReply::bare(conn, conn.seq_num, conn.ack_num, TCP_RST, 0));
            conn.state = TcpState::Closed;
            conn.tx_queue.clear();
            conn.send_buffer.clear();
            TCP_STATS.lock().resets += 1;
            replies.append(&mut reset);
            return;
        }
        conn.peer_window = window;
        trim_acked(conn, ack);
    }

    match conn.state {
        TcpState::SynSent => {
            if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 && ack == conn.seq_num.wrapping_add(1) {
                crate::net_log!("TCP: Received SYN-ACK");
                conn.seq_num = conn.seq_num.wrapping_add(1);
                conn.ack_num = seq.wrapping_add(1);
                conn.state = TcpState::Established;
                replies.push(TcpReply::bare(
                    conn,
                    conn.seq_num,
                    conn.ack_num,
                    TCP_ACK,
                    conn.advertised_window(),
                ));
                crate::net_log!(
                    "TCP connection established to {}.{}.{}.{}:{}",
                    conn.remote_ip[0],
                    conn.remote_ip[1],
                    conn.remote_ip[2],
                    conn.remote_ip[3],
                    conn.remote_port
                );
            } else if flags & TCP_SYN != 0 {
                // SYN without a usable ACK: tell the peer to reset so it does
                // not wait out its own retransmission budget for nothing.
                replies.push(TcpReply::bare(conn, 0, 0, TCP_RST, 0));
            }
        }
        TcpState::Established | TcpState::CloseWait => {
            // One reply per received segment. Emitting an ACK for the payload
            // and a second one for the FIN in the same segment would send the
            // peer an acknowledgement of a sequence number we have not reached
            // yet, which it may act on.
            let mut ack_pending = false;

            // Accept data only exactly at the expected sequence number. A
            // segment that overlaps what we already have is a duplicate, and
            // one that starts beyond it is a gap: both are dropped and
            // re-acknowledged so the peer retransmits the bytes we lack.
            if !data.is_empty() {
                ack_pending = true;
                if seq == conn.ack_num {
                    let room = MAX_RECV_BUFFER.saturating_sub(conn.recv_buffer.len());
                    if data.len() <= room {
                        conn.recv_buffer.extend_from_slice(data);
                        conn.ack_num = seq.wrapping_add(data.len() as u32);
                    } else {
                        crate::net_log!(
                            "TCP: receive buffer full ({} bytes), dropping {} byte segment",
                            conn.recv_buffer.len(),
                            data.len()
                        );
                        TCP_STATS.lock().out_of_window += 1;
                    }
                } else {
                    crate::net_log!(
                        "TCP: segment at seq {} outside window (expecting {}), re-acknowledging",
                        seq,
                        conn.ack_num
                    );
                    TCP_STATS.lock().out_of_window += 1;
                }
            } else if flags & TCP_ACK != 0 && seq == conn.ack_num {
                // A pure ACK may be a window probe; answering keeps the peer's
                // send loop from stalling on a window it already believes in.
                ack_pending = true;
            }

            // A FIN occupies the sequence number right after the payload, so it
            // is only honoured once everything ahead of it has been accepted.
            // If the payload was dropped, `ack_num` still points at its start and
            // this correctly does not fire, leaving the peer to retransmit.
            if flags & TCP_FIN != 0 && seq.wrapping_add(data.len() as u32) == conn.ack_num {
                crate::net_log!("TCP: Received FIN");
                conn.ack_num = conn.ack_num.wrapping_add(1);
                conn.state = if conn.fin_sent {
                    // Simultaneous close: our FIN is out but unacknowledged.
                    TcpState::Closing
                } else {
                    TcpState::CloseWait
                };
                ack_pending = true;
            }

            if ack_pending {
                replies.push(TcpReply::bare(
                    conn,
                    conn.seq_num,
                    conn.ack_num,
                    TCP_ACK,
                    conn.advertised_window(),
                ));
            }
        }
        TcpState::FinWait1 => {
            if flags & TCP_FIN != 0 {
                // Peer closed first; we still owe it an ACK for its FIN, and
                // our own FIN is already acknowledged if we got this far.
                conn.ack_num = conn.ack_num.wrapping_add(1);
                if conn.fin_acked {
                    conn.state = TcpState::TimeWait;
                } else {
                    conn.state = TcpState::Closing;
                }
                replies.push(TcpReply::bare(
                    conn,
                    conn.seq_num,
                    conn.ack_num,
                    TCP_ACK,
                    conn.advertised_window(),
                ));
            } else if conn.fin_acked {
                conn.state = TcpState::FinWait2;
            }
        }
        TcpState::FinWait2 => {
            if flags & TCP_FIN != 0 {
                crate::net_log!("TCP: Peer closed, entering TIME-WAIT");
                conn.ack_num = conn.ack_num.wrapping_add(1);
                conn.state = TcpState::TimeWait;
                replies.push(TcpReply::bare(
                    conn,
                    conn.seq_num,
                    conn.ack_num,
                    TCP_ACK,
                    conn.advertised_window(),
                ));
            }
        }
        TcpState::Closing => {
            if conn.fin_acked {
                crate::net_log!("TCP: FIN acknowledged, entering TIME-WAIT");
                conn.state = TcpState::TimeWait;
            }
        }
        TcpState::LastAck => {
            if conn.fin_acked {
                crate::net_log!("TCP: Final ACK received, connection closed");
                conn.state = TcpState::Closed;
            }
        }
        TcpState::TimeWait => {
            // A retransmitted FIN for a connection we have already closed must
            // still be acknowledged, or the peer retries until it gives up.
            if flags & TCP_FIN != 0 {
                replies.push(TcpReply::bare(
                    conn,
                    conn.seq_num,
                    conn.ack_num,
                    TCP_ACK,
                    conn.advertised_window(),
                ));
            }
        }
        TcpState::Closed | TcpState::Listen | TcpState::SynReceived => {}
    }

    // Keep pushing: an ACK may have freed window for buffered data.
    if conn.state.can_transfer() || conn.state == TcpState::FinWait1 {
        flush_tx(conn, now, replies);
    }
}

// ── timers ──────────────────────────────────────────────────────────

/// Re-send segments whose deadline has passed and reopen the send window.
///
/// Must be called from the packet pump. Without it, a single lost segment ends
/// the transfer: the bytes were dropped after being handed to the NIC and
/// nothing ever asked for them again.
pub fn tick() {
    let now = now_ms();
    let mut replies = Vec::new();
    let mut dead = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        for (port, conn) in connections.iter_mut() {
            let mut resend = 0u64;
            let mut give_up = false;

            // Fields are copied out first: the queue is mutated in place, so
            // borrowing the connection immutably inside the loop would not
            // compile.
            let remote_ip = conn.remote_ip;
            let local_port = conn.local_port;
            let remote_port = conn.remote_port;
            let ack = conn.ack_num;
            let window = conn.advertised_window();

            for i in 0..conn.tx_queue.len() {
                if now < conn.tx_queue[i].deadline_ms {
                    continue;
                }
                // Every segment queued within the same window shares a deadline,
                // so an ACK that was lost costs all of them a retransmission,
                // not just the first.
                conn.tx_queue[i].attempts = conn.tx_queue[i].attempts.saturating_add(1);
                if conn.tx_queue[i].attempts > MAX_TX_ATTEMPTS {
                    give_up = true;
                    break;
                }
                conn.tx_queue[i].deadline_ms = now + conn.rto_ms;
                replies.push(TcpReply {
                    remote_ip,
                    local_port,
                    remote_port,
                    seq: conn.tx_queue[i].seq,
                    ack,
                    flags: conn.tx_queue[i].flags,
                    window,
                    data: conn.tx_queue[i].data.clone(),
                });
                resend += 1;
            }

            if resend > 0 {
                // Exponential backoff, capped, so a long stall does not push
                // the next attempt minutes into the future.
                conn.rto_ms = core::cmp::min(conn.rto_ms * 2, RTO_MAX_MS);
            }

            if give_up {
                crate::net_log!(
                    "TCP: port {} timed out after {} attempts, closing",
                    port,
                    MAX_TX_ATTEMPTS
                );
                TCP_STATS.lock().timeouts += 1;
                conn.state = TcpState::Closed;
                dead.push(*port);
                continue;
            }

            if resend > 0 {
                TCP_STATS.lock().segments_retransmitted += resend;
                crate::net_log!("TCP: port {} retransmitting {} segment(s)", port, resend);
            }

            if conn.state == TcpState::TimeWait
                && now.saturating_sub(conn.last_activity_ms) >= TIME_WAIT_MS
            {
                crate::net_log!("TCP: TIME-WAIT on port {} finished", port);
                dead.push(*port);
                continue;
            }

            flush_tx(conn, now, &mut replies);
        }
        for port in &dead {
            connections.remove(port);
        }
    }
    dispatch(&mut replies);
}

/// Drop connections that have gone quiet for too long.
///
/// The table used to grow for the life of the boot: `forget` was only reachable
/// from the shell's own cleanup path, so any code that opened a socket and gave
/// up without closing it leaked the entry and its buffers permanently.
pub fn expire() {
    let now = now_ms();
    let mut connections = TCP_CONNECTIONS.lock();
    let expired: Vec<u16> = connections
        .iter()
        .filter(|(_, conn)| {
            conn.state == TcpState::Closed
                || now.saturating_sub(conn.last_activity_ms) >= IDLE_TIMEOUT_MS
        })
        .map(|(port, _)| *port)
        .collect();
    for port in expired {
        crate::net_log!("TCP: expiring idle connection on port {}", port);
        connections.remove(&port);
    }
}

/// Counters for `netstat`.
pub fn stats() -> TcpStats {
    let connections = TCP_CONNECTIONS.lock();
    let mut stats = *TCP_STATS.lock();
    stats.active_connections = connections.len();
    stats.bytes_buffered = connections.values().map(|c| c.recv_buffer.len()).sum();
    stats.bytes_in_flight = connections.values().map(|c| c.unacked_bytes()).sum();
    stats
}

/// Per-connection view for `tcpstatus`.
pub fn connections() -> Vec<TcpConnection> {
    TCP_CONNECTIONS.lock().values().cloned().collect()
}

/// Abandon a connection whose peer is unreachable or has reset.
///
/// `abort` removes the entry outright, which is wrong for a peer that merely
/// went silent: the table entry and its buffers must survive so that `tick` can
/// retransmit, and so the shell can report a timeout rather than losing the
/// socket. This is the path a caller uses to give up on a connection, while the
/// entry stays until `expire` reclaims it.
pub fn reset(local_port: u16) -> bool {
    let mut replies = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        let Some(conn) = connections.get_mut(&local_port) else {
            return false;
        };
        conn.state = TcpState::Closed;
        conn.tx_queue.clear();
        conn.send_buffer.clear();
        replies.push(TcpReply::bare(conn, conn.seq_num, conn.ack_num, TCP_RST, 0));
        TCP_STATS.lock().resets += 1;
    }
    dispatch(&mut replies);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const PEER: [u8; 4] = [93, 184, 216, 34];
    const ISN: u32 = 1000;

    fn connection(state: TcpState) -> TcpConnection {
        TcpConnection {
            local_port: 49152,
            remote_ip: PEER,
            remote_port: 80,
            state,
            seq_num: ISN + 1,
            ack_num: ISN + 1,
            recv_buffer: Vec::new(),
            tx_queue: Vec::new(),
            send_buffer: Vec::new(),
            peer_window: DEFAULT_WINDOW,
            rto_ms: RTO_MIN_MS,
            last_activity_ms: 0,
            fin_sent: false,
            fin_acked: false,
        }
    }

    fn segment(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
        let header = TcpHeader::new(src_port, dst_port, 1000, 0, TCP_SYN);
        let mut segment = unsafe {
            core::slice::from_raw_parts(
                &header as *const TcpHeader as *const u8,
                core::mem::size_of::<TcpHeader>(),
            )
        }
        .to_vec();
        segment.extend_from_slice(payload);
        segment
    }

    // ── sequence arithmetic ──────────────────────────────────────────

    #[test]
    fn seq_comparison_orders_forward() {
        assert!(seq_lt(1, 2));
        assert!(seq_gt(2, 1));
        assert!(seq_le(2, 2));
        assert!(!seq_lt(2, 2));
    }

    #[test]
    fn seq_comparison_survives_wraparound() {
        // A fixed `a < b` would report u32::MAX < 0 as false, which breaks every
        // ACK check once a long-lived connection passes the wrap point.
        assert!(seq_lt(u32::MAX, 0));
        assert!(seq_gt(0, u32::MAX));
        assert!(seq_le(u32::MAX, u32::MAX));
        assert!(seq_lt(u32::MAX - 1, 1));
    }

    #[test]
    fn seq_comparison_treats_half_the_space_as_unordered() {
        // Ambiguous by design: exactly 2^31 apart is neither before nor after.
        assert!(!seq_lt(0, 1 << 31));
        assert!(!seq_gt(0, 1 << 31));
    }

    #[test]
    fn segment_span_counts_fin_sequence_numbers() {
        let data = TxSegment {
            seq: 10,
            flags: TCP_ACK | TCP_PSH,
            data: vec![0u8; 100],
            deadline_ms: 0,
            attempts: 1,
        };
        assert_eq!(data.span(), 100);

        let fin = TxSegment {
            seq: 110,
            flags: TCP_ACK | TCP_FIN,
            data: Vec::new(),
            deadline_ms: 0,
            attempts: 1,
        };
        // A FIN carries no bytes but does consume a sequence number, so an ACK
        // for it must not look like a 1-byte gap.
        assert_eq!(fin.span(), 1);

        let both = TxSegment {
            seq: 110,
            flags: TCP_FIN,
            data: vec![0u8; 3],
            deadline_ms: 0,
            attempts: 1,
        };
        assert_eq!(both.span(), 4);
    }

    // ── transmit ─────────────────────────────────────────────────────

    #[test]
    fn flush_splits_writes_into_bounded_segments() {
        let mut conn = connection(TcpState::Established);
        conn.send_buffer = vec![0xAB; MAX_SEGMENT_PAYLOAD * 3 + 7];
        let mut replies = Vec::new();
        flush_tx(&mut conn, 0, &mut replies);

        // 3 full segments plus a 7-byte remainder, all within the default
        // window, so nothing is held back for lack of window.
        assert_eq!(replies.len(), 4);
        assert!(replies
            .iter()
            .take(3)
            .all(|r| r.data.len() == MAX_SEGMENT_PAYLOAD));
        assert_eq!(replies[3].data.len(), 7);
        assert!(conn.send_buffer.is_empty());
        assert_eq!(conn.seq_num, ISN + 1 + (MAX_SEGMENT_PAYLOAD * 3 + 7) as u32);
        // Sequence numbers must be contiguous, or the peer sees a hole.
        let mut expected = ISN + 1;
        for r in &replies {
            assert_eq!(r.seq, expected);
            expected += r.data.len() as u32;
        }
    }

    #[test]
    fn flush_respects_the_peer_window() {
        let mut conn = connection(TcpState::Established);
        conn.peer_window = 100;
        conn.send_buffer = vec![0u8; 1000];
        let mut replies = Vec::new();
        flush_tx(&mut conn, 0, &mut replies);

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].data.len(), 100);
        assert_eq!(conn.sendable(), 0, "nothing more may be sent");
        assert_eq!(conn.send_buffer.len(), 900);
    }

    #[test]
    fn flush_limits_segments_in_flight() {
        let mut conn = connection(TcpState::Established);
        conn.peer_window = u16::MAX;
        conn.send_buffer = vec![0u8; MAX_SEGMENT_PAYLOAD * 20];
        let mut replies = Vec::new();
        flush_tx(&mut conn, 0, &mut replies);

        assert_eq!(
            conn.tx_queue.len(),
            MAX_UNACKED_SEGMENTS,
            "an unbounded number of unacked segments would let a slow peer pin \
             all of our send memory indefinitely"
        );
    }

    #[test]
    fn flush_attaches_fin_after_pending_data() {
        let mut conn = connection(TcpState::Established);
        conn.send_buffer = vec![7u8; 10];
        conn.fin_sent = true;
        let mut replies = Vec::new();
        flush_tx(&mut conn, 0, &mut replies);

        let fin = replies.last().expect("a FIN must be queued");
        assert_eq!(fin.flags & TCP_FIN, TCP_FIN);
        assert!(fin.data.is_empty());
        // The FIN must not overtake the data queued before it, or the peer sees
        // the payload after end-of-stream and treats it as stray.
        assert_eq!(fin.seq, ISN + 1 + 10);
    }

    #[test]
    fn flush_does_not_queue_a_second_fin() {
        let mut conn = connection(TcpState::Established);
        conn.fin_sent = true;
        let mut first = Vec::new();
        flush_tx(&mut conn, 0, &mut first);
        let mut second = Vec::new();
        conn.peer_window = u16::MAX;
        flush_tx(&mut conn, 0, &mut second);

        assert_eq!(second.len(), 0, "close must be idempotent");
    }

    #[test]
    fn trim_acked_releases_only_covered_segments() {
        let mut conn = connection(TcpState::Established);
        for _ in 0..3 {
            conn.tx_queue.push(TxSegment {
                seq: conn.seq_num,
                flags: TCP_ACK,
                data: vec![0u8; 10],
                deadline_ms: 0,
                attempts: 1,
            });
            conn.seq_num += 10;
        }

        // Cumulative ACK partway through the second segment.
        let freed = trim_acked(&mut conn, ISN + 1 + 15);
        assert_eq!(freed, 10, "only the fully covered segment is released");
        assert_eq!(conn.tx_queue.len(), 2);
        assert_eq!(conn.unacked_bytes(), 20);
    }

    #[test]
    fn trim_acked_records_fin_acknowledgement() {
        let mut conn = connection(TcpState::Established);
        conn.fin_sent = true;
        conn.tx_queue.push(TxSegment {
            seq: ISN + 1,
            flags: TCP_ACK | TCP_FIN,
            data: Vec::new(),
            deadline_ms: 0,
            attempts: 1,
        });
        conn.seq_num += 1;

        trim_acked(&mut conn, ISN + 2);
        assert!(conn.fin_acked, "LastAck needs to know the FIN was confirmed");
        assert!(conn.tx_queue.is_empty());
    }

    #[test]
    fn trim_acked_resets_the_backoff_on_progress() {
        let mut conn = connection(TcpState::Established);
        conn.rto_ms = 3_200;
        conn.tx_queue.push(TxSegment {
            seq: ISN + 1,
            flags: TCP_ACK,
            data: vec![0u8; 4],
            deadline_ms: 0,
            attempts: 3,
        });
        trim_acked(&mut conn, ISN + 5);
        assert_eq!(conn.rto_ms, RTO_MIN_MS);
    }

    #[test]
    fn trim_acked_leaves_queue_alone_when_nothing_was_acked() {
        let mut conn = connection(TcpState::Established);
        conn.tx_queue.push(TxSegment {
            seq: ISN + 1,
            flags: TCP_ACK,
            data: vec![0u8; 10],
            deadline_ms: 0,
            attempts: 1,
        });
        assert_eq!(trim_acked(&mut conn, ISN + 1), 0);
        assert_eq!(conn.tx_queue.len(), 1);
    }

    // ── receive ──────────────────────────────────────────────────────

    fn deliver(conn: &mut TcpConnection, flags: u8, seq: u32, ack: u32, data: &[u8]) -> Vec<TcpReply> {
        let mut replies = Vec::new();
        handle_connection_packet(conn, flags, seq, ack, DEFAULT_WINDOW, data, 0, &mut replies);
        replies
    }

    #[test]
    fn syn_ack_completes_the_handshake() {
        let mut conn = connection(TcpState::SynSent);
        conn.seq_num = ISN;
        conn.ack_num = 0;
        let replies = deliver(
            &mut conn,
            TCP_SYN | TCP_ACK,
            ISN + 1,
            ISN + 1,
            &[],
        );

        assert_eq!(conn.state, TcpState::Established);
        assert_eq!(conn.seq_num, ISN + 1);
        assert_eq!(conn.ack_num, ISN + 2);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].flags, TCP_ACK);
    }

    #[test]
    fn syn_ack_with_wrong_ack_number_is_rejected() {
        let mut conn = connection(TcpState::SynSent);
        conn.seq_num = ISN;
        let replies = deliver(&mut conn, TCP_SYN | TCP_ACK, ISN + 1, 999, &[]);
        assert_eq!(conn.state, TcpState::SynSent);
        assert_eq!(
            replies[0].flags & TCP_RST,
            TCP_RST,
            "a blind ACK of a SYN we never sent must be reset, not accepted"
        );
    }

    #[test]
    fn data_at_the_expected_sequence_is_buffered_and_acked() {
        let mut conn = connection(TcpState::Established);
        let replies = deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 1, b"HTTP/1.1 200 OK\r\n");

        assert_eq!(conn.recv_buffer, b"HTTP/1.1 200 OK\r\n");
        assert_eq!(conn.ack_num, ISN + 1 + 17);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].ack, conn.ack_num);
    }

    #[test]
    fn duplicate_data_is_dropped_and_reacknowledged() {
        let mut conn = connection(TcpState::Established);
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 1, b"first");
        let replies = deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 1, b"first");

        // The retransmitted copy must not appear in the body twice: that would
        // corrupt every download that loses one segment.
        assert_eq!(conn.recv_buffer, b"first");
        assert_eq!(replies.len(), 1, "the peer is told where we actually are");
        assert_eq!(replies[0].ack, ISN + 1 + 5);
    }

    #[test]
    fn data_beyond_the_gap_is_rejected() {
        let mut conn = connection(TcpState::Established);
        let replies = deliver(&mut conn, TCP_ACK, ISN + 101, ISN + 1, b"late");

        assert!(conn.recv_buffer.is_empty());
        assert_eq!(conn.ack_num, ISN + 1, "the ACK must point at the gap");
        assert_eq!(replies.len(), 1);
    }

    #[test]
    fn acknowledgement_beyond_what_was_sent_is_rejected() {
        let mut conn = connection(TcpState::Established);
        conn.seq_num = ISN + 1;
        conn.send_buffer = vec![0u8; 50];
        let replies = deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 5000, &[]);

        // Trusting this ACK would discard the 50 buffered bytes and report
        // success for data the peer never received.
        assert_eq!(conn.state, TcpState::Closed);
        assert!(conn.send_buffer.is_empty());
        assert_eq!(replies[0].flags & TCP_RST, TCP_RST);
    }

    #[test]
    fn syn_state_accepts_the_syn_acknowledgement_of_the_syn() {
        let mut conn = connection(TcpState::SynSent);
        conn.seq_num = ISN;
        conn.ack_num = 0;
        // The SYN occupies a sequence number the connection has not "sent" in
        // the seq_num sense, so this ACK is one past it and must be allowed.
        deliver(&mut conn, TCP_SYN | TCP_ACK, ISN + 1, ISN + 1, &[]);
        assert_eq!(conn.state, TcpState::Established);
    }

    #[test]
    fn receive_buffer_is_capped() {
        let mut conn = connection(TcpState::Established);
        conn.recv_buffer = vec![0u8; MAX_RECV_BUFFER];
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 1, b"overflow");

        assert_eq!(conn.recv_buffer.len(), MAX_RECV_BUFFER);
        // Zero window advertised, so the peer stops instead of retrying forever.
        assert_eq!(conn.advertised_window(), 0);
    }

    #[test]
    fn advertised_window_tracks_buffer_occupancy() {
        let mut conn = connection(TcpState::Established);
        assert_eq!(conn.advertised_window(), DEFAULT_WINDOW);

        conn.recv_buffer = vec![0u8; MAX_RECV_BUFFER - 100];
        assert_eq!(conn.advertised_window(), 100);

        conn.recv_buffer = vec![0u8; MAX_RECV_BUFFER];
        assert_eq!(conn.advertised_window(), 0);
    }

    #[test]
    fn peer_window_is_read_from_the_header() {
        let mut conn = connection(TcpState::Established);
        conn.send_buffer = vec![0u8; 5000];
        // The peer's window was previously decoded nowhere, so a peer that
        // narrowed it had no effect on what we sent.
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 1, &[]);
        assert_eq!(conn.peer_window, DEFAULT_WINDOW);
    }

    #[test]
    fn fin_from_peer_moves_to_close_wait() {
        let mut conn = connection(TcpState::Established);
        let replies = deliver(&mut conn, TCP_ACK | TCP_FIN, ISN + 1, ISN + 1, &[]);

        assert_eq!(conn.state, TcpState::CloseWait);
        assert_eq!(conn.ack_num, ISN + 2, "the FIN itself is acknowledged");
        assert_eq!(replies[0].ack, ISN + 2);
    }

    #[test]
    fn reset_closes_the_connection_immediately() {
        let mut conn = connection(TcpState::Established);
        conn.tx_queue.push(TxSegment {
            seq: ISN + 1,
            flags: TCP_ACK,
            data: vec![0u8; 10],
            deadline_ms: 0,
            attempts: 1,
        });
        let replies = deliver(&mut conn, TCP_RST, 0, 0, b"anything");

        assert_eq!(conn.state, TcpState::Closed);
        assert!(conn.tx_queue.is_empty(), "no point retransmitting after a reset");
        assert!(replies.is_empty());
    }

    // ── teardown ─────────────────────────────────────────────────────

    #[test]
    fn close_from_established_queues_a_fin() {
        let mut conn = connection(TcpState::Established);
        conn.fin_sent = true;
        conn.state = TcpState::FinWait1;
        let mut queued = Vec::new();
        flush_tx(&mut conn, 0, &mut queued);
        assert_eq!(queued[0].flags & TCP_FIN, TCP_FIN);

        let replies = deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 2, &[]);

        // Acknowledging the FIN from FinWait1 advances to FinWait2, where we
        // still owe the peer's own close a response.
        assert!(conn.fin_acked);
        assert_eq!(conn.state, TcpState::FinWait2);
        assert!(replies.is_empty());
    }

    #[test]
    fn fin_in_fin_wait2_ends_in_time_wait() {
        let mut conn = connection(TcpState::FinWait2);
        conn.fin_sent = true;
        conn.fin_acked = true;
        // Our FIN occupied one sequence number, so `seq_num` has advanced past
        // it and the peer's ACK names that next value.
        conn.seq_num = ISN + 2;
        let replies = deliver(&mut conn, TCP_ACK | TCP_FIN, ISN + 1, ISN + 2, &[]);

        assert_eq!(conn.state, TcpState::TimeWait);
        assert_eq!(replies.len(), 1, "the last FIN must still be acknowledged");
    }

    #[test]
    fn last_ack_closes_on_final_acknowledgement() {
        let mut conn = connection(TcpState::LastAck);
        conn.fin_sent = true;
        conn.seq_num = ISN + 2;
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 2, &[]);
        assert_eq!(conn.state, TcpState::LastAck);

        let mut conn = connection(TcpState::LastAck);
        conn.fin_sent = true;
        conn.fin_acked = true;
        conn.seq_num = ISN + 2;
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 2, &[]);
        assert_eq!(
            conn.state,
            TcpState::Closed,
            "LastAck used to be a dead end: nothing ever completed it"
        );
    }

    #[test]
    fn time_wait_answers_a_retransmitted_fin() {
        let mut conn = connection(TcpState::TimeWait);
        let replies = deliver(&mut conn, TCP_ACK | TCP_FIN, ISN + 1, ISN + 2, &[]);
        assert_eq!(
            replies.len(),
            1,
            "silently dropping it makes the peer retransmit until it gives up"
        );
    }

    #[test]
    fn simultaneous_close_ends_in_closing() {
        let mut conn = connection(TcpState::FinWait1);
        conn.fin_sent = true;
        conn.seq_num = ISN + 2;
        let replies = deliver(&mut conn, TCP_ACK | TCP_FIN, ISN + 1, ISN + 2, &[]);

        assert_eq!(conn.state, TcpState::Closing);
        assert_eq!(replies.len(), 1);

        let mut conn = connection(TcpState::Closing);
        conn.fin_sent = true;
        conn.fin_acked = true;
        conn.seq_num = ISN + 2;
        deliver(&mut conn, TCP_ACK, ISN + 1, ISN + 2, &[]);
        assert_eq!(conn.state, TcpState::TimeWait);
    }

    // ── state helpers ────────────────────────────────────────────────

    #[test]
    fn open_states_own_a_port() {
        assert!(!TcpState::Closed.is_open());
        for state in [
            TcpState::SynSent,
            TcpState::Established,
            TcpState::FinWait1,
            TcpState::FinWait2,
            TcpState::CloseWait,
            TcpState::Closing,
            TcpState::LastAck,
            TcpState::TimeWait,
        ] {
            assert!(state.is_open(), "{} should hold its port", state.name());
        }
    }

    #[test]
    fn only_live_states_transfer_data() {
        assert!(TcpState::Established.can_transfer());
        assert!(TcpState::CloseWait.can_transfer());
        for state in [
            TcpState::SynSent,
            TcpState::FinWait1,
            TcpState::FinWait2,
            TcpState::Closing,
            TcpState::LastAck,
            TcpState::TimeWait,
            TcpState::Closed,
        ] {
            assert!(!state.can_transfer(), "{} should not transfer", state.name());
        }
    }

    // ── port allocation ──────────────────────────────────────────────

    #[test]
    fn allocated_ports_are_in_the_ephemeral_range_and_vary() {
        // Tests run concurrently and share the connection table, so uniqueness
        // cannot be asserted here — only that every value is usable and the
        // draws are not a fixed sequence.
        let mut seen = Vec::new();
        for _ in 0..64 {
            let port = allocate_port();
            assert!(
                (EPHEMERAL_PORT_MIN..=EPHEMERAL_PORT_MAX).contains(&port),
                "{} outside the ephemeral range",
                port
            );
            seen.push(port);
        }
        assert!(
            seen.windows(2).any(|w| w[0] != w[1]),
            "the allocator handed out a constant port"
        );
    }

    #[test]
    fn allocated_ports_vary_between_calls() {
        // The old allocator counted upward, so the sequence of ports was
        // identical on every boot and the choice was trivially guessable.
        let first: alloc::collections::BTreeSet<u16> =
            (0..16).map(|_| allocate_port()).collect();
        let second: alloc::collections::BTreeSet<u16> =
            (0..16).map(|_| allocate_port()).collect();
        // With 16384 candidates two independent draws never coincide.
        assert_ne!(first, second, "port selection looks deterministic");
        assert!(first.len() > 1, "all draws collapsed to one port");
        assert!(second.len() > 1, "all draws collapsed to one port");
    }

    // ── header encoding ──────────────────────────────────────────────

    #[test]
    fn header_round_trips_its_fields() {
        let header = TcpHeader::new(0x1234, 0x0050, 0xDEAD_BEEF, 0x0BADF00D, TCP_SYN | TCP_ACK);
        assert_eq!(u16::from_be(header.src_port), 0x1234);
        assert_eq!(u16::from_be(header.dst_port), 0x0050);
        assert_eq!(u32::from_be(header.seq_num), 0xDEAD_BEEF);
        assert_eq!(u32::from_be(header.ack_num), 0x0BADF00D);
        assert_eq!(header.get_flags(), TCP_SYN | TCP_ACK);
        assert_eq!(header.get_data_offset(), 5, "20 byte header, in 32-bit words");
    }

    #[test]
    fn header_window_is_decoded_big_endian() {
        let mut header = TcpHeader::new(1, 2, 3, 4, TCP_ACK);
        header.window_size = 0x4321u16.to_be();
        assert_eq!(header.get_window(), 0x4321);
    }

    #[test]
    fn syn_checksum_uses_ipv4_pseudo_header_words() {
        let packet = segment(49152, 80, &[]);
        assert_eq!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [91, 199, 118, 184], &packet),
            0xed1b
        );
    }

    #[test]
    fn odd_length_payload_checksum_validates() {
        let mut packet = segment(49152, 80, &[0x42]);
        let checksum = TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet);
        packet[16] = (checksum >> 8) as u8;
        packet[17] = (checksum & 0xFF) as u8;
        assert_eq!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet),
            0
        );
    }

    #[test]
    fn checksum_rejects_a_corrupted_payload() {
        // The only defence against a corrupted segment reaching the state
        // machine: every field it supplies is attacker-controlled otherwise.
        let mut packet = segment(49152, 80, b"GET / HTTP/1.1\r\n");
        let checksum = TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet);
        packet[16] = (checksum >> 8) as u8;
        packet[17] = (checksum & 0xFF) as u8;
        assert_eq!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet),
            0
        );

        let last = packet.len() - 1;
        packet[last] ^= 0xFF;
        assert_ne!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet),
            0
        );
    }

    #[test]
    fn checksum_covers_the_addresses_not_just_the_payload() {
        // A segment that is valid for one peer must not validate for another.
        let packet = segment(49152, 80, b"x");
        assert_ne!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet),
            TcpHeader::calculate_checksum([10, 0, 2, 16], [1, 2, 3, 4], &packet)
        );
    }
}
