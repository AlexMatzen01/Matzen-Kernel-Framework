//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! UDP (User Datagram Protocol)

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdpDatagram {
    pub source_ip: [u8; 4],
    pub source_port: u16,
    pub destination_port: u16,
    pub payload: Vec<u8>,
}

/// Maximum datagrams held in the receive queue.
pub const MAX_QUEUED_DATAGRAMS: usize = 32;

/// Maximum bytes of payload held in the receive queue.
///
/// The count alone is not a bound: a datagram can be up to 64 KiB, so a
/// 32-datagram queue can hold 2 MiB of heap chosen entirely by whoever sends
/// them. The byte budget is what actually caps memory; the count is kept as well
/// because 32 tiny datagrams is not worth the bookkeeping of a byte limit alone.
pub const MAX_QUEUED_BYTES: usize = 256 * 1024;

/// Dropped because the queue was full.
static DROPPED_FULL: AtomicU64 = AtomicU64::new(0);
/// Dropped because they were larger than the whole byte budget.
static DROPPED_OVERSIZED: AtomicU64 = AtomicU64::new(0);
/// Dropped because their checksum did not verify.
static BAD_CHECKSUM: AtomicU64 = AtomicU64::new(0);
/// Bytes currently held in the queue.
static QUEUED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Queue occupancy and drop counters, for `netstat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UdpStats {
    pub queued: usize,
    pub queued_bytes: usize,
    pub dropped_full: u64,
    pub dropped_oversized: u64,
    /// Datagrams discarded because their checksum did not verify.
    pub bad_checksum: u64,
}

lazy_static! {
    static ref RX_QUEUE: Mutex<VecDeque<UdpDatagram>> = Mutex::new(VecDeque::new());
}

impl UdpHeader {
    pub fn new(src_port: u16, dst_port: u16, data_len: u16) -> Self {
        UdpHeader {
            src_port: src_port.to_be(),
            dst_port: dst_port.to_be(),
            length: (8 + data_len).to_be(),
            checksum: 0, // Optional for IPv4
        }
    }

    pub fn get_src_port(&self) -> u16 {
        u16::from_be(self.src_port)
    }

    pub fn get_dst_port(&self) -> u16 {
        u16::from_be(self.dst_port)
    }

    pub fn get_length(&self) -> u16 {
        u16::from_be(self.length)
    }
}

/// Queue a datagram within the byte and count budgets.
fn enqueue(datagram: UdpDatagram) {
    if datagram.payload.len() > MAX_QUEUED_BYTES {
        // Nothing in the queue could make room for this, so it can never be
        // stored. Counting it separately distinguishes an oversized sender from a
        // busy queue.
        DROPPED_OVERSIZED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let mut queue = RX_QUEUE.lock();
    let queued_bytes = QUEUED_BYTES.load(Ordering::Relaxed);
    while (queue.len() >= MAX_QUEUED_DATAGRAMS
        || queued_bytes + datagram.payload.len() > MAX_QUEUED_BYTES)
        && !queue.is_empty()
    {
        // Drop oldest first. For a reliable transport on top (which is what this
        // queue usually carries) the oldest is the one that will be retransmitted,
        // whereas dropping the newest stalls everything queued behind it.
        if let Some(dropped) = queue.pop_front() {
            QUEUED_BYTES.fetch_sub(dropped.payload.len(), Ordering::Relaxed);
        }
    }
    if queue.len() >= MAX_QUEUED_DATAGRAMS
        || QUEUED_BYTES.load(Ordering::Relaxed) + datagram.payload.len() > MAX_QUEUED_BYTES
    {
        DROPPED_FULL.fetch_add(1, Ordering::Relaxed);
        return;
    }
    QUEUED_BYTES.fetch_add(datagram.payload.len(), Ordering::Relaxed);
    queue.push_back(datagram);
}

/// Remove the first datagram matching `predicate`, keeping the byte accounting.
fn take_matching<T>(predicate: impl Fn(&UdpDatagram) -> bool, map: impl FnOnce(UdpDatagram) -> T) -> Option<T> {
    let mut queue = RX_QUEUE.lock();
    let index = queue.iter().position(&predicate)?;
    let datagram = queue.remove(index)?;
    QUEUED_BYTES.fetch_sub(datagram.payload.len(), Ordering::Relaxed);
    Some(map(datagram))
}

/// One's-complement sum over `bytes`.
fn ones_complement_sum(bytes: &[u8], initial: u32) -> u32 {
    let mut sum = initial;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        sum += ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
        i += 2;
    }
    if i < bytes.len() {
        // Odd length: the final byte is padded on the right.
        sum += (bytes[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum
}

/// UDP checksum over the IPv4 pseudo-header and the datagram.
///
/// The pseudo-header covers the source and destination addresses, the protocol
/// number and the UDP length. A checksum over the datagram alone would not
/// notice an address being corrupted in transit, which is exactly what the field
/// is for.
///
/// The result is 0 for a correct datagram; `0xFFFF` is also correct, being the
/// all-ones form of a zero checksum.
pub fn checksum(src_ip: [u8; 4], dst_ip: [u8; 4], datagram: &[u8]) -> u16 {
    let mut sum = 0u32;
    for address in [src_ip, dst_ip] {
        sum += ((address[0] as u32) << 8) | (address[1] as u32);
        sum += ((address[2] as u32) << 8) | (address[3] as u32);
    }
    sum += 17; // protocol: UDP
    sum += datagram.len() as u32;
    !(ones_complement_sum(datagram, sum) as u16)
}

/// Whether a datagram's stored checksum is acceptable.
///
/// RFC 768 makes the UDP checksum optional over IPv4: zero means "not computed".
/// Any other value must verify, and a corrupt datagram must not be delivered as
/// data — DNS and DHCP both parse untrusted fields out of it.
///
/// Verification sums the stored checksum back in, so a correct datagram sums to
/// all-ones; it does not recompute the field and compare. Recomputing would
/// compare the stored value against a value computed with the field treated as
/// zero, which never matches.
pub fn checksum_accepts(src_ip: [u8; 4], dst_ip: [u8; 4], datagram: &[u8]) -> bool {
    if datagram.len() < 8 {
        return false;
    }
    let stored = u16::from_be_bytes([datagram[6], datagram[7]]);
    if stored == 0 {
        return true;
    }
    let mut sum = 0u32;
    for address in [src_ip, dst_ip] {
        sum += ((address[0] as u32) << 8) | (address[1] as u32);
        sum += ((address[2] as u32) << 8) | (address[3] as u32);
    }
    sum += 17;
    sum += datagram.len() as u32;
    let result = ones_complement_sum(datagram, sum) as u16;
    result == 0 || result == 0xFFFF
}

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], dst_ip: [u8; 4]) {
    if packet.len() < 8 {
        return;
    }

    let udp_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const UdpHeader) };

    let length = udp_header.get_length() as usize;
    if length < 8 || length > packet.len() {
        return;
    }
    let datagram = &packet[..length];

    // The destination comes from the IP header, not from the local configuration:
    // a datagram is checked against the address it was actually sent to. Using
    // the local address instead would reject a broadcast DHCP offer, because the
    // offer's destination is 255.255.255.255 while the local address is
    // something else entirely.
    if !checksum_accepts(src_ip, dst_ip, datagram) {
        BAD_CHECKSUM.fetch_add(1, Ordering::Relaxed);
        crate::net_log!(
            "UDP: checksum invalid, discarding {} byte datagram from {}.{}.{}.{}",
            length,
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3]
        );
        return;
    }

    let payload = &packet[8..length];

    crate::net_log!(
        "Received UDP packet from {}.{}.{}.{}:{} -> port {}",
        src_ip[0],
        src_ip[1],
        src_ip[2],
        src_ip[3],
        udp_header.get_src_port(),
        udp_header.get_dst_port()
    );

    enqueue(UdpDatagram {
        source_ip: src_ip,
        source_port: udp_header.get_src_port(),
        destination_port: udp_header.get_dst_port(),
        payload: payload.to_vec(),
    });
}

/// Pop the oldest datagram received on `local_port`.
pub fn receive(local_port: u16) -> Option<UdpDatagram> {
    take_matching(
        |datagram| datagram.destination_port == local_port,
        |datagram| datagram,
    )
}

pub fn receive_from(local_port: u16, source_ip: [u8; 4], source_port: u16) -> Option<Vec<u8>> {
    take_matching(
        |datagram| {
            datagram.destination_port == local_port
                && datagram.source_ip == source_ip
                && datagram.source_port == source_port
        },
        |datagram| datagram.payload,
    )
}

/// Queue occupancy and drop counters, for `netstat`.
pub fn stats() -> UdpStats {
    UdpStats {
        queued: RX_QUEUE.lock().len(),
        queued_bytes: QUEUED_BYTES.load(Ordering::Relaxed),
        dropped_full: DROPPED_FULL.load(Ordering::Relaxed),
        dropped_oversized: DROPPED_OVERSIZED.load(Ordering::Relaxed),
        bad_checksum: BAD_CHECKSUM.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: [u8; 4] = [10, 0, 2, 15];
    const DST: [u8; 4] = [10, 0, 2, 3];

    fn datagram(payload: &[u8]) -> Vec<u8> {
        let header = UdpHeader::new(49153, 53, payload.len() as u16);
        let mut out = Vec::new();
        unsafe {
            out.extend_from_slice(core::slice::from_raw_parts(
                &header as *const UdpHeader as *const u8,
                core::mem::size_of::<UdpHeader>(),
            ));
        }
        out.extend_from_slice(payload);
        out
    }

    fn seal(mut out: Vec<u8>) -> Vec<u8> {
        let mut computed = checksum(SRC, DST, &out);
        if computed == 0 {
            computed = 0xFFFF;
        }
        out[6] = (computed >> 8) as u8;
        out[7] = (computed & 0xFF) as u8;
        out
    }

    #[test]
    fn a_sealed_datagram_verifies() {
        let sealed = seal(datagram(b"\x00\x01hello"));
        assert!(checksum_accepts(SRC, DST, &sealed));
    }

    #[test]
    fn a_zero_checksum_is_accepted_as_unchecked() {
        // RFC 768 makes the checksum optional over IPv4, and slirp does send zero
        // in some paths, so zero must not be read as corruption.
        let mut bare = datagram(b"payload");
        bare[6] = 0;
        bare[7] = 0;
        assert!(checksum_accepts(SRC, DST, &bare));
    }

    #[test]
    fn a_corrupted_payload_is_rejected() {
        // The reason this matters: DNS and DHCP parse untrusted fields straight
        // out of the payload, so a corrupt datagram must not be delivered as if
        // it were intact.
        let mut sealed = seal(datagram(b"hello world"));
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(!checksum_accepts(SRC, DST, &sealed));
    }

    #[test]
    fn a_corrupted_header_is_rejected() {
        let mut sealed = seal(datagram(b"hello"));
        // Change the destination port without recomputing.
        sealed[2] ^= 0xFF;
        assert!(!checksum_accepts(SRC, DST, &sealed));
    }

    #[test]
    fn the_checksum_covers_the_addresses() {
        // A datagram valid for one destination must not validate for another:
        // that is what stops a corrupted address from going unnoticed.
        let sealed = seal(datagram(b"payload"));
        assert!(checksum_accepts(SRC, DST, &sealed));
        assert!(!checksum_accepts(SRC, [10, 0, 2, 4], &sealed));
        assert!(!checksum_accepts([10, 0, 2, 16], DST, &sealed));
    }

    #[test]
    fn a_short_datagram_is_never_accepted() {
        assert!(!checksum_accepts(SRC, DST, &[0; 7]));
    }

    #[test]
    fn an_odd_length_payload_is_handled() {
        // The last byte of an odd-length datagram is padded on the right, and a
        // checksum that ignored it would accept a corrupted final byte.
        for len in 1..9usize {
            let sealed = seal(datagram(&vec![0xABu8; len]));
            assert!(checksum_accepts(SRC, DST, &sealed), "len {}", len);
            let mut corrupted = sealed.clone();
            let last = corrupted.len() - 1;
            corrupted[last] ^= 0x01;
            assert!(!checksum_accepts(SRC, DST, &corrupted), "len {}", len);
        }
    }
}

pub fn send_packet(
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    data: &[u8],
) -> Result<(), &'static str> {
    // A UDP datagram is limited to 65535 bytes by its length field. Anything over
    // the link MTU is handled by IPv4 fragmentation, which reassembles it before
    // this function ever sees it.
    if data.len() > u16::MAX as usize - 8 {
        return Err("UDP payload too large");
    }
    let udp_header = UdpHeader::new(src_port, dst_port, data.len() as u16);

    let mut packet = alloc::vec::Vec::with_capacity(8 + data.len());
    unsafe {
        let header_bytes = core::slice::from_raw_parts(
            &udp_header as *const UdpHeader as *const u8,
            core::mem::size_of::<UdpHeader>(),
        );
        packet.extend_from_slice(header_bytes);
    }
    packet.extend_from_slice(data);

    // Compute the checksum rather than leaving it zero. Zero is legal over IPv4
    // and so nothing has demanded it, but it means a corrupted datagram arrives
    // intact and is then parsed as if it were trustworthy — which matters for
    // the protocols carried here, DNS and DHCP, that read fields straight out of
    // the payload. There is no transmit offload in this driver, so it has to be
    // done here.
    let src_ip = crate::net::ip::get_ip_address().unwrap_or([0, 0, 0, 0]);
    let mut computed = checksum(src_ip, dst_ip, &packet);
    if computed == 0 {
        computed = 0xFFFF;
    }
    packet[6] = (computed >> 8) as u8;
    packet[7] = (computed & 0xFF) as u8;

    crate::net::ip::send_packet(dst_ip, 17, &packet)
}
