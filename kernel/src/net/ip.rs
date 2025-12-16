//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! IPv4 protocol implementation

use spin::Mutex;
use lazy_static::lazy_static;

const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_UDP: u8 = 17;
const IP_PROTO_TCP: u8 = 6;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IpHeader {
    pub version_ihl: u8,     // Version (4 bits) + IHL (4 bits)
    pub dscp_ecn: u8,        // DSCP (6 bits) + ECN (2 bits)
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
    static ref IP_ADDRESS: Mutex<Option<[u8; 4]>> = Mutex::new(None);
}

pub fn set_ip_address(ip: [u8; 4]) {
    *IP_ADDRESS.lock() = Some(ip);
    crate::serial_println!("IP address set to {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
}

pub fn get_ip_address() -> Option<[u8; 4]> {
    *IP_ADDRESS.lock()
}

impl IpHeader {
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

        let mut sum: u32 = 0;
        for i in (0..20).step_by(2) {
            if i == 10 { continue; } // Skip checksum field
            let word = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
            sum += word;
        }

        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }

        !sum as u16
    }

    pub fn get_total_length(&self) -> u16 {
        u16::from_be(self.total_length)
    }

    pub fn get_ihl(&self) -> u8 {
        (self.version_ihl & 0x0F) * 4
    }
}

pub fn process_packet(packet: &[u8], src_mac: [u8; 6]) {
    if packet.len() < 20 {
        return;
    }

    let ip_header = unsafe {
        core::ptr::read_unaligned(packet.as_ptr() as *const IpHeader)
    };

    crate::serial_println!("IP: Received packet from {}.{}.{}.{} to {}.{}.{}.{}, protocol={}",
        ip_header.src_ip[0], ip_header.src_ip[1], ip_header.src_ip[2], ip_header.src_ip[3],
        ip_header.dst_ip[0], ip_header.dst_ip[1], ip_header.dst_ip[2], ip_header.dst_ip[3],
        ip_header.protocol);

    // Check if packet is for us
    if let Some(our_ip) = get_ip_address() {
        if ip_header.dst_ip != our_ip {
            crate::serial_println!("IP: Packet not for us (our IP: {}.{}.{}.{}), discarding",
                our_ip[0], our_ip[1], our_ip[2], our_ip[3]);
            return;
        }
    } else {
        crate::serial_println!("IP: No IP address configured, accepting all packets");
    }

    let header_len = ip_header.get_ihl() as usize;
    if packet.len() < header_len {
        return;
    }

    let payload = &packet[header_len..];

    match ip_header.protocol {
        IP_PROTO_ICMP => {
            crate::net::icmp::process_packet(payload, ip_header.src_ip, src_mac);
        }
        IP_PROTO_UDP => {
            crate::net::udp::process_packet(payload, ip_header.src_ip);
        }
        IP_PROTO_TCP => {
            crate::net::tcp::process_packet(payload, ip_header.src_ip, src_mac);
        }
        _ => {
            crate::serial_println!("IP: Unsupported protocol {}", ip_header.protocol);
        }
    }
}

pub fn send_packet(dst_ip: [u8; 4], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    let src_ip = get_ip_address().ok_or("No IP address configured")?;
    
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

    // Get MAC address for destination IP (simplified - assumes same subnet)
    let dst_mac = if let Some(mac) = crate::net::arp::lookup(dst_ip) {
        mac
    } else {
        // Send to gateway or broadcast
        [0xFF; 6]
    };

    crate::net::ethernet::send_frame(dst_mac, crate::net::ethernet::ETHERTYPE_IP, &packet)
}
