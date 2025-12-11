//! Network stack implementation

pub mod ethernet;
pub mod arp;
pub mod ip;
pub mod icmp;
pub mod udp;

use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use alloc::collections::VecDeque;

lazy_static! {
    static ref RX_QUEUE: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
}

pub fn init() {
    crate::serial_println!("Network stack initialized");
}

pub fn process_packets() {
    // Receive packets from driver
    while let Some(packet) = crate::drivers::e1000::receive_packet() {
        RX_QUEUE.lock().push_back(packet);
    }

    // Process received packets
    while let Some(packet) = RX_QUEUE.lock().pop_front() {
        ethernet::process_packet(&packet);
    }
}

pub fn send_packet(data: &[u8]) -> Result<(), &'static str> {
    crate::drivers::e1000::send_packet(data)
}
