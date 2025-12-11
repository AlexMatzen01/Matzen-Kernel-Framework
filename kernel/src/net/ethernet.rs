//! Ethernet frame handling

use alloc::vec::Vec;

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IP: u16 = 0x0800;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EthernetFrame {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub ethertype: u16,
}

impl EthernetFrame {
    pub fn new(dst_mac: [u8; 6], src_mac: [u8; 6], ethertype: u16) -> Self {
        EthernetFrame {
            dst_mac,
            src_mac,
            ethertype: ethertype.to_be(),
        }
    }

    pub fn get_ethertype(&self) -> u16 {
        u16::from_be(self.ethertype)
    }
}

pub fn process_packet(packet: &[u8]) {
    if packet.len() < 14 {
        return;
    }

    let frame = unsafe {
        core::ptr::read_unaligned(packet.as_ptr() as *const EthernetFrame)
    };

    let payload = &packet[14..];
    
    match frame.get_ethertype() {
        ETHERTYPE_ARP => {
            crate::net::arp::process_packet(payload, frame.src_mac);
        }
        ETHERTYPE_IP => {
            crate::net::ip::process_packet(payload, frame.src_mac);
        }
        _ => {
            // Unknown ethertype
        }
    }
}

pub fn send_frame(dst_mac: [u8; 6], ethertype: u16, payload: &[u8]) -> Result<(), &'static str> {
    let src_mac = crate::drivers::e1000::mac_address().ok_or("No MAC address")?;
    
    let mut packet = Vec::with_capacity(14 + payload.len());
    let frame = EthernetFrame::new(dst_mac, src_mac, ethertype);
    
    // Copy frame header
    unsafe {
        let frame_bytes = core::slice::from_raw_parts(
            &frame as *const EthernetFrame as *const u8,
            core::mem::size_of::<EthernetFrame>(),
        );
        packet.extend_from_slice(frame_bytes);
    }
    
    packet.extend_from_slice(payload);
    
    crate::net::send_packet(&packet)
}
