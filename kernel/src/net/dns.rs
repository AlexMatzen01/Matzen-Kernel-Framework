//! Minimal IPv4 DNS resolver used by the shell ping command.

use alloc::vec::Vec;

const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];
const DNS_PORT: u16 = 53;
const LOCAL_PORT: u16 = 49153;

pub fn resolve_ipv4(name: &str) -> Result<[u8; 4], &'static str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 253 {
        return Err("Invalid hostname");
    }

    let id = (crate::shell::get_tick_count() as u16).wrapping_add(1);
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("Invalid hostname");
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);

    crate::net::udp::send_packet(DNS_SERVER, LOCAL_PORT, DNS_PORT, &query)?;
    let start = crate::shell::get_tick_count();
    while crate::shell::get_tick_count().saturating_sub(start) < 3000 {
        crate::net::process_packets();
        if let Some(response) = crate::net::udp::receive(DNS_SERVER, DNS_PORT, LOCAL_PORT) {
            return parse_response(&response, id);
        }
        crate::shell::increment_tick();
        core::hint::spin_loop();
    }
    Err("DNS query timed out")
}

fn parse_response(data: &[u8], id: u16) -> Result<[u8; 4], &'static str> {
    if data.len() < 12 || u16::from_be_bytes([data[0], data[1]]) != id {
        return Err("Invalid DNS response");
    }
    if u16::from_be_bytes([data[2], data[3]]) & 0x8000 == 0 {
        return Err("Invalid DNS response");
    }
    let questions = u16::from_be_bytes([data[4], data[5]]) as usize;
    let answers = u16::from_be_bytes([data[6], data[7]]) as usize;
    let mut offset = 12;
    for _ in 0..questions {
        offset = skip_name(data, offset)?;
        offset = offset.checked_add(4).ok_or("Invalid DNS response")?;
        if offset > data.len() {
            return Err("Invalid DNS response");
        }
    }
    for _ in 0..answers {
        offset = skip_name(data, offset)?;
        if offset.checked_add(10).ok_or("Invalid DNS response")? > data.len() {
            return Err("Invalid DNS response");
        }
        let record_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let class = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
        let length = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
        offset += 10;
        if offset.checked_add(length).ok_or("Invalid DNS response")? > data.len() {
            return Err("Invalid DNS response");
        }
        if record_type == 1 && class == 1 && length == 4 {
            return Ok([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
        }
        offset += length;
    }
    Err("No IPv4 address in DNS response")
}

fn skip_name(data: &[u8], mut offset: usize) -> Result<usize, &'static str> {
    for _ in 0..128 {
        if offset >= data.len() {
            return Err("Invalid DNS response");
        }
        let length = data[offset];
        if length & 0xc0 == 0xc0 {
            if offset + 2 > data.len() {
                return Err("Invalid DNS response");
            }
            return Ok(offset + 2);
        }
        offset += 1;
        if length == 0 {
            return Ok(offset);
        }
        if length > 63 || offset + length as usize > data.len() {
            return Err("Invalid DNS response");
        }
        offset += length as usize;
    }
    Err("Invalid DNS response")
}
