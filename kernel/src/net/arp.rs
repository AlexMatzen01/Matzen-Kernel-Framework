//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ARP (Address Resolution Protocol)

use alloc::collections::BTreeMap;
use lazy_static::lazy_static;
use spin::Mutex;

const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ARP_TIMEOUT_MAX_POLLS: u64 = 2000;

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

lazy_static! {
    static ref ARP_CACHE: Mutex<BTreeMap<[u8; 4], [u8; 6]>> = Mutex::new(BTreeMap::new());
}

pub fn process_packet(packet: &[u8], _src_mac: [u8; 6]) {
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

    // Update ARP cache
    ARP_CACHE.lock().insert(arp.sender_ip, arp.sender_mac);

    if opcode == ARP_REQUEST {
        // Check if request is for our IP
        if let Some(our_ip) = crate::net::ip::get_ip_address() {
            if arp.target_ip == our_ip {
                send_arp_reply(arp.sender_mac, arp.sender_ip);
            }
        }
    }
}

pub fn send_arp_request(target_ip: [u8; 4]) -> Result<(), &'static str> {
    let our_mac = crate::drivers::e1000::mac_address().ok_or("No MAC address")?;
    let our_ip = crate::net::ip::get_ip_address().ok_or("No IP address")?;

    let arp = ArpPacket {
        hw_type: 1u16.to_be(),
        proto_type: 0x0800u16.to_be(),
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
    crate::net::ethernet::send_frame(
        broadcast_mac,
        crate::net::ethernet::ETHERTYPE_ARP,
        packet_bytes,
    )
}

fn send_arp_reply(target_mac: [u8; 6], target_ip: [u8; 4]) {
    let our_mac = match crate::drivers::e1000::mac_address() {
        Some(mac) => mac,
        None => return,
    };
    let our_ip = match crate::net::ip::get_ip_address() {
        Some(ip) => ip,
        None => return,
    };

    let arp = ArpPacket {
        hw_type: 1u16.to_be(),
        proto_type: 0x0800u16.to_be(),
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

    if let Err(error) = crate::net::ethernet::send_frame(
        target_mac,
        crate::net::ethernet::ETHERTYPE_ARP,
        packet_bytes,
    ) {
        crate::serial_println!("ARP reply send failed: {}", error);
    }
}

pub fn lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    ARP_CACHE.lock().get(&ip).copied()
}

pub fn entries() -> alloc::vec::Vec<([u8; 4], [u8; 6])> {
    ARP_CACHE
        .lock()
        .iter()
        .map(|(ip, mac)| (*ip, *mac))
        .collect()
}

/// Resolve an on-link IPv4 address. The bounded wait pumps RX packets so an
/// ARP reply is handled even while a foreground shell command is active.
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
