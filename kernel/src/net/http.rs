//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Shared minimal HTTP helpers for single-stream clients
//! (`speedtest`, `wget`).
//!
//! Uses only the existing stack: DNS (`net::dns`), TCP (`net::tcp`),
//! packet pump (`net::process_packets`) and the PIT clock
//! (`shell::get_tick_count`). HTTP/1.0 + `Connection: close` only;
//! no chunked encoding or second stack.

use alloc::vec::Vec;

/// Stay below the E1000 2048 B TX buffer (14 eth + 20 ip + 20 tcp).
pub(crate) const TCP_CHUNK: usize = 1400;
/// Upper bound for response headers (body streams separately).
pub(crate) const MAX_HEADER: usize = 8192;
pub(crate) const CONNECT_TIMEOUT_MS: u64 = 5000;

pub(crate) fn now_ms() -> u64 {
    crate::shell::get_tick_count()
}

pub(crate) fn interrupted() -> bool {
    crate::shell::is_interrupted()
}

pub(crate) fn pump() {
    crate::net::process_packets();
    for _ in 0..2000 {
        core::hint::spin_loop();
    }
}

pub(crate) fn parse_ipv4(value: &str) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut parts = 0;
    for part in value.split('.') {
        if parts >= 4 {
            return None;
        }
        ip[parts] = part.parse::<u8>().ok()?;
        parts += 1;
    }
    if parts == 4 {
        Some(ip)
    } else {
        None
    }
}

/// Validate a DNS hostname or IPv4 literal (single labels allowed).
pub(crate) fn validate_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() || host.len() > 253 {
        return Err("Invalid hostname");
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("Invalid hostname");
        }
        for b in label.bytes() {
            if !(b.is_ascii_alphanumeric() || b == b'-') {
                return Err("Invalid hostname");
            }
        }
    }
    Ok(())
}

pub(crate) fn resolve_host(host: &str) -> Result<[u8; 4], &'static str> {
    if let Some(ip) = parse_ipv4(host) {
        return Ok(ip);
    }
    crate::net::dns::resolve_ipv4(host)
}

pub(crate) fn connect_wait(ip: [u8; 4], port: u16, timeout_ms: u64) -> Result<u16, &'static str> {
    let local = crate::net::tcp::connect(ip, port)?;
    let start = now_ms();
    loop {
        pump();
        match crate::net::tcp::get_state(local) {
            Some(crate::net::tcp::TcpState::Established) => return Ok(local),
            Some(crate::net::tcp::TcpState::CloseWait)
            | Some(crate::net::tcp::TcpState::Closed) => {
                cleanup(local);
                return Err("Connection closed by server");
            }
            _ => {}
        }
        if interrupted() {
            cleanup(local);
            return Err("Cancelled");
        }
        if now_ms().saturating_sub(start) >= timeout_ms {
            cleanup(local);
            return Err("Connection timed out");
        }
    }
}

pub(crate) fn cleanup(local_port: u16) {
    let _ = crate::net::tcp::close(local_port);
    // Let the FIN out without blocking the shell for long.
    for _ in 0..20 {
        pump();
    }
    crate::net::tcp::forget(local_port);
}

pub(crate) fn send_all(local_port: u16, data: &[u8]) -> Result<(), &'static str> {
    let mut off = 0;
    while off < data.len() {
        if interrupted() {
            return Err("Cancelled");
        }
        let end = core::cmp::min(off + TCP_CHUNK, data.len());
        crate::net::tcp::send_data(local_port, &data[off..end])?;
        off = end;
        pump();
    }
    Ok(())
}

pub(crate) fn find_headers_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    for i in 0..buf.len().saturating_sub(3) {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
            return Some(i + 4);
        }
    }
    None
}

pub(crate) fn parse_status(buf: &[u8]) -> Result<u16, &'static str> {
    // Expect `HTTP/1.x NNN ...`.
    let line_end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    let line = &buf[..line_end];
    if line.len() < 12 || &line[..5] != b"HTTP/" {
        log_bad_status(buf, line_end);
        return Err("Invalid HTTP response");
    }
    let code_slice = line.get(9..12).ok_or("Invalid HTTP response")?;
    if !code_slice.iter().all(|b| b.is_ascii_digit()) {
        log_bad_status(buf, line_end);
        return Err("Invalid HTTP response");
    }
    let code = ((code_slice[0] - b'0') as u16) * 100
        + ((code_slice[1] - b'0') as u16) * 10
        + ((code_slice[2] - b'0') as u16);
    Ok(code)
}

/// Serial diagnostics for an unparseable status line (helps tell a
/// mangled stream apart from a non-HTTP server reply).
fn log_bad_status(buf: &[u8], line_end: usize) {
    crate::net_log!(
        "HTTP: bad status line (header {} bytes, first line {} bytes)",
        buf.len(),
        line_end
    );
    crate::net_print!("HTTP: head bytes:");
    for (i, &b) in buf.iter().enumerate().take(64) {
        if i % 16 == 0 {
            crate::net_print!("\nHTTP: {:04x}:", i);
        }
        crate::net_print!(" {:02x}", b);
    }
    crate::net_log!("");
}

fn lower_byte(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

pub(crate) fn parse_content_length(headers: &[u8]) -> Option<u64> {
    // Case-insensitive scan for `content-length: <digits>`.
    let needle = b"content-length:";
    if headers.len() < needle.len() {
        return None;
    }
    for i in 0..=headers.len() - needle.len() {
        let mut ok = true;
        for (j, &nb) in needle.iter().enumerate() {
            if lower_byte(headers[i + j]) != nb {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        let mut k = i + needle.len();
        while k < headers.len() && (headers[k] == b' ' || headers[k] == b'\t') {
            k += 1;
        }
        let mut val: u64 = 0;
        let mut digits = 0;
        while k < headers.len() && headers[k].is_ascii_digit() {
            val = val
                .saturating_mul(10)
                .saturating_add((headers[k] - b'0') as u64);
            digits += 1;
            k += 1;
            if digits > 10 {
                break;
            }
        }
        if digits > 0 {
            return Some(val);
        }
        return None;
    }
    None
}

pub(crate) fn is_chunked(headers: &[u8]) -> bool {
    let needle = b"transfer-encoding:";
    if headers.len() < needle.len() {
        return false;
    }
    for i in 0..=headers.len() - needle.len() {
        let mut ok = true;
        for (j, &nb) in needle.iter().enumerate() {
            if lower_byte(headers[i + j]) != nb {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        // Value contains `chunked`?
        let tail = &headers[i..core::cmp::min(i + 60, headers.len())];
        for w in 0..tail.len().saturating_sub(6) {
            if lower_byte(tail[w]) == b'c'
                && lower_byte(tail[w + 1]) == b'h'
                && lower_byte(tail[w + 2]) == b'u'
                && lower_byte(tail[w + 3]) == b'n'
                && lower_byte(tail[w + 4]) == b'k'
                && lower_byte(tail[w + 5]) == b'e'
                && lower_byte(tail[w + 6]) == b'd'
            {
                return true;
            }
        }
    }
    false
}

/// Extract a `Location:` header value (for 3xx redirects), trimmed.
///
/// Returns the raw value bytes (up to 200 bytes). Rejects empty values
/// and values containing control bytes or raw spaces.
pub(crate) fn parse_location(headers: &[u8]) -> Option<Vec<u8>> {
    let needle = b"location:";
    if headers.len() < needle.len() {
        return None;
    }
    for i in 0..=headers.len() - needle.len() {
        let mut ok = true;
        for (j, &nb) in needle.iter().enumerate() {
            if lower_byte(headers[i + j]) != nb {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        let mut k = i + needle.len();
        while k < headers.len() && (headers[k] == b' ' || headers[k] == b'\t') {
            k += 1;
        }
        let start = k;
        while k < headers.len() && headers[k] != b'\r' && headers[k] != b'\n' {
            k += 1;
        }
        let mut end = k;
        while end > start && (headers[end - 1] == b' ' || headers[end - 1] == b'\t') {
            end -= 1;
        }
        if end <= start || end - start > 200 {
            return None;
        }
        let value = &headers[start..end];
        for &b in value {
            if b < 0x20 || b == 0x7F || b == b' ' {
                return None;
            }
        }
        return Some(value.to_vec());
    }
    None
}

pub(crate) fn build_get(host: &str, path: &str) -> Vec<u8> {
    let mut req = Vec::with_capacity(192 + path.len() + host.len());
    req.extend_from_slice(b"GET ");
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(b"\r\nUser-Agent: MFK/1.0\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    req
}
