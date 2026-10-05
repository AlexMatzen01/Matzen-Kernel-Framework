//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! DHCP client (RFC 2131), enough of it to obtain a lease.
//!
//! The kernel previously had no DHCP at all: every boot required the user to
//! type `ifconfig 10.0.2.15 255.255.255.0 10.0.2.2`. That works under QEMU but
//! means the stack has never exercised address acquisition, and it is not how a
//! host is normally configured.
//!
//! Two properties shape this implementation:
//!
//! - **It runs before there is an address.** A DHCP client has no IP yet, so it
//!   cannot use `ip::send_packet`, which resolves the next hop through ARP and
//!   needs a configured address to decide what that is. Instead
//!   [`send_broadcast`] builds the IPv4 and UDP headers itself and hands the
//!   result to `ethernet::send_frame` with a broadcast destination MAC. The one
//!   frame that must not depend on the ARP cache is the first frame.
//! - **A lease from the network is untrusted input.** The offered address is
//!   checked before it is configured: a server (or anything answering on port
//!   67) can propose `0.0.0.0`, the broadcast address, a multicast address, or
//!   the address this host already holds, and each of those breaks routing if
//!   configured. A zero subnet mask, a missing server identifier, and a missing
//!   netmask are likewise refused. See [`Lease::is_usable`].

use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

pub const DHCP_SERVER_PORT: u16 = 67;
pub const DHCP_CLIENT_PORT: u16 = 68;

/// Smallest legal IPv4 datagram: 20 IP + 8 UDP + 236 BOOTP + 4 cookie + options.
/// The 576-byte minimum from RFC 2131 is the size a client must be able to
/// receive, and nothing legitimate is larger.
pub const MAX_DHCP_PACKET: usize = 576;

/// Fixed part of a BOOTP message, before the magic cookie.
pub const BOOTP_FIXED_LEN: usize = 236;
/// BOOTP header + cookie.
pub const BOOTP_HEADER_LEN: usize = BOOTP_FIXED_LEN + 4;

// BOOTP opcodes.
const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;

// DHCP message types (option 53).
pub const DHCP_DISCOVER: u8 = 1;
pub const DHCP_OFFER: u8 = 2;
pub const DHCP_REQUEST: u8 = 3;
pub const DHCP_DECLINE: u8 = 4;
pub const DHCP_ACK: u8 = 5;
pub const DHCP_NAK: u8 = 6;

// Option codes.
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_REQUEST_LIST: u8 = 55;
const OPT_BROADCAST: u8 = 28;
const OPT_END: u8 = 255;

/// Magic cookie that separates the fixed header from the options.
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

/// Set the broadcast flag so a server without a cached MAC still replies.
const FLAG_BROADCAST: u16 = 0x8000;

/// Attempts per phase of DORA before giving up.
pub const DISCOVER_ATTEMPTS: u32 = 4;
/// Attempts for the request/ack phase.
pub const REQUEST_ATTEMPTS: u32 = 4;
/// Delay before the first retry, doubling up to this ceiling.
pub const RETRY_MIN_MS: u64 = 250;
pub const RETRY_MAX_MS: u64 = 2_000;

/// Options parsed out of a server reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub message_type: Option<u8>,
    pub server_id: Option<[u8; 4]>,
    pub requested_ip: Option<[u8; 4]>,
    pub subnet_mask: Option<[u8; 4]>,
    pub router: Option<[u8; 4]>,
    pub dns: Option<[u8; 4]>,
    pub broadcast: Option<[u8; 4]>,
    /// Lease time in seconds, 0 when the server did not say.
    pub lease_secs: u32,
}

/// A lease as offered, before any decision is taken about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub offered_ip: [u8; 4],
    pub server_id: Option<[u8; 4]>,
    pub options: Options,
    /// Transaction id the offer arrived under.
    pub xid: u32,
}

/// A lease this client is willing to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub address: [u8; 4],
    pub netmask: [u8; 4],
    pub router: Option<[u8; 4]>,
    pub dns: Option<[u8; 4]>,
    pub server_id: Option<[u8; 4]>,
    pub lease_secs: u32,
}

/// Counters for `netstat`, so a failed negotiation is distinguishable from one
/// that never ran.
#[derive(Debug, Clone, Copy, Default)]
pub struct DhcpStats {
    pub discovers_sent: u64,
    pub requests_sent: u64,
    pub offers_received: u64,
    pub acks_received: u64,
    pub naks_received: u64,
    /// Replies discarded before they could be considered.
    pub malformed_dropped: u64,
    /// Offers refused as unusable, with the reason for the most recent.
    pub leases_rejected: u64,
    pub last_reject_reason: Option<&'static str>,
    pub leases_obtained: u64,
    /// Set once a lease has been configured.
    pub configured: bool,
}

/// Parse the options area.
///
/// Returns `None` for a packet whose options run past its end, which is the
/// failure mode a malformed or truncated reply produces. A missing message type
/// is not malformed, just useless, so it is reported as a parsed-but-empty
/// option set and filtered by the caller.
pub fn parse_options(options: &[u8]) -> Option<Options> {
    let mut parsed = Options {
        message_type: None,
        server_id: None,
        requested_ip: None,
        subnet_mask: None,
        router: None,
        dns: None,
        broadcast: None,
        lease_secs: 0,
    };

    let mut offset = 0usize;
    // Each iteration consumes at least one byte, so this bounds the loop without
    // needing the option data itself to be well-formed.
    while offset < options.len() {
        let code = options[offset];
        offset += 1;
        match code {
            OPT_END => break,
            // Pad, per RFC 2132: a single zero byte is allowed anywhere.
            0 => continue,
            _ => {}
        }
        let len = *options.get(offset)? as usize;
        // Length byte itself must exist, then the payload must fit.
        let payload = options.get(offset + 1..offset + 1 + len)?;
        offset += 1 + len;

        match code {
            OPT_MESSAGE_TYPE if len == 1 => parsed.message_type = Some(payload[0]),
            OPT_SERVER_ID if len == 4 => parsed.server_id = Some([payload[0], payload[1], payload[2], payload[3]]),
            OPT_REQUESTED_IP if len == 4 => {
                parsed.requested_ip = Some([payload[0], payload[1], payload[2], payload[3]])
            }
            OPT_SUBNET_MASK if len == 4 => {
                parsed.subnet_mask = Some([payload[0], payload[1], payload[2], payload[3]])
            }
            OPT_ROUTER if len == 4 => {
                parsed.router = Some([payload[0], payload[1], payload[2], payload[3]])
            }
            OPT_DNS if len == 4 => parsed.dns = Some([payload[0], payload[1], payload[2], payload[3]]),
            OPT_BROADCAST if len == 4 => {
                parsed.broadcast = Some([payload[0], payload[1], payload[2], payload[3]])
            }
            OPT_LEASE_TIME if len == 4 => {
                parsed.lease_secs = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
            }
            // Anything else, and any option whose length does not match its
            // defined size, is skipped rather than treated as fatal: a server is
            // entitled to send options this client does not know.
            _ => {}
        }
    }
    Some(parsed)
}

/// Parse a BOOTP/DHCP message.
///
/// Takes the message itself, not a UDP datagram: `udp::process_packet` has
/// already validated the UDP length and stripped the 8-byte header, so the
/// BOOTP message starts at offset 0 here. The caller is `wait_for`, which sees
/// a `UdpDatagram` whose payload is exactly this.
pub fn parse_bootp(bootp: &[u8]) -> Option<Offer> {
    if bootp.len() < BOOTP_HEADER_LEN {
        return None;
    }
    if bootp[BOOTP_FIXED_LEN..BOOTP_HEADER_LEN] != MAGIC_COOKIE {
        return None;
    }
    if bootp[0] != BOOTREPLY {
        return None;
    }

    let xid = u32::from_be_bytes([bootp[4], bootp[5], bootp[6], bootp[7]]);
    let offered_ip = [bootp[16], bootp[17], bootp[18], bootp[19]];
    let siaddr = [bootp[20], bootp[21], bootp[22], bootp[23]];

    let options = parse_options(&bootp[BOOTP_HEADER_LEN..])?;

    // RFC 2131: the server identifier is authoritative, but a server that leaves
    // option 54 empty may identify itself through `siaddr`.
    let server_id = options.server_id.or({
        if siaddr != [0, 0, 0, 0] {
            Some(siaddr)
        } else {
            None
        }
    });

    Some(Offer {
        offered_ip,
        server_id,
        options,
        xid,
    })
}

/// Whether an address is one this client must refuse to configure.
///
/// Each case below is a way a reply can leave the host with no usable route:
/// `0.0.0.0` is "any", broadcast has no host behind it, multicast is not a
/// unicast destination, and our own address means the server is talking about
/// somebody else.
pub fn address_is_usable(address: [u8; 4], our_mac: [u8; 6]) -> Result<(), &'static str> {
    if address == [0, 0, 0, 0] {
        return Err("lease address is 0.0.0.0");
    }
    if address == [255, 255, 255, 255] {
        return Err("lease address is the broadcast address");
    }
    if address[0] & 0xF0 == 0xE0 {
        return Err("lease address is multicast");
    }
    if address[0] == 127 {
        return Err("lease address is loopback");
    }
    if our_mac == [0; 6] {
        return Err("no MAC address");
    }
    Ok(())
}

/// Decide whether an offer may be configured, and produce the lease.
///
/// Returns the refusal reason rather than a bool, because a rejected lease is
/// worth explaining: a server offering an address this client refuses is either
/// misconfigured or not a server at all.
pub fn lease_from_offer(offer: &Offer, our_ip: Option<[u8; 4]>, our_mac: [u8; 6]) -> Result<Lease, &'static str> {
    address_is_usable(offer.offered_ip, our_mac)?;

    if offer.options.message_type != Some(DHCP_OFFER)
        && offer.options.message_type != Some(DHCP_ACK)
    {
        return Err("reply is not an offer or an ack");
    }

    // Without an identifier there is nobody to send the request to, and a lease
    // whose server is unknown cannot be renewed.
    let server_id = offer.server_id.ok_or("reply carried no server identifier")?;
    address_is_usable(server_id, our_mac)?;

    // A netmask is required. Guessing /24 when the server meant something else
    // either hides reachable hosts or routes everything to the gateway.
    let netmask = offer.options.subnet_mask.ok_or("offer carried no subnet mask")?;
    if netmask == [0, 0, 0, 0] {
        return Err("offer carried a zero subnet mask");
    }
    // The mask must be contiguous; a non-contiguous mask is never legitimate.
    let mut seen_zero = false;
    for byte in netmask {
        for bit in (0..8).rev() {
            if byte & (1 << bit) != 0 {
                if seen_zero {
                    return Err("offer carried a non-contiguous subnet mask");
                }
            } else {
                seen_zero = true;
            }
        }
    }

    // The address must not be the network or broadcast address of the subnet the
    // mask describes, either of which is unusable as a host address.
    let network = [
        offer.offered_ip[0] & netmask[0],
        offer.offered_ip[1] & netmask[1],
        offer.offered_ip[2] & netmask[2],
        offer.offered_ip[3] & netmask[3],
    ];
    if offer.offered_ip == network {
        return Err("offered address is the network address");
    }
    let broadcast = [
        network[0] | !netmask[0],
        network[1] | !netmask[1],
        network[2] | !netmask[2],
        network[3] | !netmask[3],
    ];
    if offer.offered_ip == broadcast {
        return Err("offered address is the broadcast address of its subnet");
    }

    if let Some(current) = our_ip {
        if current == offer.offered_ip {
            // Not fatal: the server may simply be re-offering what we hold. It
            // is accepted so a renewal does not require special handling.
            crate::net_log!("DHCP: offer matches our current address");
        }
    }

    // A router or DNS server that is 0.0.0.0 is worse than absent: `ifconfig`
    // would store it and every packet would be routed to nowhere.
    let router = offer.options.router.filter(|r| *r != [0, 0, 0, 0]);
    let dns = offer.options.dns.filter(|d| *d != [0, 0, 0, 0]);

    Ok(Lease {
        address: offer.offered_ip,
        netmask,
        router,
        dns,
        server_id: Some(server_id),
        lease_secs: offer.options.lease_secs,
    })
}

/// Build a DHCP message.
///
/// `message_type` and the options after it are appended in the order given by
/// `extra`, so the caller controls which optional fields are present.
pub fn build_message(
    message_type: u8,
    xid: u32,
    our_mac: [u8; 6],
    requested_ip: Option<[u8; 4]>,
    server_id: Option<[u8; 4]>,
) -> Vec<u8> {
    let mut message = vec![0u8; BOOTP_FIXED_LEN];
    message[0] = BOOTREQUEST;
    message[1] = 1; // htype: Ethernet
    message[2] = 6; // hlen: MAC length
    // message[3] hops = 0
    message[4..8].copy_from_slice(&xid.to_be_bytes());
    // secs left at 0: the client is not rate-limiting yet.
    message[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    // ciaddr/yiaddr/siaddr/giaddr stay 0: a client that has no lease asks.
    message[28..34].copy_from_slice(&our_mac);
    // sname and file stay empty.
    message.extend_from_slice(&MAGIC_COOKIE);

    message.push(OPT_MESSAGE_TYPE);
    message.push(1);
    message.push(message_type);

    if let Some(ip) = requested_ip {
        message.push(OPT_REQUESTED_IP);
        message.push(4);
        message.extend_from_slice(&ip);
    }
    if let Some(server) = server_id {
        message.push(OPT_SERVER_ID);
        message.push(4);
        message.extend_from_slice(&server);
    }

    // Only in a discover: ask for what this client can actually use.
    if message_type == DHCP_DISCOVER {
        message.push(OPT_PARAM_REQUEST_LIST);
        message.push(4);
        message.push(OPT_SUBNET_MASK);
        message.push(OPT_ROUTER);
        message.push(OPT_DNS);
        message.push(OPT_BROADCAST);
    }

    message.push(OPT_END);
    message
}

/// Wrap a DHCP message in IPv4 and UDP headers addressed for a pre-address client.
fn wrap_for_broadcast(message: &[u8]) -> Vec<u8> {
    let udp_len = 8 + message.len();
    let total_len = 20 + udp_len;

    let mut packet = Vec::with_capacity(total_len);
    packet.push(0x45); // IPv4, 5 words of header
    packet.push(0); // DSCP/ECN
    packet.extend_from_slice(&(total_len as u16).to_be_bytes());
    packet.extend_from_slice(&crate::entropy::u16().to_be_bytes()); // identification
    packet.extend_from_slice(&[0, 0]); // flags/fragment: none
    packet.push(64); // TTL
    packet.push(17); // UDP
    packet.extend_from_slice(&[0, 0]); // checksum placeholder
    packet.extend_from_slice(&[0, 0, 0, 0]); // source: unknown, per RFC 2131
    packet.extend_from_slice(&[255, 255, 255, 255]); // destination: broadcast

    let ip_header_len = 20;
    let checksum = super::ip::IpHeader::checksum_bytes(&packet[..ip_header_len]);
    packet[10] = (checksum >> 8) as u8;
    packet[11] = (checksum & 0xFF) as u8;

    // UDP checksum is optional over IPv4 and is left zero, as RFC 768 allows.
    packet.extend_from_slice(&DHCP_CLIENT_PORT.to_be_bytes());
    packet.extend_from_slice(&DHCP_SERVER_PORT.to_be_bytes());
    packet.extend_from_slice(&(udp_len as u16).to_be_bytes());
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(message);
    packet
}

/// Send a DHCP message with no address and no ARP.
///
/// This cannot go through `ip::send_packet`: that resolves the next hop over ARP
/// and consults the configured address to decide what the next hop is, and at
/// this point there is no address to configure and nothing cached to resolve
/// with. The frame goes out with a broadcast destination MAC instead.
fn send_broadcast(message: &[u8]) -> Result<(), &'static str> {
    if message.len() > MAX_DHCP_PACKET {
        return Err("DHCP message too large");
    }
    let packet = wrap_for_broadcast(message);
    super::ethernet::send_frame([0xFF; 6], super::ethernet::ETHERTYPE_IP, &packet)
}

/// Counters for `netstat`.
static STATS: Mutex<DhcpStats> = Mutex::new(DhcpStats {
    discovers_sent: 0,
    requests_sent: 0,
    offers_received: 0,
    acks_received: 0,
    naks_received: 0,
    malformed_dropped: 0,
    leases_rejected: 0,
    last_reject_reason: None,
    leases_obtained: 0,
    configured: false,
});

/// Snapshot of the negotiation counters.
pub fn stats() -> DhcpStats {
    *STATS.lock()
}

/// Pump packets for `window_ms`, returning the first matching reply.
///
/// DHCP is a blocking protocol: the client must wait for a reply, and the only
/// thing that can deliver it is the packet pump. `window_ms` bounds the wait so a
/// silent network cannot hang the caller.
fn wait_for<F>(window_ms: u64, mut accept: F) -> Option<Offer>
where
    F: FnMut(&Offer) -> bool,
{
    let started = crate::shell::monotonic_ms();
    loop {
        crate::net::process_packets();
        // Drain anything addressed to the client port.
        while let Some(datagram) = crate::net::udp::receive(DHCP_CLIENT_PORT) {
            let Some(offer) = parse_bootp(&datagram.payload) else {
                STATS.lock().malformed_dropped += 1;
                continue;
            };
            if accept(&offer) {
                return Some(offer);
            }
        }
        if crate::shell::monotonic_ms().saturating_sub(started) >= window_ms {
            return None;
        }
        crate::shell::increment_tick();
        core::hint::spin_loop();
    }
}

/// Run DORA and configure the interface on success.
///
/// Returns the lease. On failure the previously configured address is left
/// untouched: a failed renewal should not disconnect a working host.
pub fn configure() -> Result<Lease, &'static str> {
    let our_mac = crate::drivers::e1000::mac_address().ok_or("No MAC address")?;
    let xid = crate::entropy::u32();

    // ── Discover ──────────────────────────────────────────────────────
    let mut offered: Option<Offer> = None;
    let mut backoff = RETRY_MIN_MS;
    for attempt in 1..=DISCOVER_ATTEMPTS {
        let message = build_message(DHCP_DISCOVER, xid, our_mac, None, None);
        STATS.lock().discovers_sent += 1;
        if let Err(error) = send_broadcast(&message) {
            crate::net_log!("DHCP: discover send failed: {}", error);
        } else {
            crate::serial_println!(
                "DHCP: discover #{} (xid {:#010x})",
                attempt,
                xid
            );
        }
        if let Some(offer) = wait_for(backoff, |offer| offer.xid == xid) {
            offered = Some(offer);
            break;
        }
        backoff = core::cmp::min(backoff * 2, RETRY_MAX_MS);
    }

    let Some(offer) = offered else {
        return Err("no DHCP offer received");
    };
    STATS.lock().offers_received += 1;
    crate::serial_println!(
        "DHCP: offer {}.{}.{}.{} from server {}.{}.{}.{}",
        offer.offered_ip[0],
        offer.offered_ip[1],
        offer.offered_ip[2],
        offer.offered_ip[3],
        offer.server_id.map(|s| s[0]).unwrap_or(0),
        offer.server_id.map(|s| s[1]).unwrap_or(0),
        offer.server_id.map(|s| s[2]).unwrap_or(0),
        offer.server_id.map(|s| s[3]).unwrap_or(0),
    );

    // Validate before spending a second round trip on an offer we would refuse.
    let lease = match lease_from_offer(&offer, crate::net::ip::get_ip_address(), our_mac) {
        Ok(lease) => lease,
        Err(reason) => {
            let mut stats = STATS.lock();
            stats.leases_rejected += 1;
            stats.last_reject_reason = Some(reason);
            crate::serial_println!("DHCP: refused offer: {}", reason);
            return Err(reason);
        }
    };

    let Some(server_id) = lease.server_id else {
        return Err("no server identifier to request from");
    };

    // ── Request ───────────────────────────────────────────────────────
    let mut backoff = RETRY_MIN_MS;
    for attempt in 1..=REQUEST_ATTEMPTS {
        let message = build_message(
            DHCP_REQUEST,
            xid,
            our_mac,
            Some(lease.address),
            Some(server_id),
        );
        STATS.lock().requests_sent += 1;
        if let Err(error) = send_broadcast(&message) {
            crate::net_log!("DHCP: request send failed: {}", error);
        } else {
            crate::serial_println!(
                "DHCP: request #{} for {}.{}.{}.{}",
                attempt,
                lease.address[0],
                lease.address[1],
                lease.address[2],
                lease.address[3]
            );
        }
        if let Some(reply) = wait_for(backoff, |reply| reply.xid == xid) {
            match reply.options.message_type {
                Some(DHCP_ACK) => {
                    STATS.lock().acks_received += 1;
                    // The ack may carry a different address than the offer; the
                    // server has the final word, so re-validate rather than
                    // assuming.
                    let confirmed = match lease_from_offer(
                        &Offer {
                            offered_ip: reply.offered_ip,
                            ..offer
                        },
                        crate::net::ip::get_ip_address(),
                        our_mac,
                    ) {
                        Ok(confirmed) => confirmed,
                        Err(reason) => {
                            let mut stats = STATS.lock();
                            stats.leases_rejected += 1;
                            stats.last_reject_reason = Some(reason);
                            crate::serial_println!("DHCP: refused ack: {}", reason);
                            return Err(reason);
                        }
                    };
                    apply(&confirmed);
                    return Ok(confirmed);
                }
                Some(DHCP_NAK) => {
                    STATS.lock().naks_received += 1;
                    // The server is telling us the address is not ours. Any
                    // lease we hold is invalid, so drop it rather than keep
                    // using an address the server has reclaimed.
                    crate::serial_println!("DHCP: server sent NAK, dropping address");
                    STATS.lock().configured = false;
                    return Err("DHCP server refused the requested address (NAK)");
                }
                _ => {}
            }
        }
        backoff = core::cmp::min(backoff * 2, RETRY_MAX_MS);
    }

    Err("no DHCP acknowledgement received")
}

/// Configure the interface with a validated lease.
fn apply(lease: &Lease) {
    crate::net::ip::configure(
        lease.address,
        lease.netmask,
        lease.router,
    );
    if let Some(dns) = lease.dns {
        crate::net::dns::set_server(dns);
    } else {
        crate::net::dns::set_server(crate::net::dns::DEFAULT_SERVER);
    }
    let mut stats = STATS.lock();
    stats.leases_obtained += 1;
    stats.configured = true;
    crate::serial_println!(
        "DHCP: lease for {}.{}.{}.{} (mask {}.{}.{}.{}, lease {}s)",
        lease.address[0],
        lease.address[1],
        lease.address[2],
        lease.address[3],
        lease.netmask[0],
        lease.netmask[1],
        lease.netmask[2],
        lease.netmask[3],
        lease.lease_secs
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    fn ip(b: [u8; 4]) -> [u8; 4] {
        b
    }

    /// Build a server reply in the shape `wait_for` receives it: a BOOTP message
/// with no UDP header, because `udp::process_packet` strips that first.
fn reply(xid: u32, yiaddr: [u8; 4], options: &[u8]) -> Vec<u8> {
        let mut bootp = vec![0u8; BOOTP_FIXED_LEN];
        bootp[0] = BOOTREPLY;
        bootp[1] = 1;
        bootp[2] = 6;
        bootp[4..8].copy_from_slice(&xid.to_be_bytes());
        bootp[16..20].copy_from_slice(&yiaddr);
        bootp[28..34].copy_from_slice(&MAC);
        bootp.extend_from_slice(&MAGIC_COOKIE);
        bootp.extend_from_slice(options);
        bootp.push(OPT_END);
        bootp
    }

    fn opt(code: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![code, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    fn good_options() -> Vec<u8> {
        let mut out = opt(OPT_MESSAGE_TYPE, &[DHCP_OFFER]);
        out.extend(opt(OPT_SERVER_ID, &[10, 0, 2, 2]));
        out.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        out.extend(opt(OPT_ROUTER, &[10, 0, 2, 2]));
        out.extend(opt(OPT_DNS, &[10, 0, 2, 3]));
        out.extend(opt(OPT_LEASE_TIME, &[0, 0, 0x0e, 0x10]));
        out
    }

    // ── option parsing ────────────────────────────────────────────────

    #[test]
    fn options_parse_in_the_well_formed_case() {
        let parsed = parse_options(&good_options()).expect("valid options must parse");
        assert_eq!(parsed.message_type, Some(DHCP_OFFER));
        assert_eq!(parsed.server_id, Some([10, 0, 2, 2]));
        assert_eq!(parsed.subnet_mask, Some([255, 255, 255, 0]));
        assert_eq!(parsed.router, Some([10, 0, 2, 2]));
        assert_eq!(parsed.dns, Some([10, 0, 2, 3]));
        assert_eq!(parsed.lease_secs, 3600);
    }

    #[test]
    fn option_running_past_the_end_is_rejected() {
        // Length byte claims 10 bytes with only 3 present.
        let mut options = vec![OPT_SUBNET_MASK, 10, 255, 255, 255];
        assert!(
            parse_options(&options).is_none(),
            "a truncated option must not read past the buffer"
        );

        // Length byte present but no payload at all.
        options = vec![OPT_SUBNET_MASK, 4];
        assert!(parse_options(&options).is_none());
    }

    #[test]
    fn option_with_no_length_byte_is_rejected() {
        assert!(parse_options(&[OPT_SUBNET_MASK]).is_none());
    }

    #[test]
    fn unknown_options_are_skipped_not_fatal() {
        // A server is entitled to send options this client does not know.
        let mut options = good_options();
        options.extend(opt(200, &[1, 2, 3, 4, 5]));
        options.extend(opt(201, &[9, 9]));
        let parsed = parse_options(&options).expect("unknown options must not abort parsing");
        assert_eq!(parsed.message_type, Some(DHCP_OFFER));
        assert_eq!(parsed.dns, Some([10, 0, 2, 3]), "later options still parse");
    }

    #[test]
    fn wrongly_sized_known_option_is_ignored() {
        let mut options = vec![OPT_MESSAGE_TYPE];
        options.push(1);
        options.push(DHCP_OFFER);
        // A server id of the wrong length must not be half-read.
        options.extend([OPT_SERVER_ID, 6, 10, 0, 2, 2, 0, 0]);
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        let parsed = parse_options(&options).unwrap();
        assert_eq!(parsed.message_type, Some(DHCP_OFFER));
        assert_eq!(parsed.server_id, None);
        assert_eq!(parsed.subnet_mask, Some([255, 255, 255, 0]));
    }

    #[test]
    fn padding_and_end_are_handled() {
        let options = [0u8, OPT_MESSAGE_TYPE, 1, DHCP_OFFER, 0, 0, OPT_END, 42, 42];
        let parsed = parse_options(&options).unwrap();
        assert_eq!(parsed.message_type, Some(DHCP_OFFER));
    }

    #[test]
    fn empty_options_parse_to_nothing() {
        let parsed = parse_options(&[OPT_END]).unwrap();
        assert_eq!(parsed.message_type, None);
        assert_eq!(parsed.subnet_mask, None);
    }

    // ── packet parsing ────────────────────────────────────────────────

    #[test]
    fn a_well_formed_reply_parses() {
        let packet = reply(0xDEAD_BEEF, [10, 0, 2, 15], &good_options());
        let offer = parse_bootp(&packet).expect("valid reply must parse");
        assert_eq!(offer.xid, 0xDEAD_BEEF);
        assert_eq!(offer.offered_ip, [10, 0, 2, 15]);
        assert_eq!(offer.server_id, Some([10, 0, 2, 2]));
    }

    #[test]
    fn reply_without_the_magic_cookie_is_rejected() {
        let mut packet = reply(1, [10, 0, 2, 15], &good_options());
        packet[BOOTP_FIXED_LEN] = 0;
        assert!(parse_bootp(&packet).is_none());
    }

    #[test]
    fn reply_with_a_wrong_opcode_is_rejected() {
        let mut packet = reply(1, [10, 0, 2, 15], &good_options());
        packet[0] = BOOTREQUEST;
        assert!(parse_bootp(&packet).is_none());
    }

    #[test]
    fn truncation_can_only_remove_options_never_change_the_lease() {
        let packet = reply(1, [10, 0, 2, 15], &good_options());
        let full = lease_from_offer(&parse_bootp(&packet).unwrap(), None, MAC).unwrap();

        // Anything shorter than the fixed header plus cookie is not a reply.
        for len in 0..BOOTP_HEADER_LEN {
            assert!(
                parse_bootp(&packet[..len]).is_none(),
                "a {} byte prefix must not parse",
                len
            );
        }

        // A truncation inside the options may still parse, because the option
        // walk stops at the end of the buffer rather than reading past it. What
        // must never happen is truncation *changing* an option: a prefix may only
        // be missing options the full message had, never carry a different value
        // for one, which is what would distinguish a real read from a half-read.
        for len in BOOTP_HEADER_LEN..packet.len() {
            if let Some(offer) = parse_bootp(&packet[..len]) {
                assert_eq!(
                    offer.offered_ip, full.address,
                    "a {}-byte prefix changed the offered address",
                    len
                );
                assert_eq!(offer.xid, 1);
                // Every option a prefix yields must be one the full message also
                // carried, with the same value. A prefix may only be missing
                // options, never have different ones: that is what separates a
                // genuine read from a half-read of a truncated option.
                for (name, got, expected) in [
                    (
                        "subnet mask",
                        offer.options.subnet_mask,
                        Some(full.netmask),
                    ),
                    ("router", offer.options.router, full.router),
                    ("dns", offer.options.dns, full.dns),
                ] {
                    assert!(
                        got == expected || got.is_none(),
                        "a {}-byte prefix changed the {} option to {:?}",
                        len,
                        name,
                        got
                    );
                }
            }
        }
    }

    #[test]
    fn a_prefix_missing_the_netmask_yields_no_lease() {
        // The case that matters: cut after the message type and server
        // identifier, before the subnet mask. Three bytes of message type plus
        // six of server identifier is the cut point.
        let packet = reply(1, [10, 0, 2, 15], &good_options());
        let cut = BOOTP_HEADER_LEN + 3 + 6;
        let offer = parse_bootp(&packet[..cut]).expect("the fixed header is intact");
        assert_eq!(offer.options.message_type, Some(DHCP_OFFER));
        assert_eq!(offer.server_id, Some([10, 0, 2, 2]));
        assert_eq!(
            offer.options.subnet_mask, None,
            "the mask must be absent, not half-read"
        );
        assert!(lease_from_offer(&offer, None, MAC).is_err());
    }

    #[test]
    fn a_reply_with_no_options_parses_but_yields_no_lease() {
        // Not malformed, just useless: the fixed header is intact. It must still
        // be refused before it is configured, which is what keeps a bare reply
        // from installing a lease with no mask or server.
        let packet = reply(1, [10, 0, 2, 15], &[]);
        let offer = parse_bootp(&packet).expect("an optionless reply is well formed");
        assert_eq!(offer.offered_ip, [10, 0, 2, 15]);
        assert_eq!(offer.server_id, None);
        assert!(lease_from_offer(&offer, None, MAC).is_err());
    }

    #[test]
    fn trailing_bytes_after_the_end_option_are_ignored() {
        // A server may pad to the 576 byte minimum; the extra bytes are not
        // options and must not be walked as such.
        let mut packet = reply(1, [10, 0, 2, 15], &good_options());
        packet.extend([0xAA; 64]);
        let offer = parse_bootp(&packet).expect("padding must not break parsing");
        assert_eq!(offer.offered_ip, [10, 0, 2, 15]);
        assert_eq!(offer.options.dns, Some([10, 0, 2, 3]));
    }

    #[test]
    fn siaddr_is_a_fallback_server_identifier() {
        let mut packet = reply(1, [10, 0, 2, 15], &good_options());
        // Replace option 54 with nothing and set siaddr instead.
        let mut options: Vec<u8> = Vec::new();
        options.extend(opt(OPT_MESSAGE_TYPE, &[DHCP_OFFER]));
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        packet.truncate(BOOTP_FIXED_LEN);
        packet[20..24].copy_from_slice(&[10, 0, 2, 7]);
        packet.extend_from_slice(&MAGIC_COOKIE);
        packet.extend_from_slice(&options);

        let offer = parse_bootp(&packet).unwrap();
        assert_eq!(offer.server_id, Some([10, 0, 2, 7]));
    }

    // ── lease validation ──────────────────────────────────────────────

    #[test]
    fn a_good_offer_produces_a_lease() {
        let packet = reply(1, [10, 0, 2, 15], &good_options());
        let offer = parse_bootp(&packet).unwrap();
        let lease = lease_from_offer(&offer, None, MAC).unwrap();
        assert_eq!(lease.address, [10, 0, 2, 15]);
        assert_eq!(lease.netmask, [255, 255, 255, 0]);
        assert_eq!(lease.router, Some([10, 0, 2, 2]));
        assert_eq!(lease.dns, Some([10, 0, 2, 3]));
        assert_eq!(lease.lease_secs, 3600);
    }

    #[test]
    fn zero_broadcast_multicast_and_loopback_addresses_are_refused() {
        for (address, why) in [
            ([0, 0, 0, 0], "unspecified"),
            ([255, 255, 255, 255], "broadcast"),
            ([224, 0, 0, 1], "multicast"),
            ([127, 0, 0, 1], "loopback"),
        ] {
            let err = address_is_usable(ip(address), MAC).unwrap_err();
            assert!(!err.is_empty(), "{} must be refused", why);
        }
        assert!(address_is_usable([10, 0, 2, 15], MAC).is_ok());
    }

    #[test]
    fn a_zero_netmask_is_refused() {
        let mut options = good_options();
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        options.extend(opt(OPT_SUBNET_MASK, &[0, 0, 0, 0]));
        let packet = reply(1, [10, 0, 2, 15], &options);
        let offer = parse_bootp(&packet).unwrap();
        assert_eq!(
            lease_from_offer(&offer, None, MAC).unwrap_err(),
            "offer carried a zero subnet mask"
        );
    }

    #[test]
    fn a_non_contiguous_netmask_is_refused() {
        let mut options = good_options();
        options.extend(opt(OPT_SUBNET_MASK, &[255, 0, 255, 0]));
        let packet = reply(1, [10, 0, 2, 15], &options);
        let offer = parse_bootp(&packet).unwrap();
        assert!(lease_from_offer(&offer, None, MAC)
            .unwrap_err()
            .contains("contiguous"));
    }

    #[test]
    fn a_missing_netmask_is_refused() {
        let mut options = vec![OPT_MESSAGE_TYPE];
        options.push(1);
        options.push(DHCP_OFFER);
        options.extend(opt(OPT_SERVER_ID, &[10, 0, 2, 2]));
        let packet = reply(1, [10, 0, 2, 15], &options);
        let offer = parse_bootp(&packet).unwrap();
        assert_eq!(
            lease_from_offer(&offer, None, MAC).unwrap_err(),
            "offer carried no subnet mask"
        );
    }

    #[test]
    fn a_missing_server_identifier_is_refused() {
        let mut options = vec![OPT_MESSAGE_TYPE];
        options.push(1);
        options.push(DHCP_OFFER);
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        let packet = reply(1, [10, 0, 2, 15], &options);
        let offer = parse_bootp(&packet).unwrap();
        assert_eq!(offer.server_id, None);
        assert_eq!(
            lease_from_offer(&offer, None, MAC).unwrap_err(),
            "reply carried no server identifier"
        );
    }

    #[test]
    fn the_network_and_broadcast_addresses_of_the_lease_are_refused() {
        for address in [[10, 0, 2, 0], [10, 0, 2, 255]] {
            let packet = reply(1, address, &good_options());
            let offer = parse_bootp(&packet).unwrap();
            let err = lease_from_offer(&offer, None, MAC).unwrap_err();
            assert!(
                err.contains("network address") || err.contains("broadcast"),
                "{:?} refused for the wrong reason: {}",
                address,
                err
            );
        }
    }

    #[test]
    fn the_subnet_mask_in_the_offer_is_the_one_used() {
        // The network and broadcast checks must follow the mask the server sent,
        // not an assumed /24. Under a /16, 10.0.1.255 is an ordinary host
        // address; under a /24 it is the broadcast.
        let mut options = good_options();
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 0, 0]));
        let packet = reply(1, [10, 0, 1, 255], &options);
        let offer = parse_bootp(&packet).unwrap();
        let lease = lease_from_offer(&offer, None, MAC)
            .expect("10.0.1.255 is a valid host under a /16");
        assert_eq!(lease.netmask, [255, 255, 0, 0]);
        assert_eq!(lease.address, [10, 0, 1, 255]);

        // Same address, /24: now it is the broadcast and must be refused.
        let mut options = good_options();
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        let packet = reply(1, [10, 0, 1, 255], &options);
        let offer = parse_bootp(&packet).unwrap();
        assert!(lease_from_offer(&offer, None, MAC)
            .unwrap_err()
            .contains("broadcast"));
    }

    #[test]
    fn zero_valued_router_and_dns_are_treated_as_absent() {
        let mut options = vec![OPT_MESSAGE_TYPE];
        options.push(1);
        options.push(DHCP_OFFER);
        options.extend(opt(OPT_SERVER_ID, &[10, 0, 2, 2]));
        options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
        options.extend(opt(OPT_ROUTER, &[0, 0, 0, 0]));
        options.extend(opt(OPT_DNS, &[0, 0, 0, 0]));
        let packet = reply(1, [10, 0, 2, 15], &options);
        let offer = parse_bootp(&packet).unwrap();
        let lease = lease_from_offer(&offer, None, MAC).unwrap();
        assert_eq!(
            lease.router, None,
            "a 0.0.0.0 gateway would black-hole every packet"
        );
        assert_eq!(lease.dns, None);
    }

    #[test]
    fn a_reply_that_is_neither_offer_nor_ack_is_refused() {
        for message_type in [DHCP_DISCOVER, DHCP_REQUEST, DHCP_NAK, 99] {
            let mut options = vec![OPT_MESSAGE_TYPE];
            options.push(1);
            options.push(message_type);
            options.extend(opt(OPT_SERVER_ID, &[10, 0, 2, 2]));
            options.extend(opt(OPT_SUBNET_MASK, &[255, 255, 255, 0]));
            let packet = reply(1, [10, 0, 2, 15], &options);
            let offer = parse_bootp(&packet).unwrap();
            assert_eq!(
                lease_from_offer(&offer, None, MAC).unwrap_err(),
                "reply is not an offer or an ack"
            );
        }
    }

    // ── message construction ──────────────────────────────────────────

    #[test]
    fn a_discover_round_trips_through_our_own_parser() {
        // A message built for the wire must be understood by the same bounds
        // checks that guard a server's reply.
        let message = build_message(DHCP_DISCOVER, 0xCAFEF00D, MAC, None, None);
        assert_eq!(&message[0..4], &[BOOTREQUEST, 1, 6, 0]);
        assert_eq!(&message[4..8], &0xCAFEF00Du32.to_be_bytes());
        assert_eq!(&message[10..12], &FLAG_BROADCAST.to_be_bytes());
        assert_eq!(&message[28..34], &MAC);
        assert_eq!(&message[BOOTP_FIXED_LEN..BOOTP_HEADER_LEN], &MAGIC_COOKIE);

        let options = parse_options(&message[BOOTP_HEADER_LEN..]).unwrap();
        assert_eq!(options.message_type, Some(DHCP_DISCOVER));
        assert_eq!(options.requested_ip, None);
        assert_eq!(message[message.len() - 1], OPT_END);
    }

    #[test]
    fn a_request_carries_the_requested_address_and_server() {
        let message = build_message(
            DHCP_REQUEST,
            1,
            MAC,
            Some([10, 0, 2, 15]),
            Some([10, 0, 2, 2]),
        );
        let options = parse_options(&message[BOOTP_HEADER_LEN..]).unwrap();
        assert_eq!(options.message_type, Some(DHCP_REQUEST));
        assert_eq!(options.requested_ip, Some([10, 0, 2, 15]));
        assert_eq!(options.server_id, Some([10, 0, 2, 2]));
    }

    #[test]
    fn a_discover_asks_for_what_this_client_can_use() {
        let message = build_message(DHCP_DISCOVER, 1, MAC, None, None);
        let requested = message
            .windows(2)
            .position(|w| w[0] == OPT_PARAM_REQUEST_LIST)
            .expect("a discover must send a parameter request list");
        let len = message[requested + 1] as usize;
        let list = &message[requested + 2..requested + 2 + len];
        for wanted in [OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS, OPT_BROADCAST] {
            assert!(list.contains(&wanted), "option {} not requested", wanted);
        }
    }

    #[test]
    fn built_messages_stay_within_the_minimum_datagram() {
        for message_type in [DHCP_DISCOVER, DHCP_REQUEST] {
            let message = build_message(message_type, 1, MAC, Some([1, 2, 3, 4]), Some([5, 6, 7, 8]));
            assert!(
                message.len() + 28 <= MAX_DHCP_PACKET,
                "message larger than the 576 byte minimum"
            );
        }
    }

    #[test]
    fn the_broadcast_wrapper_produces_a_well_formed_datagram() {
        let message = build_message(DHCP_DISCOVER, 1, MAC, None, None);
        let mut packet = wrap_for_broadcast(&message);
        assert_eq!(packet[0], 0x45, "IPv4 with a 20 byte header");
        assert_eq!(packet[9], 17, "protocol must be UDP");
        assert_eq!(&packet[12..16], &[0, 0, 0, 0], "source is unknown pre-lease");
        assert_eq!(&packet[16..20], &[255, 255, 255, 255]);
        assert_eq!(u16::from_be_bytes([packet[2], packet[3]]) as usize, packet.len());
        assert_eq!(
            u16::from_be_bytes([packet[20], packet[21]]),
            DHCP_CLIENT_PORT
        );
        assert_eq!(
            u16::from_be_bytes([packet[22], packet[23]]),
            DHCP_SERVER_PORT
        );
        // The header checksum must verify, or the server drops the datagram.
        // Verification sums the stored field in, unlike the computation above,
        // and both zero and all-ones are accepted as correct.
        let verified = super::super::ip::IpHeader::verify_checksum_bytes(&packet[..20]);
        assert!(
            verified == 0 || verified == 0xFFFF,
            "verification sum {:#x} means the stored checksum is wrong",
            verified
        );

        // And a single flipped bit must be detected.
        packet[4] ^= 0xFF;
        let corrupt = super::super::ip::IpHeader::verify_checksum_bytes(&packet[..20]);
        assert!(
            corrupt != 0 && corrupt != 0xFFFF,
            "a corrupt header must not verify"
        );
    }
}
