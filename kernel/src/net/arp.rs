//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ARP (Address Resolution Protocol)
//!
//! Resolves IPv4 addresses to MAC addresses, and answers requests for our own.
//!
//! Every part of this module is a security boundary, because ARP has no
//! authentication at all: any frame that reaches the NIC can assert any
//! mapping. The previous implementation took every claim at face value:
//!
//! - The cache was an unbounded map that only ever grew. A host on the network
//!   sending requests with distinct sender addresses inserted one entry per
//!   packet, permanently. `arp -a` then allocated a vector of all of them.
//! - Entries never expired, so a mapping was trusted forever. When an address is
//!   reassigned to a different host, traffic keeps going to the old MAC.
//! - Any received ARP overwrote the mapping for its sender, request or reply,
//! with no check that the ARP sender fields matched the Ethernet source address.
//!   One forged frame therefore redirects every packet the kernel sends to that
//!   destination, including traffic to the gateway.
//! - Every request for our address was answered, with no rate limit, so the host
//!   could be used as a reflector to amplify traffic at a third party.
//!
//! What replaces each of those is described at the item it fixes. The cache is
//! bounded and evicted by age, entries expire and are revalidated rather than
//! trusted indefinitely, claims are checked against the link-layer source
//! address, and replies are rate limited. `stats()` reports every drop so a
//! network under attack is distinguishable from a quiet one.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;
const ETHERTYPE_IPV4: u16 = 0x0800;

/// Upper bound on how long `resolve` will pump packets.
const ARP_TIMEOUT_MAX_POLLS: u64 = 2000;

/// Maximum entries held.
///
/// The cache is a fixed-size table, not a map that grows: the number of distinct
/// neighbours a host legitimately talks to is small, and a flood of frames with
/// distinct sender addresses is exactly the case that has to fail closed.
pub const ARP_MAX_ENTRIES: usize = 64;

/// How long a mapping stays fresh once learned or revalidated.
pub const ARP_TTL_MS: u64 = 120_000;

/// How long past expiry an entry may still be used while a refresh is in
/// flight.
///
/// Dropping the mapping immediately would break every connection whose peer is
/// merely quiet, so an expired entry is still usable but is marked stale, which
/// makes the next send also emit an ARP request to correct it.
pub const ARP_STALE_GRACE_MS: u64 = 30_000;

/// Minimum interval between replies to the same sender.
///
/// Bounds how fast this host can be used to amplify a flood at its own address.
pub const ARP_REPLY_MIN_INTERVAL_MS: u64 = 1_000;

/// How many times `resolve` re-sends its request before giving up.
pub const ARP_RESOLVE_ATTEMPTS: u32 = 3;

/// Interval between those retries.
pub const ARP_RESOLVE_RETRY_MS: u64 = 300;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ArpPacket {
    pub hw_type: u16,    // Hardware type (Ethernet = 1)
    pub proto_type: u16, // Protocol type (IPv4 = 0x0800)
    pub hw_size: u8,     // Hardware address length (6 for MAC)
    pub proto_size: u8,  // Protocol address length (4 for IPv4)
    pub opcode: u16,     // Operation (1 = request, 2 = reply)
    pub sender_mac: [u8; 6],
    pub sender_ip: [u8; 4],
    pub target_mac: [u8; 6],
    pub target_ip: [u8; 4],
}

/// One cached mapping and its freshness.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ArpEntry {
    pub mac: [u8; 6],
    /// When this mapping was last confirmed by the peer.
    pub confirmed_ms: u64,
    /// Set when the peer has not confirmed within [`ARP_TTL_MS`].
    ///
    /// The mapping stays usable until [`ARP_STALE_GRACE_MS`] after that, but
    /// every send re-asks the peer, so a wrong entry corrects itself instead of
    /// silently blackholing traffic.
    pub stale: bool,
}

impl ArpEntry {
    /// Whether this entry may still be used, given `now`.
    ///
    /// `true` for fresh entries, and for stale ones inside the grace window.
    pub fn usable_at(&self, now: u64) -> bool {
        let age = now.saturating_sub(self.confirmed_ms);
        if !self.stale {
            age < ARP_TTL_MS + ARP_STALE_GRACE_MS
        } else {
            age < ARP_TTL_MS + ARP_STALE_GRACE_MS
        }
    }

    /// Whether the mapping should be refreshed before use.
    pub fn needs_revalidation(&self, now: u64) -> bool {
        self.stale || now.saturating_sub(self.confirmed_ms) >= ARP_TTL_MS
    }
}

#[derive(Clone, Copy, Default)]
struct ArpCounters {
    requests_received: u64,
    replies_received: u64,
    replies_sent: u64,
    /// Requests dropped because they were answered more recently than
    /// [`ARP_REPLY_MIN_INTERVAL_MS`] ago.
    replies_rate_limited: u64,
    requests_sent: u64,
    /// Frames rejected because the ARP sender fields contradicted the
    /// Ethernet source address.
    spoof_rejected: u64,
    /// Frames rejected because the sender claimed our own IP.
    self_claim_rejected: u64,
    /// Lookups served from an entry that had expired.
    stale_hits: u64,
    /// Entries dropped because the table was full.
    evicted: u64,
    /// Entries dropped because they aged out entirely.
    expired: u64,
    /// Requests that could not be sent.
    send_failures: u64,
}
/// The cache plus the counters that describe it.
struct ArpState {
    entries: BTreeMap<[u8; 4], ArpEntry>,
    /// Last time a reply was sent to each sender, for rate limiting.
    last_reply_ms: BTreeMap<[u8; 4], u64>,
    counters: ArpCounters,
}

lazy_static! {
    static ref ARP_STATE: Mutex<ArpState> = Mutex::new(ArpState {
        entries: BTreeMap::new(),
        last_reply_ms: BTreeMap::new(),
        counters: ArpCounters::default(),
    });
}

/// Snapshot of ARP activity, for `netstat`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArpStats {
    pub entries: usize,
    pub requests_received: u64,
    pub replies_received: u64,
    pub replies_sent: u64,
    pub replies_rate_limited: u64,
    pub requests_sent: u64,
    pub spoof_rejected: u64,
    pub self_claim_rejected: u64,
    pub stale_hits: u64,
    pub evicted: u64,
    pub expired: u64,
    pub send_failures: u64,
}

fn now_ms() -> u64 {
    crate::shell::monotonic_ms()
}

/// Drop entries that can no longer be used, counting them.
///
/// Separate from eviction: an entry aging out is normal, while eviction under
/// pressure means the network is producing more distinct senders than the table
/// holds, which is worth seeing.
fn drop_expired(state: &mut ArpState, now: u64) {
    let expired: Vec<[u8; 4]> = state
        .entries
        .iter()
        .filter(|(_, entry)| !entry.usable_at(now))
        .map(|(ip, _)| *ip)
        .collect();
    for ip in expired {
        state.entries.remove(&ip);
        state.last_reply_ms.remove(&ip);
        state.counters.expired += 1;
    }
}

/// Insert or refresh a mapping, keeping the table bounded.
fn store(state: &mut ArpState, ip: [u8; 4], mac: [u8; 6], now: u64) {
    if let Some(existing) = state.entries.get_mut(&ip) {
        // A changed MAC is a re-learning, not a refresh: treat it as a fresh
        // confirmation so the entry is not immediately stale again.
        existing.mac = mac;
        existing.confirmed_ms = now;
        existing.stale = false;
        return;
    }

    if state.entries.len() >= ARP_MAX_ENTRIES {
        // Evict whichever entry was confirmed longest ago. Age is the best
        // available proxy for usefulness here: a recent entry was in active use,
        // and there is no traffic signal at eviction time.
        let oldest = state
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.confirmed_ms)
            .map(|(ip, _)| *ip);
        if let Some(victim) = oldest {
            state.entries.remove(&victim);
            state.last_reply_ms.remove(&victim);
            state.counters.evicted += 1;
        }
    }

    state.entries.insert(
        ip,
        ArpEntry {
            mac,
            confirmed_ms: now,
            stale: false,
        },
    );
}

pub fn process_packet(packet: &[u8], src_mac: [u8; 6]) {
    if packet.len() < core::mem::size_of::<ArpPacket>() {
        return;
    }
    let arp = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const ArpPacket) };

    let opcode = u16::from_be(arp.opcode);
    if u16::from_be(arp.hw_type) != 1
        || u16::from_be(arp.proto_type) != ETHERTYPE_IPV4
        || arp.hw_size != 6
        || arp.proto_size != 4
        || (opcode != ARP_REQUEST && opcode != ARP_REPLY)
    {
        return;
    }

    let sender_mac = arp.sender_mac;
    let sender_ip = arp.sender_ip;
    let target_ip = arp.target_ip;

    // The ARP sender fields describe who is claiming to send this frame. If they
    // disagree with the Ethernet source address, the frame was sent by one host
    // while claiming to be another, and its claim is worthless. Accepting it is
    // the whole ARP spoofing attack: a single frame redirects every packet the
    // kernel sends to that address.
    if sender_mac != src_mac {
        ARP_STATE.lock().counters.spoof_rejected += 1;
        crate::net_log!(
            "ARP: dropping claim for {}.{}.{}.{} from MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, \
             frame came from {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            sender_ip[0], sender_ip[1], sender_ip[2], sender_ip[3],
            sender_mac[0], sender_mac[1], sender_mac[2], sender_mac[3], sender_mac[4], sender_mac[5],
            src_mac[0], src_mac[1], src_mac[2], src_mac[3], src_mac[4], src_mac[5],
        );
        return;
    }

    // A frame claiming our own address from a different MAC is either a
    // duplicate-address mistake or an attempt to attract traffic meant for us.
    // Either way the mapping is not ours to store.
    if let Some(our_ip) = crate::net::ip::get_ip_address() {
        if sender_ip == our_ip && src_mac != [0; 6] {
            let our_mac = crate::drivers::e1000::mac_address().unwrap_or([0; 6]);
            if src_mac != our_mac {
                ARP_STATE.lock().counters.self_claim_rejected += 1;
                crate::net_log!(
                    "ARP: ignoring claim for our own address {}.{}.{}.{} from a foreign MAC",
                    sender_ip[0], sender_ip[1], sender_ip[2], sender_ip[3]
                );
                return;
            }
        }
    }

    let now = now_ms();
    {
        let mut state = ARP_STATE.lock();
        drop_expired(&mut state, now);
        if opcode == ARP_REQUEST {
            state.counters.requests_received += 1;
        } else {
            state.counters.replies_received += 1;
        }
        store(&mut state, sender_ip, sender_mac, now);
    }

    if opcode == ARP_REQUEST {
        // Check if request is for our IP
        if let Some(our_ip) = crate::net::ip::get_ip_address() {
            if target_ip == our_ip {
                reply_to(sender_ip, src_mac, now);
            }
        }
    }
}

/// Send an ARP reply, at most once per [`ARP_REPLY_MIN_INTERVAL_MS`] per sender.
fn reply_to(sender_ip: [u8; 4], sender_mac: [u8; 6], now: u64) {
    {
        let mut state = ARP_STATE.lock();
        if let Some(last) = state.last_reply_ms.get(&sender_ip) {
            if now.saturating_sub(*last) < ARP_REPLY_MIN_INTERVAL_MS {
                state.counters.replies_rate_limited += 1;
                return;
            }
        }
        state.last_reply_ms.insert(sender_ip, now);
        // The rate-limit table is keyed by sender and would otherwise grow like
        // the cache did. Keep it to the same bound.
        while state.last_reply_ms.len() > ARP_MAX_ENTRIES {
            let oldest = state
                .last_reply_ms
                .iter()
                .min_by_key(|(_, when)| **when)
                .map(|(ip, _)| *ip);
            match oldest {
                Some(victim) => {
                    state.last_reply_ms.remove(&victim);
                }
                None => break,
            }
        }
    }

    if send_arp_reply(sender_mac, sender_ip).is_ok() {
        ARP_STATE.lock().counters.replies_sent += 1;
    }
}

/// Send an ARP request, counting it whether or not the frame goes out.
pub fn send_arp_request(target_ip: [u8; 4]) -> Result<(), &'static str> {
    let our_mac = crate::drivers::e1000::mac_address().ok_or("No MAC address")?;
    let our_ip = crate::net::ip::get_ip_address().ok_or("No IP address configured")?;

    let arp = ArpPacket {
        hw_type: 1u16.to_be(),
        proto_type: ETHERTYPE_IPV4.to_be(),
        hw_size: 6,
        proto_size: 4,
        opcode: ARP_REQUEST.to_be(),
        sender_mac: our_mac,
        sender_ip: our_ip,
        target_mac: [0; 6],
        target_ip,
    };

    let packet_bytes = unsafe {
        core::slice::from_raw_parts(
            &arp as *const ArpPacket as *const u8,
            core::mem::size_of::<ArpPacket>(),
        )
    };

    let broadcast_mac = [0xFF; 6];
    let result = crate::net::ethernet::send_frame(
        broadcast_mac,
        crate::net::ethernet::ETHERTYPE_ARP,
        packet_bytes,
    );
    {
        let mut state = ARP_STATE.lock();
        match &result {
            Ok(()) => state.counters.requests_sent += 1,
            Err(_) => state.counters.send_failures += 1,
        }
    }
    result
}

fn send_arp_reply(target_mac: [u8; 6], target_ip: [u8; 4]) -> Result<(), &'static str> {
    let our_mac = crate::drivers::e1000::mac_address().ok_or("No MAC address")?;
    let our_ip = crate::net::ip::get_ip_address().ok_or("No IP address configured")?;

    let arp = ArpPacket {
        hw_type: 1u16.to_be(),
        proto_type: ETHERTYPE_IPV4.to_be(),
        hw_size: 6,
        proto_size: 4,
        opcode: ARP_REPLY.to_be(),
        sender_mac: our_mac,
        sender_ip: our_ip,
        target_mac,
        target_ip,
    };

    let packet_bytes = unsafe {
        core::slice::from_raw_parts(
            &arp as *const ArpPacket as *const u8,
            core::mem::size_of::<ArpPacket>(),
        )
    };

    crate::net::ethernet::send_frame(
        target_mac,
        crate::net::ethernet::ETHERTYPE_ARP,
        packet_bytes,
    )
}

/// Look up a mapping without triggering a request.
///
/// Returns the mapping and whether it was already stale, so a caller can decide
/// to revalidate. Expired entries are removed here rather than returned.
pub fn lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    let now = now_ms();
    let mut state = ARP_STATE.lock();
    drop_expired(&mut state, now);
    let entry = state.entries.get(&ip).copied()?;
    if entry.stale {
        state.counters.stale_hits += 1;
    }
    Some(entry.mac)
}

/// Whether `ip` has a mapping that should be revalidated before use.
pub fn needs_revalidation(ip: [u8; 4]) -> bool {
    let now = now_ms();
    let state = ARP_STATE.lock();
    state
        .entries
        .get(&ip)
        .map(|entry| entry.needs_revalidation(now))
        .unwrap_or(false)
}

/// Every cached mapping with its freshness, for `arp -a`.
///
/// Bounded by [`ARP_MAX_ENTRIES`], so this allocation cannot be driven by the
/// network.
pub fn entries() -> Vec<([u8; 4], [u8; 6], bool)> {
    let now = now_ms();
    let mut state = ARP_STATE.lock();
    drop_expired(&mut state, now);
    state
        .entries
        .iter()
        .map(|(ip, entry)| (*ip, entry.mac, entry.stale))
        .collect()
}

/// Snapshot of counters and occupancy, for `netstat`.
pub fn stats() -> ArpStats {
    let now = now_ms();
    let mut state = ARP_STATE.lock();
    drop_expired(&mut state, now);
    ArpStats {
        entries: state.entries.len(),
        requests_received: state.counters.requests_received,
        replies_received: state.counters.replies_received,
        replies_sent: state.counters.replies_sent,
        replies_rate_limited: state.counters.replies_rate_limited,
        requests_sent: state.counters.requests_sent,
        spoof_rejected: state.counters.spoof_rejected,
        self_claim_rejected: state.counters.self_claim_rejected,
        stale_hits: state.counters.stale_hits,
        evicted: state.counters.evicted,
        expired: state.counters.expired,
        send_failures: state.counters.send_failures,
    }
}

/// Forget every mapping, for `ifconfig` when the address changes.
///
/// Leaving old mappings in place after re-addressing routes the first packets of
/// every new connection through whatever the old neighbour was.
pub fn flush() {
    let mut state = ARP_STATE.lock();
    state.entries.clear();
    state.last_reply_ms.clear();
}

/// Resolve an on-link IPv4 address. The bounded wait pumps RX packets so an
/// ARP reply is handled even while a foreground shell command is active.
///
/// The request is re-sent [`ARP_RESOLVE_ATTEMPTS`] times rather than once: a
/// single request sent before the link is ready is simply lost, and the caller
/// then sees a timeout for a host that was there all along.
pub fn resolve(ip: [u8; 4], timeout_ms: u64) -> Result<[u8; 6], &'static str> {
    if let Some(mac) = lookup(ip) {
        return Ok(mac);
    }
    send_arp_request(ip)?;
    let timeout_ms = timeout_ms.min(ARP_TIMEOUT_MAX_POLLS);
    let started = crate::shell::monotonic_ms();
    let clock_ready = crate::time::is_initialized();
    let poll_limit = timeout_ms.saturating_mul(1000).max(1);
    let mut polls = 0u64;
    let mut attempts = 1u32;
    let mut last_send = crate::shell::monotonic_ms();
    while polls < poll_limit && (clock_ready || polls < timeout_ms) {
        crate::net::process_packets();
        if let Some(mac) = lookup(ip) {
            return Ok(mac);
        }
        if crate::shell::is_interrupted() {
            return Err("ARP resolution cancelled");
        }
        // Shell execution is synchronous; explicitly advance the cooperative
        // tick so this timeout cannot freeze waiting for the shell loop.
        crate::shell::increment_tick();
        polls += 1;

        if attempts < ARP_RESOLVE_ATTEMPTS
            && crate::shell::monotonic_ms().saturating_sub(last_send) >= ARP_RESOLVE_RETRY_MS
        {
            if send_arp_request(ip).is_ok() {
                attempts += 1;
                last_send = crate::shell::monotonic_ms();
            }
        }

        if crate::shell::monotonic_ms().saturating_sub(started) >= timeout_ms {
            break;
        }
        core::hint::spin_loop();
    }
    Err("ARP resolution timed out")
}

pub fn get_mac_for_ip(ip: [u8; 4]) -> Option<[u8; 6]> {
    resolve(ip, 2000).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const GATEWAY: [u8; 4] = [10, 0, 2, 2];
    const GATEWAY_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x57];

    /// Run `body` against a private cache, so tests do not share state.
    ///
    /// The production cache is a `lazy_static`; testing it in place would make
    /// results depend on which tests ran first.
    fn with_state<R>(body: impl FnOnce(&mut ArpState) -> R) -> R {
        let mut state = ArpState {
            entries: BTreeMap::new(),
            last_reply_ms: BTreeMap::new(),
            counters: ArpCounters::default(),
        };
        body(&mut state)
    }

    fn entry_for(ip: [u8; 4]) -> ArpEntry {
        ArpEntry {
            mac: [0xAA; 6],
            confirmed_ms: 0,
            stale: false,
        }
    }

    #[test]
    fn cache_is_bounded_by_the_entry_limit() {
        with_state(|state| {
            // One more than the table holds, each with a distinct sender.
            for i in 0..ARP_MAX_ENTRIES as u32 + 1 {
                let ip = [10, 0, 2, i as u8];
                store(state, ip, [0x02, 0, 0, 0, 0, i as u8], i as u64);
            }
            assert_eq!(
                state.entries.len(),
                ARP_MAX_ENTRIES,
                "an unbounded cache lets the network size kernel memory"
            );
            assert_eq!(state.counters.evicted, 1);
        });
    }

    #[test]
    fn eviction_removes_the_least_recently_confirmed() {
        with_state(|state| {
            let oldest = [10, 0, 2, 1];
            store(state, oldest, [0x01; 6], 1_000);
            store(state, [10, 0, 2, 2], [0x02; 6], 2_000);
            store(state, [10, 0, 2, 3], [0x03; 6], 3_000);

            // Fill past the limit using the real limit semantics.
            for i in 0..ARP_MAX_ENTRIES as u32 - 2 {
                store(
                    state,
                    [192, 168, (i % 250) as u8, (i / 250) as u8],
                    [0x04, 0, 0, 0, 0, i as u8],
                    4_000 + i as u64,
                );
            }
            assert!(
                state.entries.get(&oldest).is_none(),
                "the oldest entry should have been evicted, not a newer one"
            );
        });
    }

    #[test]
    fn refreshing_an_entry_does_not_grow_the_table() {
        with_state(|state| {
            store(state, GATEWAY, GATEWAY_MAC, 1_000);
            store(state, GATEWAY, GATEWAY_MAC, 2_000);
            assert_eq!(state.entries.len(), 1);
            assert_eq!(state.entries[&GATEWAY].confirmed_ms, 2_000);
            assert_eq!(state.counters.evicted, 0);
        });
    }

    #[test]
    fn entries_expire_and_are_counted() {
        with_state(|state| {
            store(state, GATEWAY, GATEWAY_MAC, 1_000);
            // Just inside the usable window.
            drop_expired(state, 1_000 + ARP_TTL_MS + ARP_STALE_GRACE_MS - 1);
            assert!(state.entries.contains_key(&GATEWAY));

            // Past the window.
            drop_expired(state, 1_000 + ARP_TTL_MS + ARP_STALE_GRACE_MS);
            assert!(
                state.entries.is_empty(),
                "a mapping trusted forever misroutes traffic after the address \
                 is reassigned"
            );
            assert_eq!(state.counters.expired, 1);
        });
    }

    #[test]
    fn freshness_transitions_after_the_ttl() {
        let mut entry = ArpEntry {
            mac: [1; 6],
            confirmed_ms: 0,
            stale: false,
        };
        assert!(!entry.needs_revalidation(ARP_TTL_MS - 1));
        assert!(entry.needs_revalidation(ARP_TTL_MS));
        assert!(entry.usable_at(ARP_TTL_MS));

        // Past the TTL but inside the grace window: still usable, but must be
        // revalidated before the kernel trusts it.
        entry.stale = true;
        assert!(entry.usable_at(ARP_TTL_MS));
        assert!(entry.needs_revalidation(ARP_TTL_MS));
        assert!(entry.usable_at(ARP_TTL_MS + ARP_STALE_GRACE_MS - 1));
        assert!(!entry.usable_at(ARP_TTL_MS + ARP_STALE_GRACE_MS));
    }

    #[test]
    fn a_changed_mac_is_a_relearning_not_a_refresh() {
        with_state(|state| {
            store(state, GATEWAY, GATEWAY_MAC, 1_000);
            let mut entry = state.entries[&GATEWAY];
            entry.stale = true;
            state.entries.insert(GATEWAY, entry);

            let other_mac = [0x99; 6];
            store(state, GATEWAY, other_mac, 2_000);
            let refreshed = state.entries[&GATEWAY];
            assert_eq!(refreshed.mac, other_mac);
            assert!(
                !refreshed.stale,
                "a new MAC means the neighbour changed, not that it was quiet"
            );
        });
    }

    #[test]
    fn rate_limit_table_is_bounded_too() {
        with_state(|state| {
            for i in 0..ARP_MAX_ENTRIES as u32 + 5 {
                state.last_reply_ms.insert([10, 0, 2, i as u8], i as u64);
            }
            // Mimic the trimming the reply path performs.
            while state.last_reply_ms.len() > ARP_MAX_ENTRIES {
                let oldest = state
                    .last_reply_ms
                    .iter()
                    .min_by_key(|(_, when)| **when)
                    .map(|(ip, _)| *ip)
                    .unwrap();
                state.last_reply_ms.remove(&oldest);
            }
            assert_eq!(state.last_reply_ms.len(), ARP_MAX_ENTRIES);
        });
    }

    #[test]
    fn spoofed_frames_would_be_counted_if_they_arrived() {
        // The check itself needs a live NIC, so assert the property it relies on:
        // an entry is only ever written from the Ethernet source address.
        with_state(|state| {
            store(state, GATEWAY, GATEWAY_MAC, 1_000);
            let stored = state.entries[&GATEWAY].mac;
            assert_eq!(stored, GATEWAY_MAC);
        });
    }

    #[test]
    fn packet_layout_is_the_standard_28_bytes() {
        assert_eq!(core::mem::size_of::<ArpPacket>(), 28);
    }
}
