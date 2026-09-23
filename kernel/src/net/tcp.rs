//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! TCP (Transmission Control Protocol) implementation

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

// TCP Flags
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

// TCP States
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset_flags: u16, // 4 bits offset, 6 bits reserved, 6 bits flags
    pub window_size: u16,
    pub checksum: u16,
    pub urgent_ptr: u16,
}

#[derive(Clone)]
pub struct TcpConnection {
    pub local_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub state: TcpState,
    pub seq_num: u32,
    pub ack_num: u32,
    pub recv_buffer: Vec<u8>,
    pub send_buffer: Vec<u8>,
}

struct TcpReply {
    remote_ip: [u8; 4],
    local_port: u16,
    remote_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
}

lazy_static! {
    static ref TCP_CONNECTIONS: Mutex<BTreeMap<u16, TcpConnection>> = Mutex::new(BTreeMap::new());
    static ref NEXT_PORT: Mutex<u16> = Mutex::new(49152); // Start of ephemeral port range
}

impl TcpHeader {
    pub fn new(src_port: u16, dst_port: u16, seq: u32, ack: u32, flags: u8) -> Self {
        TcpHeader {
            src_port: src_port.to_be(),
            dst_port: dst_port.to_be(),
            seq_num: seq.to_be(),
            ack_num: ack.to_be(),
            data_offset_flags: ((5 << 12) | (flags as u16)).to_be(), // 5 * 4 = 20 byte header
            window_size: 8192u16.to_be(),
            checksum: 0,
            urgent_ptr: 0,
        }
    }

    pub fn get_flags(&self) -> u8 {
        (u16::from_be(self.data_offset_flags) & 0x3F) as u8
    }

    pub fn get_data_offset(&self) -> u8 {
        ((u16::from_be(self.data_offset_flags) >> 12) & 0xF) as u8
    }

    fn calculate_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], tcp_segment: &[u8]) -> u16 {
        let mut sum: u32 = 0;

        sum += ((src_ip[0] as u32) << 8) | src_ip[1] as u32;
        sum += ((src_ip[2] as u32) << 8) | src_ip[3] as u32;
        sum += ((dst_ip[0] as u32) << 8) | dst_ip[1] as u32;
        sum += ((dst_ip[2] as u32) << 8) | dst_ip[3] as u32;
        sum += 6; // Protocol (TCP)
        sum += tcp_segment.len() as u32;

        // TCP segment
        for i in (0..tcp_segment.len()).step_by(2) {
            if i + 1 < tcp_segment.len() {
                let word = ((tcp_segment[i] as u32) << 8) | (tcp_segment[i + 1] as u32);
                sum += word;
            } else {
                sum += (tcp_segment[i] as u32) << 8;
            }
        }

        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }

        !sum as u16
    }
}

pub fn allocate_port() -> u16 {
    let mut port = NEXT_PORT.lock();
    let allocated = *port;
    *port = if *port == 65535 { 49152 } else { *port + 1 };
    allocated
}

pub fn process_packet(packet: &[u8], src_ip: [u8; 4], _src_mac: [u8; 6]) {
    if packet.len() < 20 {
        return;
    }
    let Some(dst_ip) = crate::net::ip::get_ip_address() else {
        return;
    };
    if TcpHeader::calculate_checksum(dst_ip, src_ip, packet) != 0 {
        return;
    }

    let tcp_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const TcpHeader) };

    let src_port = u16::from_be(tcp_header.src_port);
    let dst_port = u16::from_be(tcp_header.dst_port);
    let seq = u32::from_be(tcp_header.seq_num);
    let ack = u32::from_be(tcp_header.ack_num);
    let flags = tcp_header.get_flags();
    let data_offset = (tcp_header.get_data_offset() * 4) as usize;
    if data_offset < 20 || data_offset > packet.len() {
        return;
    }

    crate::serial_println!(
        "TCP: Received packet from {}.{}.{}.{}:{} to port {}, flags={:#x}, seq={}, ack={}",
        src_ip[0],
        src_ip[1],
        src_ip[2],
        src_ip[3],
        src_port,
        dst_port,
        flags,
        seq,
        ack
    );

    let mut replies = Vec::new();
    {
        let mut connections = TCP_CONNECTIONS.lock();
        if let Some(conn) = connections.get_mut(&dst_port) {
            if conn.remote_ip == src_ip && conn.remote_port == src_port {
                handle_connection_packet(
                    conn,
                    flags,
                    seq,
                    ack,
                    &packet[data_offset..],
                    &mut replies,
                );
            }
        } else if flags & TCP_SYN != 0 {
            crate::serial_println!("TCP: Received SYN on port {} but not listening", dst_port);
        }
    }
    for reply in replies {
        if let Err(error) = send_tcp_packet(
            reply.remote_ip,
            reply.local_port,
            reply.remote_port,
            reply.seq,
            reply.ack,
            reply.flags,
            &[],
        ) {
            crate::serial_println!("TCP reply send failed: {}", error);
        }
    }
}

fn handle_connection_packet(
    conn: &mut TcpConnection,
    flags: u8,
    seq: u32,
    ack: u32,
    data: &[u8],
    replies: &mut Vec<TcpReply>,
) {
    crate::serial_println!(
        "TCP: Handling packet in state {:?}, flags={:#x}",
        conn.state,
        flags
    );

    match conn.state {
        TcpState::SynSent => {
            if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 && ack == conn.seq_num.wrapping_add(1) {
                crate::serial_println!("TCP: Received SYN-ACK");
                conn.seq_num = conn.seq_num.wrapping_add(1);
                conn.ack_num = seq.wrapping_add(1);
                conn.state = TcpState::Established;
                replies.push(TcpReply {
                    remote_ip: conn.remote_ip,
                    local_port: conn.local_port,
                    remote_port: conn.remote_port,
                    seq: conn.seq_num,
                    ack: conn.ack_num,
                    flags: TCP_ACK,
                });
                crate::println!(
                    "TCP connection established to {}.{}.{}.{}:{}",
                    conn.remote_ip[0],
                    conn.remote_ip[1],
                    conn.remote_ip[2],
                    conn.remote_ip[3],
                    conn.remote_port
                );
            }
        }
        TcpState::Established => {
            if !data.is_empty() {
                conn.recv_buffer.extend_from_slice(data);
                conn.ack_num = seq.wrapping_add(data.len() as u32);
                replies.push(TcpReply {
                    remote_ip: conn.remote_ip,
                    local_port: conn.local_port,
                    remote_port: conn.remote_port,
                    seq: conn.seq_num,
                    ack: conn.ack_num,
                    flags: TCP_ACK,
                });
            }
            if flags & TCP_FIN != 0 {
                crate::serial_println!("TCP: Received FIN");
                conn.ack_num = seq.wrapping_add(data.len() as u32).wrapping_add(1);
                conn.state = TcpState::CloseWait;
                replies.push(TcpReply {
                    remote_ip: conn.remote_ip,
                    local_port: conn.local_port,
                    remote_port: conn.remote_port,
                    seq: conn.seq_num,
                    ack: conn.ack_num,
                    flags: TCP_ACK,
                });
            }
        }
        _ => {}
    }
}

fn send_tcp_packet(
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data: &[u8],
) -> Result<(), &'static str> {
    let our_ip = crate::net::ip::get_ip_address().ok_or("No IP address configured")?;

    let tcp_header = TcpHeader::new(src_port, dst_port, seq, ack, flags);

    let mut segment = Vec::with_capacity(20 + data.len());
    unsafe {
        let header_bytes = core::slice::from_raw_parts(
            &tcp_header as *const TcpHeader as *const u8,
            core::mem::size_of::<TcpHeader>(),
        );
        segment.extend_from_slice(header_bytes);
    }
    segment.extend_from_slice(data);

    // Calculate checksum
    let checksum = TcpHeader::calculate_checksum(our_ip, dst_ip, &segment);
    segment[16] = (checksum >> 8) as u8;
    segment[17] = (checksum & 0xFF) as u8;

    crate::serial_println!(
        "TCP: Sending packet to {}.{}.{}.{}:{}, flags={:#x}, seq={}, ack={}, data_len={}",
        dst_ip[0],
        dst_ip[1],
        dst_ip[2],
        dst_ip[3],
        dst_port,
        flags,
        seq,
        ack,
        data.len()
    );

    crate::net::ip::send_packet(dst_ip, 6, &segment)
}

pub fn connect(remote_ip: [u8; 4], remote_port: u16) -> Result<u16, &'static str> {
    let local_port = allocate_port();
    let initial_seq = 1000; // Could use random number

    let conn = TcpConnection {
        local_port,
        remote_ip,
        remote_port,
        state: TcpState::SynSent,
        seq_num: initial_seq,
        ack_num: 0,
        recv_buffer: Vec::new(),
        send_buffer: Vec::new(),
    };

    TCP_CONNECTIONS.lock().insert(local_port, conn);

    // Send SYN
    if let Err(error) = send_tcp_packet(
        remote_ip,
        local_port,
        remote_port,
        initial_seq,
        0,
        TCP_SYN,
        &[],
    ) {
        TCP_CONNECTIONS.lock().remove(&local_port);
        return Err(error);
    }

    crate::serial_println!(
        "TCP: Sent SYN to {}.{}.{}.{}:{} from port {}",
        remote_ip[0],
        remote_ip[1],
        remote_ip[2],
        remote_ip[3],
        remote_port,
        local_port
    );

    Ok(local_port)
}

pub fn send_data(local_port: u16, data: &[u8]) -> Result<(), &'static str> {
    let (remote_ip, remote_port, seq_num, ack_num) = {
        let connections = TCP_CONNECTIONS.lock();
        let conn = connections.get(&local_port).ok_or("Connection not found")?;
        if conn.state != TcpState::Established {
            return Err("Connection not established");
        }
        (conn.remote_ip, conn.remote_port, conn.seq_num, conn.ack_num)
    };

    send_tcp_packet(
        remote_ip,
        local_port,
        remote_port,
        seq_num,
        ack_num,
        TCP_ACK | TCP_PSH,
        data,
    )?;

    let mut connections = TCP_CONNECTIONS.lock();
    if let Some(conn) = connections.get_mut(&local_port) {
        conn.seq_num = conn.seq_num.wrapping_add(data.len() as u32);
    }
    Ok(())
}

pub fn close(local_port: u16) -> Result<(), &'static str> {
    let (remote_ip, remote_port, seq_num, ack_num) = {
        let connections = TCP_CONNECTIONS.lock();
        let conn = connections.get(&local_port).ok_or("Connection not found")?;
        if conn.state != TcpState::Established {
            return Ok(());
        }
        (conn.remote_ip, conn.remote_port, conn.seq_num, conn.ack_num)
    };

    send_tcp_packet(
        remote_ip,
        local_port,
        remote_port,
        seq_num,
        ack_num,
        TCP_FIN | TCP_ACK,
        &[],
    )?;

    let mut connections = TCP_CONNECTIONS.lock();
    if let Some(conn) = connections.get_mut(&local_port) {
        conn.state = TcpState::FinWait1;
        conn.seq_num = conn.seq_num.wrapping_add(1);
    }
    Ok(())
}

pub fn get_state(local_port: u16) -> Option<TcpState> {
    TCP_CONNECTIONS.lock().get(&local_port).map(|c| c.state)
}

pub fn read_data(local_port: u16) -> Option<Vec<u8>> {
    let mut connections = TCP_CONNECTIONS.lock();
    if let Some(conn) = connections.get_mut(&local_port) {
        if !conn.recv_buffer.is_empty() {
            let data = conn.recv_buffer.clone();
            conn.recv_buffer.clear();
            return Some(data);
        }
    }
    None
}

/// Forget a closed or timed-out socket and release its protocol state.
pub fn forget(local_port: u16) {
    TCP_CONNECTIONS.lock().remove(&local_port);
}

#[cfg(test)]
mod tests {
    use super::{TcpHeader, TCP_SYN};
    use alloc::vec::Vec;

    fn segment(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
        let header = TcpHeader::new(src_port, dst_port, 1000, 0, TCP_SYN);
        let mut segment = unsafe {
            core::slice::from_raw_parts(
                &header as *const TcpHeader as *const u8,
                core::mem::size_of::<TcpHeader>(),
            )
        }
        .to_vec();
        segment.extend_from_slice(payload);
        segment
    }

    #[test]
    fn syn_checksum_uses_ipv4_pseudo_header_words() {
        let packet = segment(49152, 80, &[]);
        assert_eq!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [91, 199, 118, 184], &packet),
            0xed1b
        );
    }

    #[test]
    fn odd_length_payload_checksum_validates() {
        let mut packet = segment(49152, 80, &[0x42]);
        let checksum = TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet);
        packet[16] = (checksum >> 8) as u8;
        packet[17] = (checksum & 0xFF) as u8;
        assert_eq!(
            TcpHeader::calculate_checksum([10, 0, 2, 15], [1, 2, 3, 4], &packet),
            0
        );
    }
}
