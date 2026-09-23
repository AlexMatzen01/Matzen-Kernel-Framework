//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! UDP (User Datagram Protocol)

use alloc::collections::VecDeque;
use alloc::vec::Vec;
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

pub fn process_packet(packet: &[u8], src_ip: [u8; 4]) {
    if packet.len() < 8 {
        return;
    }

    let udp_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const UdpHeader) };

    let length = udp_header.get_length() as usize;
    if length < 8 || length > packet.len() {
        return;
    }
    let payload = &packet[8..length];

    crate::serial_println!(
        "Received UDP packet from {}.{}.{}.{}:{} -> port {}",
        src_ip[0],
        src_ip[1],
        src_ip[2],
        src_ip[3],
        udp_header.get_src_port(),
        udp_header.get_dst_port()
    );

    let mut queue = RX_QUEUE.lock();
    // Keep the small kernel queue bounded; discard oldest datagrams first.
    if queue.len() >= 32 {
        queue.pop_front();
    }
    queue.push_back(UdpDatagram {
        source_ip: src_ip,
        source_port: udp_header.get_src_port(),
        destination_port: udp_header.get_dst_port(),
        payload: payload.to_vec(),
    });
}

/// Pop the oldest datagram received on `local_port`.
pub fn receive(local_port: u16) -> Option<UdpDatagram> {
    let mut queue = RX_QUEUE.lock();
    let index = queue
        .iter()
        .position(|p| p.destination_port == local_port)?;
    queue.remove(index)
}

pub fn receive_from(local_port: u16, source_ip: [u8; 4], source_port: u16) -> Option<Vec<u8>> {
    let mut queue = RX_QUEUE.lock();
    let index = queue.iter().position(|packet| {
        packet.destination_port == local_port
            && packet.source_ip == source_ip
            && packet.source_port == source_port
    })?;
    queue.remove(index).map(|packet| packet.payload)
}

pub fn queued_count() -> usize {
    RX_QUEUE.lock().len()
}

pub fn send_packet(
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    data: &[u8],
) -> Result<(), &'static str> {
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

    crate::net::ip::send_packet(dst_ip, 17, &packet)
}
