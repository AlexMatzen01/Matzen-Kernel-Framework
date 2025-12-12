//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ICMP (Internet Control Message Protocol) - for ping

use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IcmpHeader {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub identifier: u16,
    pub sequence: u16,
}

#[derive(Clone)]
pub struct PingRequest {
    pub target_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub sent_time: u64,
}

#[derive(Clone)]
pub struct PingReply {
    pub source_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub rtt_ms: u64,
}

lazy_static! {
    static ref PENDING_PINGS: Mutex<BTreeMap<(u16, u16), PingRequest>> = Mutex::new(BTreeMap::new());
    static ref PING_REPLIES: Mutex<alloc::vec::Vec<PingReply>> = Mutex::new(alloc::vec::Vec::new());
}

impl IcmpHeader {
    fn calculate_checksum(data: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        
        for i in (0..data.len()).step_by(2) {
            if i + 1 < data.len() {
                let word = ((data[i] as u32) << 8) | (data[i + 1] as u32);
                sum += word;
            } else {
                sum += (data[i] as u32) << 8;
            }
        }

        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }

        !sum as u16
    }
}

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], _src_mac: [u8; 6]) {
    if packet.len() < 8 {
        return;
    }

    let icmp_header = unsafe {
        core::ptr::read_unaligned(packet.as_ptr() as *const IcmpHeader)
    };

    if icmp_header.icmp_type == ICMP_ECHO_REQUEST {
        crate::serial_println!("Received ping from {}.{}.{}.{}", 
            src_ip[0], src_ip[1], src_ip[2], src_ip[3]);

        // Send echo reply
        let mut reply_header = icmp_header;
        reply_header.icmp_type = ICMP_ECHO_REPLY;
        reply_header.checksum = 0;

        let mut reply = alloc::vec::Vec::with_capacity(packet.len());
        unsafe {
            let header_bytes = core::slice::from_raw_parts(
                &reply_header as *const IcmpHeader as *const u8,
                core::mem::size_of::<IcmpHeader>(),
            );
            reply.extend_from_slice(header_bytes);
        }
        
        // Copy payload
        if packet.len() > 8 {
            reply.extend_from_slice(&packet[8..]);
        }

        // Calculate checksum
        let checksum = IcmpHeader::calculate_checksum(&reply);
        reply[2] = (checksum >> 8) as u8;
        reply[3] = (checksum & 0xFF) as u8;

        let _ = crate::net::ip::send_packet(src_ip, 1, &reply);
        
        crate::println!("Ping reply sent to {}.{}.{}.{}", 
            src_ip[0], src_ip[1], src_ip[2], src_ip[3]);
    } else if icmp_header.icmp_type == ICMP_ECHO_REPLY {
        // Handle echo reply
        let identifier = u16::from_be(icmp_header.identifier);
        let sequence = u16::from_be(icmp_header.sequence);
        
        let mut pending = PENDING_PINGS.lock();
        if let Some(request) = pending.remove(&(identifier, sequence)) {
            let current_time = crate::shell::get_tick_count();
            let rtt_ms = current_time.saturating_sub(request.sent_time);
            
            crate::serial_println!("Received ping reply from {}.{}.{}.{} (seq={}, rtt={}ms)",
                src_ip[0], src_ip[1], src_ip[2], src_ip[3], sequence, rtt_ms);
            
            PING_REPLIES.lock().push(PingReply {
                source_ip: src_ip,
                identifier,
                sequence,
                rtt_ms,
            });
        }
    }
}

pub fn send_ping(dst_ip: [u8; 4], identifier: u16, sequence: u16) -> Result<(), &'static str> {
    let icmp = IcmpHeader {
        icmp_type: ICMP_ECHO_REQUEST,
        code: 0,
        checksum: 0,
        identifier: identifier.to_be(),
        sequence: sequence.to_be(),
    };

    let payload = b"MFK Ping!";
    let mut packet = alloc::vec::Vec::with_capacity(8 + payload.len());
    
    unsafe {
        let header_bytes = core::slice::from_raw_parts(
            &icmp as *const IcmpHeader as *const u8,
            core::mem::size_of::<IcmpHeader>(),
        );
        packet.extend_from_slice(header_bytes);
    }
    packet.extend_from_slice(payload);

    // Calculate checksum
    let checksum = IcmpHeader::calculate_checksum(&packet);
    packet[2] = (checksum >> 8) as u8;
    packet[3] = (checksum & 0xFF) as u8;

    // Record the pending request
    let current_time = crate::shell::get_tick_count();
    PENDING_PINGS.lock().insert(
        (identifier, sequence),
        PingRequest {
            target_ip: dst_ip,
            identifier,
            sequence,
            sent_time: current_time,
        },
    );

    crate::serial_println!("ICMP: Sending ping to {}.{}.{}.{}, seq={}", 
        dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3], sequence);

    crate::net::ip::send_packet(dst_ip, 1, &packet)
}

pub fn get_pending_count() -> usize {
    PENDING_PINGS.lock().len()
}

pub fn check_timeouts() -> usize {
    let current_time = crate::shell::get_tick_count();
    let timeout_ms = 5000; // 5 second timeout
    
    let mut pending = PENDING_PINGS.lock();
    let mut timed_out = alloc::vec::Vec::new();
    
    for ((id, seq), req) in pending.iter() {
        if current_time.saturating_sub(req.sent_time) > timeout_ms {
            timed_out.push((*id, *seq));
        }
    }
    
    for key in &timed_out {
        pending.remove(key);
    }
    
    timed_out.len()
}

pub fn pop_reply() -> Option<PingReply> {
    let mut replies = PING_REPLIES.lock();
    if replies.is_empty() {
        None
    } else {
        Some(replies.remove(0))
    }
}
