//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ARP (Address Resolution Protocol)

use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ArpPacket {
    pub hw_type: u16,      // Hardware type (Ethernet = 1)
    pub proto_type: u16,   // Protocol type (IPv4 = 0x0800)
    pub hw_size: u8,       // Hardware address length (6 for MAC)
    pub proto_size: u8,    // Protocol address length (4 for IPv4)
    pub opcode: u16,       // Operation (1 = request, 2 = reply)
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

    let arp = unsafe {
        core::ptr::read_unaligned(packet.as_ptr() as *const ArpPacket)
    };

    let opcode = u16::from_be(arp.opcode);
    
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
    crate::net::ethernet::send_frame(broadcast_mac, crate::net::ethernet::ETHERTYPE_ARP, packet_bytes)
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

    let _ = crate::net::ethernet::send_frame(target_mac, crate::net::ethernet::ETHERTYPE_ARP, packet_bytes);
}

pub fn lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    ARP_CACHE.lock().get(&ip).copied()
}

pub fn get_mac_for_ip(ip: [u8; 4]) -> Option<[u8; 6]> {
    // Check cache first
    if let Some(mac) = lookup(ip) {
        return Some(mac);
    }

    // Send ARP request
    let _ = send_arp_request(ip);
    
    // In a real implementation, we'd wait for a reply
    // For now, return None
    None
}
