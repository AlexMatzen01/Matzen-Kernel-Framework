//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! IPv4 protocol implementation

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_UDP: u8 = 17;
pub const IP_PROTO_TCP: u8 = 6;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IpHeader {
    pub version_ihl: u8, // Version (4 bits) + IHL (4 bits)
    pub dscp_ecn: u8,    // DSCP (6 bits) + ECN (2 bits)
    pub total_length: u16,
    pub identification: u16,
    pub flags_fragment: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src_ip: [u8; 4],
    pub dst_ip: [u8; 4],
}

lazy_static! {
    static ref NETWORK_CONFIG: Mutex<NetworkConfig> = Mutex::new(NetworkConfig::default());
}

/// IPv4 interface settings. A zero mask disables subnet routing; without a
/// gateway, only directly resolved hosts are reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkConfig {
    pub address: Option<[u8; 4]>,
    pub netmask: [u8; 4],
    pub gateway: Option<[u8; 4]>,
}

impl NetworkConfig {
    pub const fn default() -> Self {
        Self {
            address: None,
            netmask: [255, 255, 255, 0],
            gateway: None,
        }
    }
}

/// Choose the on-link destination or gateway for a destination address.
pub fn next_hop(config: NetworkConfig, destination: [u8; 4]) -> Option<[u8; 4]> {
    let address = config.address?;
    if config.netmask == [0, 0, 0, 0] {
        return config.gateway;
    }
    let on_link =
        (0..4).all(|i| (address[i] & config.netmask[i]) == (destination[i] & config.netmask[i]));
    if on_link {
        Some(destination)
    } else {
        config.gateway
    }
}

pub fn network_config() -> NetworkConfig {
    *NETWORK_CONFIG.lock()
}

pub fn configure(address: [u8; 4], netmask: [u8; 4], gateway: Option<[u8; 4]>) {
    let previous = NETWORK_CONFIG.lock().address;
    *NETWORK_CONFIG.lock() = NetworkConfig {
        address: Some(address),
        netmask,
        gateway,
    };
    // Re-addressing invalidates every learned mapping: the neighbours of the old
    // network are not the neighbours of the new one, and keeping the entries
    // would route the first packets of every new connection through whatever the
    // previous network's last neighbour was.
    if previous != Some(address) {
        crate::net::arp::flush();
    }
    crate::serial_println!(
        "IPv4 configured: {}.{}.{}.{} mask {}.{}.{}.{} gateway {}",
        address[0],
        address[1],
        address[2],
        address[3],
        netmask[0],
        netmask[1],
        netmask[2],
        netmask[3],
        match gateway {
            Some(g) => alloc::format!("{}.{}.{}.{}", g[0], g[1], g[2], g[3]),
            None => alloc::string::String::from("(none)"),
        }
    );
}

pub fn set_ip_address(ip: [u8; 4]) {
    let mut cfg = NETWORK_CONFIG.lock();
    cfg.address = Some(ip);
    crate::serial_println!("IP address set to {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
}

pub fn get_ip_address() -> Option<[u8; 4]> {
    NETWORK_CONFIG.lock().address
}

impl IpHeader {
    /// One's-complement sum over `bytes` with the checksum field at offset
    /// 10..12 treated as zero.
    ///
    /// This computes a checksum to store: call it on a header whose checksum
    /// field is still zero. To *verify* a received header use
    /// [`verify_checksum_bytes`], which must sum the stored field in.
    ///
    /// Public because the DHCP client builds an IPv4 header itself: it has no
    /// address yet, so it cannot go through `send_packet`, and a second copy of
    /// this loop is a second place for a checksum bug to hide.
    pub fn checksum_bytes(bytes: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            if i == 10 {
                // Skip the checksum field itself.
                i += 2;
                continue;
            }
            let word = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
            sum += word;
            i += 2;
        }
        if i < bytes.len() {
            // Odd length: the final byte is padded on the right.
            sum += (bytes[i] as u32) << 8;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        let result = !sum as u16;
        // RFC 1071: a computed zero is transmitted as all-ones. Storing 0x0000
        // would verify as 0xFFFF instead of 0, so the two forms would disagree.
        if result == 0 {
            0xFFFF
        } else {
            result
        }
    }

    /// Sum over `bytes` including the stored checksum.
    ///
    /// Returns zero for a header whose stored checksum is correct, because
    /// summing the complement of a sum plus the sum itself wraps to zero.
    ///
    /// A result of `0xFFFF` also means correct, for a sender that stored the
    /// all-ones form of a zero checksum instead of normalising it. A reader
    /// should accept either; a writer should normalise, which
    /// [`checksum_bytes`] does.
    pub fn verify_checksum_bytes(bytes: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            let word = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
            sum += word;
            i += 2;
        }
        if i < bytes.len() {
            sum += (bytes[i] as u32) << 8;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        sum as u16
    }

    pub fn new(src_ip: [u8; 4], dst_ip: [u8; 4], protocol: u8, payload_len: u16) -> Self {
        let total_len = 20 + payload_len; // 20 bytes header + payload

        let mut header = IpHeader {
            version_ihl: 0x45, // Version 4, IHL 5 (20 bytes)
            dscp_ecn: 0,
            total_length: total_len.to_be(),
            identification: 0,
            flags_fragment: 0,
            ttl: 64,
            protocol,
            checksum: 0,
            src_ip,
            dst_ip,
        };

        header.checksum = header.calculate_checksum().to_be();
        header
    }

    fn calculate_checksum(&self) -> u16 {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                self as *const IpHeader as *const u8,
                core::mem::size_of::<IpHeader>(),
            )
        };
        IpHeader::checksum_bytes(bytes)
    }

    pub fn get_total_length(&self) -> u16 {
        u16::from_be(self.total_length)
    }

    pub fn get_ihl(&self) -> u8 {
        (self.version_ihl & 0x0F) * 4
    }
}

/// Whether a datagram addressed to `dst` is for this host.
///
/// A host must accept three destinations, not one: its own address, the limited
/// broadcast, and "this network". RFC 1122 requires the latter two, and a
/// DHCP client needs them in particular — a server replies to a client that
/// already holds a lease with the OFFER or ACK addressed to 255.255.255.255, and
/// a stack that filters on its own address alone silently drops every renewal
/// after the first lease.
pub fn destination_is_accepted(our_ip: Option<[u8; 4]>, dst: [u8; 4]) -> bool {
    if dst == [255, 255, 255, 255] || dst == [0, 0, 0, 0] {
        return true;
    }
    match our_ip {
        Some(ours) => dst == ours,
        // With no address there is nothing to match against; accept and let the
        // upper layers decide, which is what makes the pre-lease DHCP exchange
        // possible at all.
        None => true,
    }
}

/// The subnet broadcast address for a network under `netmask`.
pub fn subnet_broadcast(address: [u8; 4], netmask: [u8; 4]) -> [u8; 4] {
    [
        address[0] | !netmask[0],
        address[1] | !netmask[1],
        address[2] | !netmask[2],
        address[3] | !netmask[3],
    ]
}

/// Whether an IPv4 header's stored checksum is correct.
///
/// Zero is the natural result for a correct header, and `0xFFFF` is also correct
/// for a sender that stored the all-ones form of a zero checksum rather than
/// normalising it, so both are accepted. `IpHeader::checksum_bytes` normalises on
/// the way out.
pub fn checksum_valid(header: &[u8]) -> bool {
    if header.len() < 20 {
        return false;
    }
    let sum = IpHeader::verify_checksum_bytes(&header[..20]);
    sum == 0 || sum == 0xFFFF
}

/// Packets discarded because the IPv4 header checksum did not verify.
static STATS_BAD_CHECKSUM: AtomicU64 = AtomicU64::new(0);

/// Packets discarded as malformed before reaching an upper layer.
static STATS_MALFORMED: AtomicU64 = AtomicU64::new(0);
static STATS_REASSEMBLED: AtomicU64 = AtomicU64::new(0);
static STATS_FRAGMENTED_SENT: AtomicU64 = AtomicU64::new(0);
static STATS_FRAGMENTS_SENT: AtomicU64 = AtomicU64::new(0);
static STATS_NO_ROUTE: AtomicU64 = AtomicU64::new(0);
static STATS_TOO_LARGE: AtomicU64 = AtomicU64::new(0);

/// Receive-side counters, for `netstat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpStats {
    pub bad_checksum: u64,
    pub malformed: u64,
    pub fragments_reassembled: u64,
    pub fragmented_sent: u64,
    pub fragments_sent: u64,
    pub dropped_no_route: u64,
    pub too_large: u64,
}

/// Snapshot of the IP counters.
pub fn stats() -> IpStats {
    IpStats {
        bad_checksum: STATS_BAD_CHECKSUM.load(Ordering::Relaxed),
        malformed: STATS_MALFORMED.load(Ordering::Relaxed),
        fragments_reassembled: STATS_REASSEMBLED.load(Ordering::Relaxed),
        fragmented_sent: STATS_FRAGMENTED_SENT.load(Ordering::Relaxed),
        fragments_sent: STATS_FRAGMENTS_SENT.load(Ordering::Relaxed),
        dropped_no_route: STATS_NO_ROUTE.load(Ordering::Relaxed),
        too_large: STATS_TOO_LARGE.load(Ordering::Relaxed),
    }
}

pub fn process_packet(packet: &[u8], src_mac: [u8; 6]) {
    if packet.len() < 20 {
        return;
    }

    let ip_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const IpHeader) };

    if ip_header.version_ihl >> 4 != 4 {
        return;
    }

    let header_len = ip_header.get_ihl() as usize;
    // The header length must be checked before the checksum, because the
    // checksum covers however many words IHL claims.
    if header_len < core::mem::size_of::<IpHeader>() || packet.len() < header_len {
        return;
    }
    // A corrupt header must not be trusted: every field read below, including
    // the protocol number and both addresses, is used to decide what the packet
    // is and where it goes. Verify before any of it is acted on.
    if !checksum_valid(&packet[..header_len]) {
        STATS_BAD_CHECKSUM.fetch_add(1, Ordering::Relaxed);
        crate::net_log!(
            "IP: header checksum invalid, discarding {} byte packet from {}.{}.{}.{}",
            packet.len(),
            ip_header.src_ip[0],
            ip_header.src_ip[1],
            ip_header.src_ip[2],
            ip_header.src_ip[3]
        );
        return;
    }

    crate::net_log!(
        "IP: Received packet from {}.{}.{}.{} to {}.{}.{}.{}, protocol={}",
        ip_header.src_ip[0],
        ip_header.src_ip[1],
        ip_header.src_ip[2],
        ip_header.src_ip[3],
        ip_header.dst_ip[0],
        ip_header.dst_ip[1],
        ip_header.dst_ip[2],
        ip_header.dst_ip[3],
        ip_header.protocol
    );

    // Check if packet is for us. Broadcast and "this network" are accepted too:
    // see `destination_is_accepted`, which explains why that matters to DHCP.
    if !destination_is_accepted(get_ip_address(), ip_header.dst_ip) {
        crate::net_log!(
            "IP: Packet to {}.{}.{}.{} is not for us (our IP: {}.{}.{}.{}), discarding",
            ip_header.dst_ip[0],
            ip_header.dst_ip[1],
            ip_header.dst_ip[2],
            ip_header.dst_ip[3],
            get_ip_address().map(|ip| ip[0]).unwrap_or(0),
            get_ip_address().map(|ip| ip[1]).unwrap_or(0),
            get_ip_address().map(|ip| ip[2]).unwrap_or(0),
            get_ip_address().map(|ip| ip[3]).unwrap_or(0)
        );
        return;
    }

    let total_len = ip_header.get_total_length() as usize;
    if total_len < header_len || packet.len() < total_len {
        return;
    }

    // Ethernet pads short frames to its minimum frame size. Do not pass that
    // padding to ICMP/UDP/TCP as part of the IPv4 payload.
    let payload = &packet[header_len..total_len];

    // A fragment carries only part of a datagram. It cannot be handed to an upper
    // layer until every piece has arrived; the last piece to land completes it.
    if is_fragment(u16::from_be(ip_header.flags_fragment)) {
        match reassemble(ip_header, payload, now_ms()) {
            Reassembly::Incomplete => return,
            Reassembly::Complete { src_ip, dst_ip, protocol, data } => {
                STATS_REASSEMBLED.fetch_add(1, Ordering::Relaxed);
                crate::net_log!(
                    "IP: reassembled {} byte datagram from {}.{}.{}.{}",
                    data.len(),
                    src_ip[0],
                    src_ip[1],
                    src_ip[2],
                    src_ip[3]
                );
                deliver(src_ip, dst_ip, protocol, &data, src_mac);
                return;
            }
            Reassembly::Dropped(reason) => {
                STATS_MALFORMED.fetch_add(1, Ordering::Relaxed);
                crate::net_log!("IP: fragment discarded: {}", reason);
                return;
            }
        }
    }

    deliver(
        ip_header.src_ip,
        ip_header.dst_ip,
        ip_header.protocol,
        payload,
        src_mac,
    );
}

/// Hand a whole datagram to the protocol that owns it.
fn deliver(src_ip: [u8; 4], dst_ip: [u8; 4], protocol: u8, payload: &[u8], src_mac: [u8; 6]) {
    match protocol {
        IP_PROTO_ICMP => {
            crate::net::icmp::process_packet(payload, src_ip, src_mac);
        }
        IP_PROTO_UDP => {
            crate::net::udp::process_packet(payload, src_ip, dst_ip);
        }
        IP_PROTO_TCP => {
            crate::net::tcp::process_packet(payload, src_ip, src_mac);
        }
        _ => {
            crate::net_log!("IP: Unsupported protocol {}", protocol);
        }
    }
}

/// Largest IPv4 datagram this link will carry.
///
/// 1500 is the Ethernet MTU. The E1000's own receive buffer is 2048 bytes, which
/// is comfortably above this, so the limit is the link and not the driver.
pub const MTU: usize = 1500;

/// Largest IPv4 payload that fits in one frame.
pub const MAX_PAYLOAD: usize = MTU - 20;

/// Flag: do not fragment. When set, an oversize datagram is refused rather than
/// split, and the peer answers with ICMP Fragmentation Needed.
pub const FLAG_DONT_FRAGMENT: u16 = 0x4000;
/// Flag: more fragments follow.
pub const FLAG_MORE_FRAGMENTS: u16 = 0x2000;
/// Mask for the fragment offset field of the flags/fragment word.
///
/// RFC 791 draws this word as three flag bits followed by thirteen offset bits:
/// bit 15 is reserved, bit 14 is DF, bit 13 is MF, and bits 12..0 hold the
/// fragment offset *in 8-byte units*. The offset occupies the low bits
/// directly, so it is masked and multiplied, never shifted — treating it as
/// shifted reads a wholly different position for every fragment.
const FRAGMENT_OFFSET_MASK: u16 = 0x1FFF;

/// Whether a header describes a fragment rather than a whole datagram.
///
/// Takes the flags word in *host* order, as produced by `u16::from_be` on the
/// wire field. The struct stores raw wire bytes, so callers must convert.
pub fn is_fragment(flags_fragment: u16) -> bool {
    flags_fragment & (FLAG_MORE_FRAGMENTS | FRAGMENT_OFFSET_MASK) != 0
}

/// Byte offset of a fragment within the original datagram.
///
/// Host order, as for [`is_fragment`]. The largest expressible offset is
/// 65528 bytes.
pub fn fragment_offset_bytes(flags_fragment: u16) -> usize {
    (flags_fragment & FRAGMENT_OFFSET_MASK) as usize * 8
}

/// Header for one fragment.
///
/// `offset_bytes` must be a multiple of 8, since the field counts 8-byte units.
pub fn fragment_header(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    protocol: u8,
    identification: u16,
    offset_bytes: usize,
    more_fragments: bool,
    payload_len: usize,
) -> IpHeader {
    debug_assert!(offset_bytes % 8 == 0, "offsets are counted in 8-byte units");
    let mut header = IpHeader::new(src_ip, dst_ip, protocol, payload_len as u16);
    header.identification = identification.to_be();
    // The offset field sits at bits 3..=12 and counts 8-byte units; MF is bit 13.
    // Stored as raw wire bytes, like every other field in this struct, so the
    // value is byte-swapped here and read back with `u16::from_be`.
    header.flags_fragment = ((offset_bytes / 8) as u16
        | if more_fragments { FLAG_MORE_FRAGMENTS } else { 0 })
        .to_be();
    header.checksum = 0;
    header.checksum = header.calculate_checksum().to_be();
    header
}

/// Maximum reassembly slots held at once.
///
/// A peer that sends fragment headers without ever completing them must not be
/// able to grow this without limit; each slot holds a partial datagram.
pub const MAX_REASSEMBLY: usize = 8;

/// Largest datagram the reassembler reconstructs: the protocol maximum.
pub const MAX_REASSEMBLY_BYTES: usize = 65535;

/// How long a partial datagram waits for its remaining pieces.
pub const REASSEMBLY_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone)]
struct ReassemblySlot {
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    protocol: u8,
    identification: u16,
    /// Bytes received so far, indexed by offset within the datagram.
    data: Vec<u8>,
    /// Which parts of `data` have been filled, so a gap is detectable.
    received: Vec<bool>,
    /// Highest offset seen plus one: how far the datagram is known to reach.
    filled_until: usize,
    last_seen_ms: u64,
}

lazy_static! {
    static ref REASSEMBLY: Mutex<Vec<ReassemblySlot>> = Mutex::new(Vec::new());
}

fn now_ms() -> u64 {
    crate::shell::monotonic_ms()
}

fn evict_expired_reassembly(now: u64) {
    REASSEMBLY
        .lock()
        .retain(|slot| now.saturating_sub(slot.last_seen_ms) < REASSEMBLY_TIMEOUT_MS);
}

/// Outcome of offering one fragment to the reassembler.
#[derive(Debug)]
enum Reassembly {
    /// More pieces are needed.
    Incomplete,
    /// Every piece arrived; here is the datagram.
    Complete {
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
        protocol: u8,
        data: Vec<u8>,
    },
    /// The set was discarded, with the reason.
    Dropped(&'static str),
}

/// Key identifying one fragmented datagram.
///
/// The source address is deliberately part of the key: a fragment from a
/// different sender carrying the same identification must not be able to join an
/// existing set, because that is how an attacker splices their own bytes into
/// someone else's datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DatagramKey {
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    protocol: u8,
    identification: u16,
}

/// The kernel's own address, or all-ones when none is configured.
///
/// A pre-lease host still receives broadcast traffic, so the destination is
/// needed before an address exists.
fn local_address() -> [u8; 4] {
    get_ip_address().unwrap_or([255, 255, 255, 255])
}

/// Add one fragment to the reassembly table.
fn reassemble(header: IpHeader, payload: &[u8], now: u64) -> Reassembly {
    // The struct holds raw wire bytes, so convert before interpreting the fields.
    let flags = u16::from_be(header.flags_fragment);
    let offset = fragment_offset_bytes(flags);
    let more = flags & FLAG_MORE_FRAGMENTS != 0;

    // A non-final fragment must end on an 8-byte boundary, or the pieces cannot
    // tile the datagram. That is a property of the *end* of this fragment, which
    // is what the next fragment's offset must line up with.
    if more && (offset + payload.len()) % 8 != 0 {
        return Reassembly::Dropped("non-final fragment does not end on an 8-byte boundary");
    }
    if payload.is_empty() {
        return Reassembly::Dropped("empty fragment");
    }
    if offset + payload.len() > MAX_REASSEMBLY_BYTES {
        return Reassembly::Dropped("fragment extends past the maximum datagram size");
    }

    evict_expired_reassembly(now);
    let mut table = REASSEMBLY.lock();

    // Source, destination, protocol and identification together identify a
    // datagram. A fragment from a different source carrying the same id must not
    // join this set: that is how an attacker splices their own bytes into
    // someone else's datagram.
    let position = table.iter().position(|slot| {
        slot.src_ip == header.src_ip
            && slot.dst_ip == header.dst_ip
            && slot.protocol == header.protocol
            && slot.identification == u16::from_be(header.identification)
    });

    let index = match position {
        Some(index) => {
            table[index].last_seen_ms = now;
            index
        }
        None => {
            if table.len() >= MAX_REASSEMBLY {
                // Bounded: evict the oldest rather than growing. A peer opening
                // more sets than this has them dropped, which it cannot
                // distinguish from loss.
                if let Some(oldest) = table
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.last_seen_ms)
                    .map(|(index, _)| index)
                {
                    table.remove(oldest);
                }
            }
            let end = offset + payload.len();
            table.push(ReassemblySlot {
                src_ip: header.src_ip,
                dst_ip: header.dst_ip,
                protocol: header.protocol,
                identification: u16::from_be(header.identification),
                data: alloc::vec![0u8; end],
                received: alloc::vec![false; end],
                filled_until: 0,
                last_seen_ms: now,
            });
            table.len() - 1
        }
    };

    let slot = &mut table[index];
    if offset + payload.len() > slot.data.len() {
        slot.data.resize(offset + payload.len(), 0);
        slot.received.resize(offset + payload.len(), false);
    }

    // Overlapping fragments are permitted by RFC 791, and a retransmitted
    // fragment is the common case. Copy only bytes that are new, and accept an
    // overlap only where the bytes agree: a disagreeing overlap is a deliberate
    // attempt to put a second payload through the reassembler.
    for (position, byte) in payload.iter().enumerate() {
        let target = offset + position;
        match slot.received[target] {
            true if slot.data[target] != *byte => {
                let id = slot.identification;
                table.remove(index);
                crate::net_log!(
                    "IP: conflicting overlapping fragments for id {:#x}, discarding the set",
                    id
                );
                return Reassembly::Dropped("conflicting overlap");
            }
            false => {
                slot.data[target] = *byte;
                slot.received[target] = true;
            }
            true => {}
        }
    }
    slot.filled_until = slot.filled_until.max(offset + payload.len());

    if more {
        return Reassembly::Incomplete;
    }

    // Final fragment: everything below its end must have arrived, with no gap.
    if slot.filled_until != slot.data.len() || slot.received.iter().any(|filled| !filled) {
        return Reassembly::Incomplete;
    }

// `index` came from `table.len() - 1` or from a lookup, and nothing has
    // removed a slot since, so this cannot fail. An empty fallback keeps the
    // compiler happy without a panic path in the common case.
    let Some(completed) = table.get(index).cloned() else {
        return Reassembly::Incomplete;
    };
    table.remove(index);
    Reassembly::Complete {
        src_ip: completed.src_ip,
        dst_ip: completed.dst_ip,
        protocol: completed.protocol,
        data: completed.data,
    }
}

/// Drop partial datagrams, for the shell.
pub fn clear_reassembly() {
    REASSEMBLY.lock().clear();
}

/// How many partial datagrams are being held.
pub fn reassembly_count() -> usize {
    REASSEMBLY.lock().len()
}

/// Send ICMP Destination Unreachable, Fragmentation Needed, with the next-hop MTU.
///
/// This is how a sender learns the path MTU instead of transmitting something
/// that cannot arrive.
pub fn send_fragmentation_needed(dst_ip: [u8; 4], next_hop_mtu: u16) -> Result<(), &'static str> {
    crate::net::icmp::send_dest_unreachable_frag_needed(dst_ip, next_hop_mtu, 0, 0)
}

pub fn send_packet(dst_ip: [u8; 4], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    send_packet_with_flags(dst_ip, protocol, payload, 0)
}

/// Send a datagram, fragmenting it if it does not fit the link MTU.
///
/// Fragmentation was absent: a payload over the MTU was handed to the NIC, which
/// refused anything past its 2048-byte buffer, so the failure surfaced as
/// "Packet too large" from the driver for a packet that was in fact perfectly
/// legal. TCP avoids this by segmenting to 1400 bytes, which is why it went
/// unnoticed; a UDP datagram larger than the MTU simply could not be sent.
pub fn send_packet_with_flags(
    dst_ip: [u8; 4],
    protocol: u8,
    payload: &[u8],
    flags_fragment: u16,
) -> Result<(), &'static str> {
    let src_ip = get_ip_address().ok_or("No IP address configured")?;
    if payload.len() > u16::MAX as usize - core::mem::size_of::<IpHeader>() {
        STATS_TOO_LARGE.fetch_add(1, Ordering::Relaxed);
        return Err("IPv4 packet too large");
    }

    let cfg = network_config();
    let Some(next) = next_hop(cfg, dst_ip) else {
        STATS_NO_ROUTE.fetch_add(1, Ordering::Relaxed);
        return Err("No route to host (configure a gateway with ifconfig)".into());
    };
    let dst_mac = crate::net::arp::resolve(next, 2000)?;

    if payload.len() <= MAX_PAYLOAD {
        let ip_header = IpHeader::new(src_ip, dst_ip, protocol, payload.len() as u16);
        let mut packet = alloc::vec::Vec::with_capacity(20 + payload.len());
        unsafe {
            let header_bytes = core::slice::from_raw_parts(
                &ip_header as *const IpHeader as *const u8,
                core::mem::size_of::<IpHeader>(),
            );
            packet.extend_from_slice(header_bytes);
        }
        packet.extend_from_slice(payload);
        return crate::net::ethernet::send_frame(
            dst_mac,
            crate::net::ethernet::ETHERTYPE_IP,
            &packet,
        );
    }

    if flags_fragment & FLAG_DONT_FRAGMENT != 0 {
        // The caller asked not to fragment. Refusing is correct: silently
        // fragmenting would violate the request, and silently truncating would
        // corrupt the datagram.
        STATS_TOO_LARGE.fetch_add(1, Ordering::Relaxed);
        return Err("datagram exceeds MTU and DF is set");
    }

    // Fragment. Every datagram carries one identification value so the receiver
    // can group its pieces, taken from the same entropy source as a TCP ISN.
    let identification = (crate::entropy::u16()).to_be();
    let mut offset = 0usize;
    let mut sent = 0u64;
    // The final fragment carries the remainder, which may be shorter than
    // 8 bytes; every other fragment is rounded down to a multiple of 8 so the
    // offsets line up.
    while offset < payload.len() {
        let remaining = payload.len() - offset;
        let mut chunk_len = core::cmp::min(MAX_PAYLOAD, remaining);
        if remaining > MAX_PAYLOAD {
            chunk_len -= chunk_len % 8;
        }
        let more = remaining > chunk_len;

        let ip_header = fragment_header(
            src_ip,
            dst_ip,
            protocol,
            identification,
            offset,
            more,
            chunk_len,
        );
        let mut packet = alloc::vec::Vec::with_capacity(20 + chunk_len);
        unsafe {
            let header_bytes = core::slice::from_raw_parts(
                &ip_header as *const IpHeader as *const u8,
                core::mem::size_of::<IpHeader>(),
            );
            packet.extend_from_slice(header_bytes);
        }
        packet.extend_from_slice(&payload[offset..offset + chunk_len]);

        crate::net::ethernet::send_frame(
            dst_mac,
            crate::net::ethernet::ETHERTYPE_IP,
            &packet,
        )?;
        sent += 1;
        offset += chunk_len;
    }

    STATS_FRAGMENTED_SENT.fetch_add(1, Ordering::Relaxed);
    STATS_FRAGMENTS_SENT.fetch_add(sent, Ordering::Relaxed);
    crate::net_log!("IP: fragmented {} byte datagram into {} pieces", payload.len(), sent);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        checksum_valid, clear_reassembly, destination_is_accepted, fragment_header,
        fragment_offset_bytes, is_fragment, next_hop, reassemble, reassembly_count,
        subnet_broadcast, IpHeader, NetworkConfig, Reassembly, FLAG_MORE_FRAGMENTS,
        FRAGMENT_OFFSET_MASK, MAX_REASSEMBLY, MAX_REASSEMBLY_BYTES, MTU, MAX_PAYLOAD,
        REASSEMBLY_TIMEOUT_MS,
    };
    use spin::Mutex;

    const SRC: [u8; 4] = [10, 0, 2, 15];
const DST: [u8; 4] = [10, 0, 2, 2];

/// Serialises the reassembly tests.
    ///
    /// The reassembly table is a process-wide singleton, and the test harness runs
    /// tests on several threads, so without this one test's fragments land in
    /// another's set. `clear_reassembly` alone is not enough: the clear and the
    /// fragments must be atomic with respect to other tests.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Run `body` with exclusive use of the reassembly table.
    fn alone<R>(body: impl FnOnce() -> R) -> R {
        let _guard = TEST_LOCK.lock();
        clear_reassembly();
        body()
    }

    /// Offer one fragment to the reassembler, given the fields that identify it.
    ///
    /// The flags word is built directly rather than through `fragment_header`
    /// because that helper takes a byte offset and asserts it is 8-aligned, which
    /// is exactly what some of these cases are not.
    fn piece(identification: u16, offset_bytes: usize, more: bool, payload: &[u8]) -> Reassembly {
        let mut header = IpHeader::new(SRC, DST, 17, payload.len() as u16);
        header.identification = identification.to_be();
        header.flags_fragment = ((offset_bytes / 8) as u16 | if more { FLAG_MORE_FRAGMENTS } else { 0 })
            .to_be();
        header.checksum = 0;
        header.checksum = header.calculate_checksum().to_be();
        reassemble(header, payload, 1_000)
    }

    // ── header checksum ─────────────────────────────────────────────

    fn header_bytes(identification: u16, flags: u16) -> [u8; 20] {
        let mut header = IpHeader::new(SRC, DST, 17, 4);
        header.identification = identification.to_be();
        header.flags_fragment = flags.to_be();
        header.checksum = 0;
        header.checksum = header.calculate_checksum().to_be();
        let mut out = [0u8; 20];
        unsafe {
            out.copy_from_slice(core::slice::from_raw_parts(
                &header as *const IpHeader as *const u8,
                20,
            ));
        }
        out
    }

#[test]
    fn a_correct_header_checksum_validates() {
        assert!(checksum_valid(&header_bytes(0x1234, 0)));
    }

    #[test]
    fn a_single_flipped_bit_fails_the_header_checksum() {
        // Every field in the header decides what the packet is and where it goes,
        // so a corrupt header must not be acted on. This includes the checksum
        // field itself.
        for offset in 0..20usize {
            let mut header = header_bytes(0x1234, 0);
            header[offset] ^= 0x01;
            assert!(!checksum_valid(&header), "bit {} accepted", offset);
        }
    }

    #[test]
    fn an_all_ones_stored_checksum_is_accepted() {
        // RFC 1071: a sender that stores the all-ones form of a zero checksum
        // rather than normalising it is still correct. The way to reach that
        // state is to *compute* the checksum and store the result without the
        // normalisation this implementation applies, which is what a naive peer
        // does.
        let mut header = header_bytes(0x1234, 0);
        // Undo our normalisation for the case where the computed value was zero.
        let mut unnormalised = header;
        unnormalised[10] = 0;
        unnormalised[11] = 0;
        let computed = IpHeader::checksum_bytes(&unnormalised);
        unnormalised[10] = (computed >> 8) as u8;
        unnormalised[11] = (computed & 0xFF) as u8;
        // Whatever it computed, the stored value must verify.
        assert!(checksum_valid(&unnormalised));
        assert!(checksum_valid(&header));
    }

    #[test]
    fn a_truncated_header_is_never_valid() {
        assert!(!checksum_valid(&[0u8; 19]));
    }

    // ── fragment header encoding ─────────────────────────────────────

    #[test]
    fn fragment_headers_encode_offset_and_more_flag() {
        let header = fragment_header(SRC, DST, 17, 0xBEEF, 1480, true, 100);
        assert_eq!(u16::from_be(header.flags_fragment) & FLAG_MORE_FRAGMENTS, FLAG_MORE_FRAGMENTS);
        assert_eq!(fragment_offset_bytes(u16::from_be(header.flags_fragment)), 1480);
        assert_eq!(u16::from_be(header.identification), 0xBEEF);
        assert!(is_fragment(u16::from_be(header.flags_fragment)));
        assert!(
            !is_fragment(0),
            "a whole datagram must not be treated as a fragment"
        );
    }

    #[test]
    fn a_final_fragment_has_no_more_flag_but_is_still_a_fragment() {
        let header = fragment_header(SRC, DST, 17, 1, 1480, false, 20);
        let flags = u16::from_be(header.flags_fragment);
        assert_eq!(flags & FLAG_MORE_FRAGMENTS, 0);
        assert_eq!(fragment_offset_bytes(flags), 1480);
        assert!(
            is_fragment(flags),
            "a last fragment still needs reassembly"
        );
    }

    #[test]
    fn every_fragment_header_carries_a_valid_checksum() {
        for offset in [0usize, 8, 1480, 2960] {
            for more in [true, false] {
                let header = fragment_header(SRC, DST, 17, 0x4242, offset, more, 64);
                let mut out = [0u8; 20];
                unsafe {
                    out.copy_from_slice(core::slice::from_raw_parts(
                        &header as *const IpHeader as *const u8,
                        20,
                    ));
                }
                assert!(checksum_valid(&out), "offset {} more {}", offset, more);
            }
        }
    }

    // ── reassembly ───────────────────────────────────────────────────

    #[test]
    fn two_fragments_reassemble_into_the_original_bytes() {
        alone(|| {
            let body: Vec<u8> = (0..200u16).map(|i| i as u8).collect();
            // The split must be 8-aligned: a non-final fragment has to end on an
            // 8-byte boundary, which is what lets the next fragment's offset
            // line up.
            let split = 104;
            assert!(matches!(
                piece(1, 0, true, &body[..split]),
                Reassembly::Incomplete
            ));
            match piece(1, split, false, &body[split..]) {
                Reassembly::Complete { data, src_ip, protocol, .. } => {
                    assert_eq!(data, body);
                    assert_eq!(src_ip, SRC);
                    assert_eq!(protocol, 17);
                }
                other => panic!("expected a complete datagram, got {:?}", other),
            }
            assert_eq!(reassembly_count(), 0, "the slot must be released");
        })
    }

    #[test]
    fn a_missing_middle_fragment_never_completes() {
        alone(|| {
            // Three fragments, the middle one absent.
            assert!(matches!(
                piece(2, 0, true, &[0u8; 8]),
                Reassembly::Incomplete
            ));
            assert!(matches!(
                piece(2, 24, false, &[0u8; 8]),
                Reassembly::Incomplete
            ));
            assert_eq!(reassembly_count(), 1, "the partial set must be held");
        })
    }

    #[test]
    fn an_out_of_order_final_fragment_still_completes() {
        alone(|| {
            let body: Vec<u8> = (0..16u8).collect();
            assert!(matches!(
                piece(3, 8, true, &body[8..]),
                Reassembly::Incomplete
            ));
            match piece(3, 0, false, &body[..8]) {
                Reassembly::Complete { data, .. } => assert_eq!(data, body),
                other => panic!("arrival order must not matter, got {:?}", other),
            }
        })
    }

    #[test]
    fn a_retransmitted_fragment_is_harmless() {
        alone(|| {
            let body: Vec<u8> = (0..16u8).collect();
            assert!(matches!(
                piece(4, 0, true, &body[..8]),
                Reassembly::Incomplete
            ));
            // The same fragment again, byte for byte: legal, and must not
            // disturb the set.
            assert!(matches!(
                piece(4, 0, true, &body[..8]),
                Reassembly::Incomplete
            ));
            match piece(4, 8, false, &body[8..]) {
                Reassembly::Complete { data, .. } => assert_eq!(data, body),
                other => panic!("a retransmission is legal, got {:?}", other),
            }
        })
    }

    #[test]
    fn a_conflicting_overlap_discards_the_whole_set() {
        // Two fragments claiming the same bytes with different contents is the
        // teardrop-style attack: the reassembler must not pick one and continue.
        alone(|| {
            assert!(matches!(
                piece(5, 0, true, &[0xAA; 16]),
                Reassembly::Incomplete
            ));
            let result = piece(5, 8, true, &[0xBB; 8]);
            assert!(
                matches!(result, Reassembly::Dropped(_)),
                "a disagreeing overlap must be refused, not resolved"
            );
            assert_eq!(reassembly_count(), 0, "the whole set must be released");
        })
    }

    #[test]
    fn a_fragment_from_another_source_cannot_join_the_set() {
        // Same identification, different sender: splicing someone else's bytes
        // into a datagram would be possible if the source were not part of the
        // key.
        alone(|| {
            assert!(matches!(
                piece(6, 0, true, &[0u8; 8]),
                Reassembly::Incomplete
            ));
            let mut foreign = fragment_header([1, 2, 3, 4], DST, 17, 6, 8, false, 8);
            foreign.checksum = 0;
            foreign.checksum = foreign.calculate_checksum().to_be();
            // A different key means a new set, so the original must not have
            // been completed by foreign bytes.
            assert!(matches!(
                reassemble(foreign, &[9u8; 8], 1_000),
                Reassembly::Incomplete
            ));
            assert_eq!(reassembly_count(), 2);
        })
    }

    #[test]
    fn a_non_final_fragment_not_ending_on_a_boundary_is_refused() {
        // 13 bytes from offset 0 ends at byte 13, which is not a multiple of 8.
        // The next fragment's offset could not line up with it, so the pieces
        // could not tile the datagram and the set must be refused.
        alone(|| {
            assert!(matches!(piece(7, 0, true, &[0u8; 13]), Reassembly::Dropped(_)));
            assert_eq!(reassembly_count(), 0);
        })
    }

    #[test]
    fn a_final_fragment_may_end_anywhere() {
        // Only a *non-final* fragment is constrained; the last piece carries the
        // remainder, which is routinely not a multiple of 8. So the non-final
        // piece here is 16 bytes (a legal multiple of 8) and the final piece is
        // the 5-byte remainder.
        alone(|| {
            let body: Vec<u8> = (0..21u8).collect();
            assert!(matches!(
                piece(12, 0, true, &body[..16]),
                Reassembly::Incomplete
            ));
            match piece(12, 16, false, &body[16..]) {
                Reassembly::Complete { data, .. } => assert_eq!(data, body),
                other => panic!("a 5-byte final fragment is legal, got {:?}", other),
            }
        })
    }

    #[test]
    fn an_empty_fragment_is_refused() {
        alone(|| {
            assert!(matches!(piece(8, 0, true, &[]), Reassembly::Dropped(_)));
        })
    }

    #[test]
    fn a_fragment_past_the_maximum_datagram_is_refused() {
        alone(|| {
            // The largest representable offset is 65528; a fragment there that
            // runs past 65535 must be refused rather than allocated.
            let result = piece(9, 65528, false, &[0u8; 16]);
            assert!(matches!(result, Reassembly::Dropped(_)));
            assert_eq!(reassembly_count(), 0);
        })
    }

    #[test]
    fn the_offset_field_cannot_wrap_into_a_small_offset() {
        // 13 bits of offset cannot express 65536. A field set past the limit
        // must not wrap to a small offset that would collide with a real one.
        let mut header = IpHeader::new(SRC, DST, 17, 8);
        header.flags_fragment = 0xFFFFu16.to_be();
        let flags = u16::from_be(header.flags_fragment);
        assert_eq!(
            fragment_offset_bytes(flags),
            65528,
            "the largest expressible byte offset"
        );
        assert!(flags & FLAG_MORE_FRAGMENTS != 0);
    }

    #[test]
    fn the_reassembly_table_is_bounded() {
        // A peer that opens fragment sets and never completes them must not be
        // able to grow the table without limit.
        alone(|| {
            for id in 0..(MAX_REASSEMBLY as u16 * 3) {
                let _ = piece(id, 0, true, &[0u8; 8]);
            }
            assert!(
                reassembly_count() <= MAX_REASSEMBLY,
                "held {} sets, limit is {}",
                reassembly_count(),
                MAX_REASSEMBLY
            );
        })
    }

    #[test]
    fn a_partial_set_is_evicted_after_the_timeout() {
        alone(|| {
            assert!(matches!(
                piece(10, 0, true, &[0u8; 8]),
                Reassembly::Incomplete
            ));
            assert_eq!(reassembly_count(), 1);
            // A much later fragment triggers the sweep.
            let header = fragment_header(SRC, DST, 17, 11, 0, true, 8);
            let _ = reassemble(header, &[0u8; 8], REASSEMBLY_TIMEOUT_MS + 1_000);
            assert_eq!(
                reassembly_count(),
                1,
                "the expired set must be gone and only the new one held"
            );
        })
    }

    #[test]
    fn the_mtu_leaves_room_for_a_full_sized_tcp_segment() {
        // TCP segments to 1400 bytes; the fragmenter must not chop that up.
        assert!(1400 <= MAX_PAYLOAD, "a 1400 byte segment must fit the MTU");
        assert_eq!(MAX_PAYLOAD, MTU - 20);
    }
}
