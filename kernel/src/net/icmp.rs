//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ICMP (Internet Control Message Protocol) - for ping

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU16, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

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
    static ref PENDING_PINGS: Mutex<BTreeMap<(u16, u16), PingRequest>> =
        Mutex::new(BTreeMap::new());
    static ref PING_REPLIES: Mutex<alloc::vec::Vec<PingReply>> = Mutex::new(alloc::vec::Vec::new());
}

static NEXT_PING_ID: AtomicU16 = AtomicU16::new(1);

pub fn next_identifier() -> u16 {
    NEXT_PING_ID.fetch_add(1, Ordering::Relaxed)
}

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

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], _src_mac: [u8; 6]) {
    if packet.len() < 8 || packet[1] != 0 || calculate_checksum(packet) != 0 {
        return;
    }

    if packet[0] == ICMP_ECHO_REQUEST {
        crate::serial_println!(
            "Received ping from {}.{}.{}.{}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3]
        );

        let mut reply = packet.to_vec();
        reply[0] = ICMP_ECHO_REPLY;
        reply[2] = 0;
        reply[3] = 0;
        let checksum = calculate_checksum(&reply);
        reply[2] = (checksum >> 8) as u8;
        reply[3] = (checksum & 0xFF) as u8;

        if let Err(error) = crate::net::ip::send_packet(src_ip, 1, &reply) {
            crate::serial_println!("ICMP reply send failed: {}", error);
        }

        crate::println!(
            "Ping reply sent to {}.{}.{}.{}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3]
        );
    } else if packet[0] == ICMP_ECHO_REPLY {
        let identifier = u16::from_be_bytes([packet[4], packet[5]]);
        let sequence = u16::from_be_bytes([packet[6], packet[7]]);

        crate::serial_println!(
            "ICMP: Received echo reply from {}.{}.{}.{}, id={}, seq={}",
            src_ip[0],
            src_ip[1],
            src_ip[2],
            src_ip[3],
            identifier,
            sequence
        );

        let mut pending = PENDING_PINGS.lock();
        if pending
            .get(&(identifier, sequence))
            .map(|request| request.target_ip == src_ip)
            .unwrap_or(false)
        {
            let request = pending.remove(&(identifier, sequence)).unwrap();
            let current_time = crate::shell::monotonic_ms();
            let rtt_ms = current_time.saturating_sub(request.sent_time);

            crate::serial_println!(
                "Received ping reply from {}.{}.{}.{} (seq={}, rtt={}ms)",
                src_ip[0],
                src_ip[1],
                src_ip[2],
                src_ip[3],
                sequence,
                rtt_ms
            );

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
    let payload = b"MFK Ping!";
    let mut packet = alloc::vec::Vec::with_capacity(8 + payload.len());
    packet.extend_from_slice(&[ICMP_ECHO_REQUEST, 0, 0, 0]);
    packet.extend_from_slice(&identifier.to_be_bytes());
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(payload);

    let checksum = calculate_checksum(&packet);
    packet[2] = (checksum >> 8) as u8;
    packet[3] = (checksum & 0xFF) as u8;

    // Record the pending request
    let current_time = crate::shell::monotonic_ms();
    PENDING_PINGS.lock().insert(
        (identifier, sequence),
        PingRequest {
            target_ip: dst_ip,
            identifier,
            sequence,
            sent_time: current_time,
        },
    );

    crate::serial_println!(
        "ICMP: Sending ping to {}.{}.{}.{}, seq={}",
        dst_ip[0],
        dst_ip[1],
        dst_ip[2],
        dst_ip[3],
        sequence
    );

    match crate::net::ip::send_packet(dst_ip, 1, &packet) {
        Ok(()) => Ok(()),
        Err(e) => {
            PENDING_PINGS.lock().remove(&(identifier, sequence));
            Err(e)
        }
    }
}

pub fn clear_pending(identifier: u16) {
    PENDING_PINGS.lock().retain(|&(id, _), _| id != identifier);
    PING_REPLIES
        .lock()
        .retain(|reply| reply.identifier != identifier);
}

pub fn get_pending_count() -> usize {
    PENDING_PINGS.lock().len()
}

pub fn check_timeouts() -> usize {
    let current_time = crate::shell::monotonic_ms();
    let timeout_ms = 5000; // 5 second timeout

    let mut pending = PENDING_PINGS.lock();
    let mut timed_out = alloc::vec::Vec::new();

    for ((id, seq), req) in pending.iter() {
        if current_time.saturating_sub(req.sent_time) >= timeout_ms {
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

pub fn pop_reply_for(identifier: u16) -> Option<PingReply> {
    let mut replies = PING_REPLIES.lock();
    let index = replies
        .iter()
        .position(|reply| reply.identifier == identifier)?;
    Some(replies.remove(index))
}
