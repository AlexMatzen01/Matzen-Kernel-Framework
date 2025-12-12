//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! UDP (User Datagram Protocol)

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
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

pub fn process_packet(packet: &[u8], src_ip: [u8; 4]) {
    if packet.len() < 8 {
        return;
    }

    let udp_header = unsafe {
        core::ptr::read_unaligned(packet.as_ptr() as *const UdpHeader)
    };

    let payload = &packet[8..];
    
    crate::serial_println!("Received UDP packet from {}.{}.{}.{}:{} -> port {}",
        src_ip[0], src_ip[1], src_ip[2], src_ip[3],
        udp_header.get_src_port(), udp_header.get_dst_port());
    
    if payload.len() > 0 {
        if let Ok(s) = core::str::from_utf8(payload) {
            crate::println!("UDP data: {}", s);
        }
    }
}

pub fn send_packet(dst_ip: [u8; 4], src_port: u16, dst_port: u16, data: &[u8]) -> Result<(), &'static str> {
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

    crate::net::ip::send_packet(dst_ip, 17, &packet)
}
