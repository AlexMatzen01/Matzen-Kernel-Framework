//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Network stack implementation

pub mod arp;
pub mod debug;
pub mod dns;
#[cfg(feature = "net_tls")]
pub mod entropy;
pub mod ethernet;
pub(crate) mod http;
pub mod icmp;
pub mod ip;
pub mod speedtest;
pub mod tcp;
#[cfg(feature = "net_tls")]
pub mod tls;
#[cfg(not(feature = "net_tls"))]
#[path = "tls_stub.rs"]
pub mod tls;
pub mod udp;
pub mod wget;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const MAX_RX_QUEUE: usize = 64;
const MAX_RX_PER_PUMP: usize = 64;

lazy_static! {
    static ref RX_QUEUE: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
}

pub fn init() {
    crate::serial_println!("Network stack initialized");
}

pub fn process_packets() {
    for _ in 0..MAX_RX_PER_PUMP {
        let Some(packet) = crate::drivers::e1000::receive_packet() else {
            break;
        };
        crate::net_log!("RX: Received packet {} bytes", packet.len());
        let mut queue = RX_QUEUE.lock();
        if queue.len() >= MAX_RX_QUEUE {
            queue.pop_front();
        }
        queue.push_back(packet);
    }

    for _ in 0..MAX_RX_PER_PUMP {
        let packet = { RX_QUEUE.lock().pop_front() };
        let Some(packet) = packet else { break };
        crate::net_log!("Processing packet {} bytes", packet.len());
        ethernet::process_packet(&packet);
    }
}

pub fn send_packet(data: &[u8]) -> Result<(), &'static str> {
    crate::drivers::e1000::send_packet(data)
}
