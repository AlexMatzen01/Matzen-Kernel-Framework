//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! IPv4 protocol implementation

use lazy_static::lazy_static;
use spin::Mutex;

const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_UDP: u8 = 17;
const IP_PROTO_TCP: u8 = 6;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IpHeader {
    pub version_ihl: u8, // Version (4 bits) + IHL (4 bits)
    pub dscp_ecn: u8,    // DSCP (6 bits) + ECN (2 bits)
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
    static ref NETWORK_CONFIG: Mutex<NetworkConfig> = Mutex::new(NetworkConfig::default());
}

/// IPv4 interface settings. A zero mask disables subnet routing; without a
/// gateway, only directly resolved hosts are reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkConfig {
    pub address: Option<[u8; 4]>,
    pub netmask: [u8; 4],
    pub gateway: Option<[u8; 4]>,
}

impl NetworkConfig {
    pub const fn default() -> Self {
        Self {
            address: None,
            netmask: [255, 255, 255, 0],
            gateway: None,
        }
    }
}

/// Choose the on-link destination or gateway for a destination address.
pub fn next_hop(config: NetworkConfig, destination: [u8; 4]) -> Option<[u8; 4]> {
    let address = config.address?;
    if config.netmask == [0, 0, 0, 0] {
        return config.gateway;
    }
    let on_link =
        (0..4).all(|i| (address[i] & config.netmask[i]) == (destination[i] & config.netmask[i]));
    if on_link {
        Some(destination)
    } else {
        config.gateway
    }
}

pub fn network_config() -> NetworkConfig {
    *NETWORK_CONFIG.lock()
}

pub fn configure(address: [u8; 4], netmask: [u8; 4], gateway: Option<[u8; 4]>) {
    *NETWORK_CONFIG.lock() = NetworkConfig {
        address: Some(address),
        netmask,
        gateway,
    };
    crate::serial_println!(
        "IPv4 configured: {}.{}.{}.{} mask {}.{}.{}.{} gateway {}",
        address[0],
        address[1],
        address[2],
        address[3],
        netmask[0],
        netmask[1],
        netmask[2],
        netmask[3],
        match gateway {
            Some(g) => alloc::format!("{}.{}.{}.{}", g[0], g[1], g[2], g[3]),
            None => alloc::string::String::from("(none)"),
        }
    );
}

pub fn set_ip_address(ip: [u8; 4]) {
    let mut cfg = NETWORK_CONFIG.lock();
    cfg.address = Some(ip);
    crate::serial_println!("IP address set to {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
}

pub fn get_ip_address() -> Option<[u8; 4]> {
    NETWORK_CONFIG.lock().address
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
            if i == 10 {
                continue;
            } // Skip checksum field
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

    let ip_header = unsafe { core::ptr::read_unaligned(packet.as_ptr() as *const IpHeader) };

    if ip_header.version_ihl >> 4 != 4 {
        return;
    }

    crate::serial_println!(
        "IP: Received packet from {}.{}.{}.{} to {}.{}.{}.{}, protocol={}",
        ip_header.src_ip[0],
        ip_header.src_ip[1],
        ip_header.src_ip[2],
        ip_header.src_ip[3],
        ip_header.dst_ip[0],
        ip_header.dst_ip[1],
        ip_header.dst_ip[2],
        ip_header.dst_ip[3],
        ip_header.protocol
    );

    // Check if packet is for us
    if let Some(our_ip) = get_ip_address() {
        if ip_header.dst_ip != our_ip {
            crate::serial_println!(
                "IP: Packet not for us (our IP: {}.{}.{}.{}), discarding",
                our_ip[0],
                our_ip[1],
                our_ip[2],
                our_ip[3]
            );
            return;
        }
    } else {
        crate::serial_println!("IP: No IP address configured, accepting all packets");
    }

    let header_len = ip_header.get_ihl() as usize;
    let total_len = ip_header.get_total_length() as usize;
    if header_len < core::mem::size_of::<IpHeader>()
        || total_len < header_len
        || packet.len() < total_len
    {
        return;
    }

    // Ethernet pads short frames to its minimum frame size. Do not pass that
    // padding to ICMP/UDP/TCP as part of the IPv4 payload.
    let payload = &packet[header_len..total_len];

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
    if payload.len() > u16::MAX as usize - core::mem::size_of::<IpHeader>() {
        return Err("IPv4 packet too large");
    }

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

    let cfg = network_config();
    let next_hop =
        next_hop(cfg, dst_ip).ok_or("No route to host (configure a gateway with ifconfig)")?;
    let dst_mac = crate::net::arp::resolve(next_hop, 2000)?;

    crate::net::ethernet::send_frame(dst_mac, crate::net::ethernet::ETHERTYPE_IP, &packet)
}

#[cfg(test)]
mod tests {
    use super::{next_hop, NetworkConfig};

    #[test]
    fn route_uses_destination_for_on_link_hosts() {
        let cfg = NetworkConfig {
            address: Some([10, 0, 2, 15]),
            netmask: [255, 255, 255, 0],
            gateway: Some([10, 0, 2, 2]),
        };
        assert_eq!(next_hop(cfg, [10, 0, 2, 2]), Some([10, 0, 2, 2]));
    }

    #[test]
    fn route_uses_gateway_for_off_link_hosts() {
        let cfg = NetworkConfig {
            address: Some([10, 0, 2, 15]),
            netmask: [255, 255, 255, 0],
            gateway: Some([10, 0, 2, 2]),
        };
        assert_eq!(next_hop(cfg, [1, 1, 1, 1]), Some([10, 0, 2, 2]));
    }

    #[test]
    fn no_gateway_means_no_off_link_route() {
        let cfg = NetworkConfig {
            address: Some([10, 0, 2, 15]),
            netmask: [255, 255, 255, 0],
            gateway: None,
        };
        assert_eq!(next_hop(cfg, [1, 1, 1, 1]), None);
    }

    #[test]
    fn default_route_uses_gateway() {
        let cfg = NetworkConfig {
            address: Some([192, 168, 1, 10]),
            netmask: [0, 0, 0, 0],
            gateway: Some([192, 168, 1, 1]),
        };
        assert_eq!(next_hop(cfg, [8, 8, 8, 8]), Some([192, 168, 1, 1]));
    }
}
