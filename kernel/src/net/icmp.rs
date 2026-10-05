//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ICMP (Internet Control Message Protocol)
//!
//! Echo request and reply, plus the error messages a host needs to hear about:
//! Destination Unreachable (including Fragmentation Needed) and Time Exceeded.
//! Those were previously discarded, so every ICMP error was invisible — a send
//! that failed because a router dropped the datagram looked exactly like a send
//! that failed because nothing answered.
//!
//! All of it is attacker-influenced input, so the handling is bounded: the
//! checksum is verified before any field is read, the echoed payload of a reply
//! is capped so this host cannot be used as an amplifier, and received errors are
//! kept in a fixed-size ring.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

const ICMP_DEST_UNREACHABLE: u8 = 3;
const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_TIME_EXCEEDED: u8 = 11;
const ICMP_PARAMETER_PROBLEM: u8 = 12;
const ICMP_ECHO_REQUEST: u8 = 8;

// Destination Unreachable codes.
const ICMP_NET_UNREACHABLE: u8 = 0;
const ICMP_HOST_UNREACHABLE: u8 = 1;
const ICMP_PROTOCOL_UNREACHABLE: u8 = 2;
const ICMP_PORT_UNREACHABLE: u8 = 3;
const ICMP_FRAG_NEEDED: u8 = 4;

/// Largest payload echoed back in an echo reply.
///
/// The protocol allows echoing whatever arrived, but that lets a peer send a
/// maximal frame and have it reflected at line rate to a third party. Capping it
/// means the amplification is bounded by the cap rather than by the link.
pub const MAX_ECHO_PAYLOAD: usize = 64;

/// Errors remembered for `netstat` and the shell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IcmpError {
    NetworkUnreachable,
    HostUnreachable,
    ProtocolUnreachable,
    PortUnreachable,
    FragmentationNeeded { next_hop_mtu: u16 },
    TimeExceeded,
    ParameterProblem,
    Other(u8),
}

impl IcmpError {
    pub fn name(self) -> &'static str {
        match self {
            IcmpError::NetworkUnreachable => "network unreachable",
            IcmpError::HostUnreachable => "host unreachable",
            IcmpError::ProtocolUnreachable => "protocol unreachable",
            IcmpError::PortUnreachable => "port unreachable",
            IcmpError::FragmentationNeeded { .. } => "fragmentation needed",
            IcmpError::TimeExceeded => "time exceeded",
            IcmpError::ParameterProblem => "parameter problem",
            IcmpError::Other(_) => "other",
        }
    }
}

/// One received ICMP error, with the datagram that provoked it.
#[derive(Clone, Copy)]
pub struct ErrorReport {
    pub from_ip: [u8; 4],
    pub error: IcmpError,
    /// Transport protocol of the quoted datagram, 0 when it could not be read.
    pub quoted_protocol: u8,
    /// Destination the kernel was trying to reach.
    pub quoted_dst: [u8; 4],
    pub received_ms: u64,
}

/// Errors kept for reporting.
pub const ERROR_HISTORY: usize = 8;

#[derive(Clone)]
pub struct PingRequest {
    pub target_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub sent_time: u64,
}

#[derive(Clone)]
pub struct PingReply {
    pub source_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub rtt_ms: u64,
}

lazy_static! {
    static ref PENDING_PINGS: Mutex<BTreeMap<(u16, u16), PingRequest>> =
        Mutex::new(BTreeMap::new());
    static ref PING_REPLIES: Mutex<Vec<PingReply>> = Mutex::new(Vec::new());
    static ref ICMP_ERRORS: Mutex<Vec<ErrorReport>> = Mutex::new(Vec::new());
}

static NEXT_PING_ID: AtomicU16 = AtomicU16::new(1);

/// Echo replies dropped because the request was larger than the echo cap.
static ECHO_DROPPED: AtomicU64 = AtomicU64::new(0);
/// Errors received.
static ERRORS_RECEIVED: AtomicU64 = AtomicU64::new(0);

/// Receive-side counters, for `netstat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IcmpStats {
    pub echo_requests: u64,
    pub echo_replies: u64,
    pub errors: u64,
    /// Echo requests refused because their payload exceeded the cap.
    pub echo_dropped_oversized: u64,
    /// Echo requests refused because their checksum did not verify.
    pub echo_dropped_bad_checksum: u64,
}

/// Snapshot of the ICMP counters.
pub fn stats() -> IcmpStats {
    IcmpStats {
        echo_requests: ECHO_REQUESTS.load(Ordering::Relaxed),
        echo_replies: ECHO_REPLIES.load(Ordering::Relaxed),
        errors: ERRORS_RECEIVED.load(Ordering::Relaxed),
        echo_dropped_oversized: ECHO_DROPPED.load(Ordering::Relaxed),
        echo_dropped_bad_checksum: BAD_CHECKSUM.load(Ordering::Relaxed),
    }
}

static ECHO_REQUESTS: AtomicU64 = AtomicU64::new(0);
static ECHO_REPLIES: AtomicU64 = AtomicU64::new(0);
static BAD_CHECKSUM: AtomicU64 = AtomicU64::new(0);

/// Errors received so far, oldest first.
pub fn errors() -> Vec<ErrorReport> {
    ICMP_ERRORS.lock().clone()
}

pub fn next_identifier() -> u16 {
    NEXT_PING_ID.fetch_add(1, Ordering::Relaxed)
}

fn calculate_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;

    for i in (0..data.len()).step_by(2) {
        if i + 1 < data.len() {
            let word = ((data[i] as u32) << 8) | (data[i + 1] as u32);
            sum += word;
        } else {
            sum += (data[i] as u32) << 8;
        }
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    let result = !sum as u16;
    // RFC 1071: a computed zero is transmitted as all-ones. Without this the
    // verification below would reject a correct message whose checksum is zero.
    if result == 0 {
        0xFFFF
    } else {
        result
    }
}

/// Sum over `data` including its stored checksum; zero (or all-ones) means valid.
fn checksum_verifies(data: &[u8]) -> bool {
    let mut sum: u32 = 0;
    let mut i = 0usize;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | (data[i + 1] as u32);
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let result = sum as u16;
    result == 0 || result == 0xFFFF
}

/// Decode the quoted datagram carried inside an ICMP error.
///
/// RFC 792 puts the offending header plus its first eight bytes of payload in the
/// error. Reading it tells the user what was actually being attempted, which is
/// the whole value of receiving the error at all. The quoted header has not been
/// checksum-verified by us and must not be trusted as more than a hint.
fn parse_quoted(quoted: &[u8]) -> (u8, [u8; 4]) {
    if quoted.len() < 20 {
        return (0, [0; 4]);
    }
    if quoted[0] >> 4 != 4 {
        return (0, [0; 4]);
    }
    let protocol = quoted[9];
    let dst = [quoted[16], quoted[17], quoted[18], quoted[19]];
    (protocol, dst)
}

/// Record an ICMP error for the shell and `netstat`.
fn record_error(from_ip: [u8; 4], error: IcmpError, quoted: &[u8]) {
    let (quoted_protocol, quoted_dst) = parse_quoted(quoted);
    ERRORS_RECEIVED.fetch_add(1, Ordering::Relaxed);
    let report = ErrorReport {
        from_ip,
        error,
        quoted_protocol,
        quoted_dst,
        received_ms: crate::shell::monotonic_ms(),
    };
    crate::net_log!(
        "ICMP: {} from {}.{}.{}.{} about protocol {} -> {}.{}.{}.{}",
        error.name(),
        from_ip[0], from_ip[1], from_ip[2], from_ip[3],
        quoted_protocol,
        quoted_dst[0], quoted_dst[1], quoted_dst[2], quoted_dst[3],
    );
    let mut errors = ICMP_ERRORS.lock();
    // Fixed-size ring: an ICMP error storm must not grow this without limit.
    if errors.len() >= ERROR_HISTORY {
        errors.remove(0);
    }
    errors.push(report);
}

/// Handle an ICMP error message.
///
/// Returns the error kind for the caller to report. Unknown types are counted
/// and ignored rather than treated as fatal, since the type space is open.
fn handle_error(packet: &[u8], src_ip: [u8; 4]) {
    // Type, code, checksum, then the quoted datagram.
    let quoted = if packet.len() > 8 { &packet[8..] } else { &[][..] };
    match packet[0] {
        ICMP_DEST_UNREACHABLE => {
            let error = match packet[1] {
                ICMP_NET_UNREACHABLE => IcmpError::NetworkUnreachable,
                ICMP_HOST_UNREACHABLE => IcmpError::HostUnreachable,
                ICMP_PROTOCOL_UNREACHABLE => IcmpError::ProtocolUnreachable,
                ICMP_PORT_UNREACHABLE => IcmpError::PortUnreachable,
                ICMP_FRAG_NEEDED => {
                    // The next-hop MTU is in the low half of the fourth word of
                    // the ICMP header, which overlaps the first two bytes of the
                    // quoted datagram.
                    let mtu = if quoted.len() >= 2 {
                        u16::from_be_bytes([quoted[0], quoted[1]])
                    } else {
                        0
                    };
                    IcmpError::FragmentationNeeded { next_hop_mtu: mtu }
                }
                other => IcmpError::Other(other),
            };
            record_error(src_ip, error, quoted);
        }
        ICMP_TIME_EXCEEDED => record_error(src_ip, IcmpError::TimeExceeded, quoted),
        ICMP_PARAMETER_PROBLEM => record_error(src_ip, IcmpError::ParameterProblem, quoted),
        _ => {}
    }
}

/// Build and send an ICMP Destination Unreachable, Fragmentation Needed.
pub fn send_dest_unreachable_frag_needed(
    dst_ip: [u8; 4],
    next_hop_mtu: u16,
    identification: u16,
    offset: u8,
) -> Result<(), &'static str> {
    let mut message = alloc::vec![0u8; 12 + 28];
    message[0] = ICMP_DEST_UNREACHABLE;
    message[1] = ICMP_FRAG_NEEDED;
    // Checksum left zero for calculation.
    message[4] = 0;
    message[5] = 0;
    // Unused field.
    message[6] = 0;
    message[7] = 0;
    // Next-hop MTU, then the reserved byte and fragment offset of the quoted
    // datagram, as RFC 792 requires.
    message[8..12].copy_from_slice(&next_hop_mtu.to_be_bytes());
    let quoted = &mut message[12..];
    quoted[0] = 0x45;
    quoted[1] = 0;
    quoted[2..4].copy_from_slice(&28u16.to_be_bytes());
    quoted[4..6].copy_from_slice(&identification.to_be_bytes());
    quoted[6..8].copy_from_slice(&((offset as u16) << 3).to_be_bytes());
    quoted[8] = 64; // TTL
    quoted[9] = crate::net::ip::IP_PROTO_UDP;
    quoted[16..20].copy_from_slice(&dst_ip);

    let checksum = calculate_checksum(&message);
    message[2] = (checksum >> 8) as u8;
    message[3] = (checksum & 0xFF) as u8;

    crate::net::ip::send_packet(dst_ip, crate::net::ip::IP_PROTO_ICMP, &message)
}

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], _src_mac: [u8; 6]) {
    if packet.len() < 8 {
        return;
    }
    // Verify before reading any field: the type and code decide what happens next,
    // so a corrupt message must not be acted on.
    if !checksum_verifies(packet) {
        BAD_CHECKSUM.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let message_type = packet[0];
    if message_type == ICMP_DEST_UNREACHABLE
        || message_type == ICMP_TIME_EXCEEDED
        || message_type == ICMP_PARAMETER_PROBLEM
    {
        handle_error(packet, src_ip);
        return;
    }

    if message_type == ICMP_ECHO_REQUEST {
        ECHO_REQUESTS.fetch_add(1, Ordering::Relaxed);
        crate::net_log!(
            "Received ping from {}.{}.{}.{}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3]
        );

        // Refuse to reflect an arbitrarily large payload. Echoing whatever
        // arrived is what makes a host usable as a traffic amplifier.
        if packet.len() - 8 > MAX_ECHO_PAYLOAD {
            ECHO_DROPPED.fetch_add(1, Ordering::Relaxed);
            crate::net_log!(
                "ICMP: echo request of {} bytes exceeds the {}-byte cap, not replying",
                packet.len() - 8,
                MAX_ECHO_PAYLOAD
            );
            return;
        }

        let mut reply = packet.to_vec();
        reply[0] = ICMP_ECHO_REPLY;
        reply[2] = 0;
        reply[3] = 0;
        let checksum = calculate_checksum(&reply);
        reply[2] = (checksum >> 8) as u8;
        reply[3] = (checksum & 0xFF) as u8;
        ECHO_REPLIES.fetch_add(1, Ordering::Relaxed);

        if let Err(error) = crate::net::ip::send_packet(src_ip, 1, &reply) {
            crate::net_log!("ICMP reply send failed: {}", error);
        }

        crate::net_log!(
            "Ping reply sent to {}.{}.{}.{}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3]
        );
    } else if packet[0] == ICMP_ECHO_REPLY {
        ECHO_REPLIES.fetch_add(1, Ordering::Relaxed);
        let identifier = u16::from_be_bytes([packet[4], packet[5]]);
        let sequence = u16::from_be_bytes([packet[6], packet[7]]);

        crate::net_log!(
            "ICMP: Received echo reply from {}.{}.{}.{}, id={}, seq={}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3],
            identifier,
            sequence
        );

        let mut pending = PENDING_PINGS.lock();
        if pending
            .get(&(identifier, sequence))
            .map(|request| request.target_ip == src_ip)
            .unwrap_or(false)
        {
            let request = pending.remove(&(identifier, sequence)).unwrap();
            let current_time = crate::shell::monotonic_ms();
            let rtt_ms = current_time.saturating_sub(request.sent_time);

            crate::net_log!(
                "Received ping reply from {}.{}.{}.{} (seq={}, rtt={}ms)",
                src_ip[0],
                src_ip[1],
                src_ip[2],
                src_ip[3],
                sequence,
                rtt_ms
            );

            PING_REPLIES.lock().push(PingReply {
                source_ip: src_ip,
                identifier,
                sequence,
                rtt_ms,
            });
        }
    }
}

pub fn send_ping(dst_ip: [u8; 4], identifier: u16, sequence: u16) -> Result<(), &'static str> {
    let payload = b"MFK Ping!";
    let mut packet = alloc::vec::Vec::with_capacity(8 + payload.len());
    packet.extend_from_slice(&[ICMP_ECHO_REQUEST, 0, 0, 0]);
    packet.extend_from_slice(&identifier.to_be_bytes());
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(payload);

    let checksum = calculate_checksum(&packet);
    packet[2] = (checksum >> 8) as u8;
    packet[3] = (checksum & 0xFF) as u8;

    // Record the pending request
    let current_time = crate::shell::monotonic_ms();
    PENDING_PINGS.lock().insert(
        (identifier, sequence),
        PingRequest {
            target_ip: dst_ip,
            identifier,
            sequence,
            sent_time: current_time,
        },
    );

    crate::net_log!(
        "ICMP: Sending ping to {}.{}.{}.{}, seq={}",
        dst_ip[0],
        dst_ip[1],
        dst_ip[2],
        dst_ip[3],
        sequence
    );

    match crate::net::ip::send_packet(dst_ip, 1, &packet) {
        Ok(()) => Ok(()),
        Err(e) => {
            PENDING_PINGS.lock().remove(&(identifier, sequence));
            Err(e)
        }
    }
}

pub fn clear_pending(identifier: u16) {
    PENDING_PINGS.lock().retain(|&(id, _), _| id != identifier);
    PING_REPLIES
        .lock()
        .retain(|reply| reply.identifier != identifier);
}

pub fn get_pending_count() -> usize {
    PENDING_PINGS.lock().len()
}

pub fn check_timeouts() -> usize {
    let current_time = crate::shell::monotonic_ms();
    let timeout_ms = 5000; // 5 second timeout

    let mut pending = PENDING_PINGS.lock();
    let mut timed_out = alloc::vec::Vec::new();

    for ((id, seq), req) in pending.iter() {
        if current_time.saturating_sub(req.sent_time) >= timeout_ms {
            timed_out.push((*id, *seq));
        }
    }

    for key in &timed_out {
        pending.remove(key);
    }

    timed_out.len()
}

pub fn pop_reply() -> Option<PingReply> {
    let mut replies = PING_REPLIES.lock();
    if replies.is_empty() {
        None
    } else {
        Some(replies.remove(0))
    }
}

pub fn pop_reply_for(identifier: u16) -> Option<PingReply> {
    let mut replies = PING_REPLIES.lock();
    let index = replies
        .iter()
        .position(|reply| reply.identifier == identifier)?;
    Some(replies.remove(index))
}
