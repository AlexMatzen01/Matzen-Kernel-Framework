//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Network stack implementation

pub mod arp;
pub mod debug;
pub mod dhcp;
pub mod dns;
#[cfg(feature = "net_tls")]
pub mod entropy;
pub mod ethernet;
pub(crate) mod http;
pub mod icmp;
pub mod ip;
pub mod speedtest;
pub mod socket;
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
    // When the NIC has an interrupt line, drain only what an interrupt
    // signalled. Without one the driver has no way to announce work, so the ring
    // is polled every pass, as before.
    let _ = crate::drivers::e1000::take_rx_pending();

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

    // Retransmission and reclaimation, driven from the same pump as delivery
    // so a lost segment is noticed while the connection is still live.
    tcp::tick();
    tcp::expire();
}

pub fn send_packet(data: &[u8]) -> Result<(), &'static str> {
    crate::drivers::e1000::send_packet(data)
}

/// Pump received packets and advance the cooperative clock.
///
/// Both halves are required. `process_packets` moves received frames into the
/// protocol handlers, and `increment_tick` keeps the millisecond clock advancing
/// even on a machine whose IRQ0 never fires — which is every QEMU configuration
/// this kernel is tested under. The spin loop keeps the cooperative scheduler
/// from monopolising the core between poll intervals.
///
/// This lives here rather than in `http` so that the stack's scheduler has no
/// dependency on a protocol module: TCP needs to call it while its send buffer
/// is full, and when it lived in `http` the dependency ran `tcp -> http -> tcp`.
pub fn pump() {
    process_packets();
    // Keep deadlines progressing even on machines without a working IRQ0.
    crate::shell::increment_tick();
    for _ in 0..2000 {
        core::hint::spin_loop();
    }
}
