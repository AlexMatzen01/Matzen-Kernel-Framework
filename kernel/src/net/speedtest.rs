//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! LibreSpeed-compatible internet speedtest (HTTP/HTTPS, single stream).
//!
//! Uses only the existing MFK network stack: DNS (`net::dns`), TCP
//! (`net::tcp`), packet pump (`net::process_packets`) and the PIT clock
//! (`shell::get_tick_count`). No second stack or browser needed.
//!
//! LibreSpeed backend protocol (verified against
//! `librespeed/speedtest` `backend/empty.php`, `backend/garbage.php` and
//! `speedtest_worker.js`):
//! - Latency:  `GET <base>empty.php?r=<rand>` -> `200 OK`, headers only.
//! - Download: `GET <base>garbage.php?ckSize=<1..1024>&r=<rand>` ->
//!   `ckSize * 1 MiB` of random bytes (`application/octet-stream`).
//! - Upload:   `POST <base>empty.php?r=<rand>` with a binary body
//!   (`Content-Encoding: identity`), server discards it and replies `200`.
//!
//! Throughput is `body_bytes * 8 / elapsed`, displayed in Mbps
//! (1 Mbps = 1_000_000 bit/s). Only the HTTP body counts, not headers.
//!
//! Server configuration supports HTTP and TLS 1.3 HTTPS:
//! - `speedtest-server` shows the current server.
//! - `speedtest-server <host>[:port][/base/]>` sets it.
//! - `speedtest [host[:port][/base/]]` runs once, optionally overriding
//!   the stored server for that run only.
//! Accepted forms (all equivalent after normalization):
//!   `fra.speedtest.clouvider.net`
//!   `fra.speedtest.clouvider.net:80 /backend/`
//!   `fra.speedtest.clouvider.net:80/backend/`
//!   `http://fra.speedtest.clouvider.net/backend/`
//!   `https://speedtest.example/backend/`
//!   `10.0.2.2:8080/` (self-hosted LibreSpeed on the QEMU host)

use alloc::string::String;
use alloc::vec::Vec;
use embedded_io::{Read, Write};
use lazy_static::lazy_static;
use spin::Mutex;

// ── Defaults ────────────────────────────────────────────────
// Public LibreSpeed servers that speak plain HTTP on port 80 (entries
// using `//...` in upstream `server-list.json` support both HTTP/HTTPS).
// This default is only a starting point; any LibreSpeed PHP backend works.
// Prefer a host-local server (e.g. `10.0.2.2`) when the VM has no route
// to the public internet.
pub const DEFAULT_HOST: &str = "fra.speedtest.clouvider.net";
pub const DEFAULT_PORT: u16 = 80;
pub const DEFAULT_BASE: &str = "/backend/";

const DL_FILE: &str = "garbage.php";
const UL_FILE: &str = "empty.php";
const PING_FILE: &str = "empty.php";

// Small single-stream profile (streams to fit the kernel heap; minimal
// TCP implementation without retransmission).
const PING_SAMPLES: usize = 8;
const PING_TIMEOUT_MS: u64 = 5000;
const DOWNLOAD_CKSIZE_MB: u64 = 1;
const DOWNLOAD_TIMEOUT_MS: u64 = 25000;
const UPLOAD_BYTES: u32 = 128 * 1024; // 131_072
const UPLOAD_TIMEOUT_MS: u64 = 20000;

#[derive(Clone)]
struct Config {
    secure: bool,
    host: String,
    port: u16,
    base: String,
}

lazy_static! {
    static ref CONFIG: Mutex<Config> = Mutex::new(Config {
        secure: false,
        host: String::from(DEFAULT_HOST),
        port: DEFAULT_PORT,
        base: String::from(DEFAULT_BASE),
    });
}

/// Human-readable `host:port/base` for shell output.
pub fn describe() -> String {
    let cfg = CONFIG.lock();
    alloc::format!(
        "{}://{}:{}{}",
        if cfg.secure { "https" } else { "http" },
        cfg.host,
        cfg.port,
        cfg.base
    )
}

/// Show current server plus expected format.
pub fn cmd_server(args: &str) {
    let (_net_dbg, args_owned) = super::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    let args = args.trim();
    if args.is_empty() || args == "--help" || args == "-h" || args == "help" {
        let cur = describe();
        crate::println!("Speedtest server: {}", cur);
        crate::println!("  ping: GET {}empty.php", base_of(&cur));
        crate::println!("  down: GET {}garbage.php?ckSize=1", base_of(&cur));
        crate::println!("  up:   POST {}empty.php", base_of(&cur));
        crate::println!("Usage: speedtest-server <host>[:port][/base/]");
        crate::println!("  e.g. speedtest-server fra.speedtest.clouvider.net");
        crate::println!("  e.g. speedtest-server fra.speedtest.clouvider.net:80 /backend/");
        crate::println!("  e.g. speedtest-server 10.0.2.2:8080 /");
        crate::println!("  e.g. speedtest-server http://10.0.2.2/backend/");
        crate::println!("HTTP works by default; HTTPS requires a net_tls-enabled build.");
        crate::println!("Needs backend/empty.php");
        crate::println!("and backend/garbage.php (LibreSpeed PHP backend).");
        return;
    }
    match set_server(args) {
        Ok(()) => crate::println!("Speedtest server set to {}", describe()),
        Err(e) => crate::println!("speedtest-server: {}: '{}'", e, args),
    }
}

fn base_of(desc: &str) -> &str {
    let start = desc.find("://").map(|i| i + 3).unwrap_or(0);
    match desc[start..].find('/') {
        Some(pos) => &desc[start + pos..],
        None => "/",
    }
}

/// Parse `host[:port][/base]` (optional `http://` prefix) and store it.
pub fn set_server(arg: &str) -> Result<(), &'static str> {
    let (secure, host, port, base) = parse_server_arg(arg, None)?;
    let mut cfg = CONFIG.lock();
    cfg.secure = secure;
    cfg.host = host;
    cfg.port = port;
    cfg.base = base;
    Ok(())
}

/// Parse a server argument. `fallback` supplies port/base when the arg
/// omits them (stored config for `speedtest <host>`, defaults for set).
fn parse_server_arg(
    arg: &str,
    fallback: Option<(bool, u16, String)>,
) -> Result<(bool, String, u16, String), &'static str> {
    let mut s = arg.trim();
    if s.is_empty() {
        return Err("Empty server");
    }
    if s.len() > 200 {
        return Err("Server string too long");
    }
    // Strip scheme.
    let secure = if s.starts_with("http://") || s.starts_with("HTTP://") {
        s = &s[7..];
        false
    } else if s.starts_with("https://") || s.starts_with("HTTPS://") {
        s = &s[8..];
        true
    } else {
        false
    };
    // Split hostport / path at first '/' or whitespace.
    let mut hostport_end = s.len();
    for (i, c) in s.char_indices() {
        if c == '/' || c == ' ' || c == '\t' {
            hostport_end = i;
            break;
        }
    }
    let hostport = s[..hostport_end].trim();
    let mut rest = s[hostport_end..].trim();
    // Allow `host:port /base/` (space separated) as well as `host:port/base/`.
    if !rest.is_empty() {
        if let Some(first) = rest.split_whitespace().next() {
            // If rest starts with '/', it is the base; otherwise the first
            // token may still be a base without leading '/'.
            let _ = first;
        }
        // Take only the first whitespace-separated token as the base so
        // trailing junk does not corrupt the config.
        if let Some(tok) = rest.split_whitespace().next() {
            rest = tok;
        }
    }
    if hostport.is_empty() {
        return Err("Missing hostname");
    }
    // Split optional :port.
    let (host, port_opt) = match hostport.rfind(':') {
        Some(colon) => {
            let (h, p) = (&hostport[..colon], &hostport[colon + 1..]);
            if p.is_empty() {
                (h, None)
            } else if p.bytes().all(|b| b.is_ascii_digit()) {
                let port: u16 = p.parse().map_err(|_| "Invalid port")?;
                if port == 0 {
                    return Err("Invalid port");
                }
                (h, Some(port))
            } else {
                // Colon is part of a bad hostname, not a port.
                return Err("Invalid port");
            }
        }
        None => (hostport, None),
    };
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err("Invalid hostname");
    }
    for label in host.split('.') {
        // Allow single-label names (e.g. `localhost`) and IPv4.
        if label.is_empty() || label.len() > 63 {
            return Err("Invalid hostname");
        }
        for b in label.bytes() {
            if !(b.is_ascii_alphanumeric() || b == b'-') {
                return Err("Invalid hostname");
            }
        }
    }
    let (fb_secure, fb_port, fb_base) = fallback.unwrap_or((
        secure,
        if secure { 443 } else { DEFAULT_PORT },
        String::from(DEFAULT_BASE),
    ));
    let port = port_opt.unwrap_or(fb_port);
    let base = if rest.is_empty() {
        fb_base
    } else {
        normalize_base(rest)?
    };
    Ok((
        if s == arg.trim() { fb_secure } else { secure },
        String::from(host),
        port,
        base,
    ))
}

fn normalize_base(path: &str) -> Result<String, &'static str> {
    let p = path.trim();
    if p.is_empty() {
        return Ok(String::from("/"));
    }
    if p.len() > 64 {
        return Err("Base path too long");
    }
    for b in p.bytes() {
        if !(b.is_ascii_alphanumeric() || b == b'/' || b == b'-' || b == b'_' || b == b'.') {
            return Err("Invalid base path");
        }
    }
    let mut base = String::from("/");
    base.push_str(p.trim_matches('/'));
    if !base.ends_with('/') {
        base.push('/');
    }
    if base.len() > 64 {
        return Err("Base path too long");
    }
    Ok(base)
}

use super::http::{
    build_get, cleanup, connect_wait, find_headers_end, interrupted, is_chunked, now_ms,
    parse_content_length, parse_status, pump, resolve_host, send_all, CONNECT_TIMEOUT_MS,
    MAX_HEADER, TCP_CHUNK,
};

fn cache_buster(extra: u64) -> u32 {
    (now_ms().wrapping_add(extra).wrapping_mul(2654435761) & 0x7FFF_FFFF) as u32 | 1
}

fn build_post_headers(host: &str, path: &str, content_len: u32) -> Vec<u8> {
    let mut req = Vec::with_capacity(256 + path.len() + host.len());
    req.extend_from_slice(b"POST ");
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(b"\r\nUser-Agent: MFK-speedtest/1.0\r\nAccept: */*\r\n");
    req.extend_from_slice(b"Content-Type: application/octet-stream\r\n");
    req.extend_from_slice(b"Content-Encoding: identity\r\nConnection: close\r\nContent-Length: ");
    // Small integer formatting without `format!` to keep heap use tiny.
    let mut digits = [0u8; 10];
    let mut n = content_len as u64;
    let mut len = 0;
    if n == 0 {
        digits[0] = b'0';
        len = 1;
    } else {
        let mut tmp = [0u8; 10];
        while n > 0 && len < 10 {
            tmp[len] = (n % 10) as u8 + b'0';
            n /= 10;
            len += 1;
        }
        for i in 0..len {
            digits[i] = tmp[len - 1 - i];
        }
    }
    req.extend_from_slice(&digits[..len]);
    req.extend_from_slice(b"\r\n\r\n");
    req
}

// Simple xorshift32 for incompressible-ish upload bytes.
fn rand_byte(state: &mut u32) -> u8 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x >> 24) as u8
}

/// Single HTTP latency sample: time from GET send to response headers.
fn ping_once(
    secure: bool,
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
    bust: u32,
) -> Result<u64, &'static str> {
    if secure {
        return ping_tls_once(host, base, ip, port, bust);
    }
    let path = alloc::format!("{}{}?r={}", base, PING_FILE, bust);
    let local = connect_wait(ip, port, CONNECT_TIMEOUT_MS)?;
    let req = build_get(host, &path);
    let start = now_ms();
    let res: Result<u64, &'static str> = (|| {
        send_all(local, &req)?;
        let mut headers: Vec<u8> = Vec::new();
        loop {
            pump();
            if let Some(chunk) = crate::net::tcp::read_data(local) {
                if headers.len() + chunk.len() > MAX_HEADER + 1460 {
                    return Err("HTTP header too large");
                }
                headers.extend_from_slice(&chunk);
                if let Some(end) = find_headers_end(&headers) {
                    let code = parse_status(&headers[..end])?;
                    if code != 200 {
                        return Err("Ping request failed (HTTP status)");
                    }
                    return Ok(now_ms().saturating_sub(start));
                }
            }
            if interrupted() {
                return Err("Cancelled");
            }
            if now_ms().saturating_sub(start) >= PING_TIMEOUT_MS {
                return Err("Ping timed out");
            }
            match crate::net::tcp::get_state(local) {
                Some(crate::net::tcp::TcpState::CloseWait)
                | Some(crate::net::tcp::TcpState::Closed) => {
                    if find_headers_end(&headers).is_some() {
                        let end = find_headers_end(&headers).unwrap_or(headers.len());
                        let code = parse_status(&headers[..end])?;
                        if code == 200 {
                            return Ok(now_ms().saturating_sub(start));
                        }
                    }
                    return Err("Server closed connection");
                }
                _ => {}
            }
        }
    })();
    cleanup(local);
    res
}

fn tls_stream<'a>(
    host: &'a str,
    ip: [u8; 4],
    port: u16,
    root: &'a [u8],
    read_storage: &'a mut Vec<u8>,
    write_storage: &'a mut Vec<u8>,
) -> Result<super::tls::TlsStream<'a>, &'static str> {
    super::tls::TlsStream::connect(
        host,
        ip,
        port,
        root,
        read_storage.as_mut_slice(),
        write_storage.as_mut_slice(),
    )
    .map_err(|_| "TLS handshake or certificate verification failed")
}

fn ping_tls_once(
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
    bust: u32,
) -> Result<u64, &'static str> {
    let path = alloc::format!("{}{}?r={}", base, PING_FILE, bust);
    let mut root_storage = Vec::new();
    root_storage.resize(2048, 0);
    let root = super::tls::load_default_root(&mut root_storage)?;
    let mut read_storage = Vec::new();
    read_storage.resize(16_640, 0);
    let mut write_storage = Vec::new();
    write_storage.resize(16_640, 0);
    let mut stream = tls_stream(host, ip, port, root, &mut read_storage, &mut write_storage)?;
    stream
        .write_all(&build_get(host, &path))
        .map_err(|_| "TLS request failed")?;
    let start = now_ms();
    let mut headers = Vec::new();
    let mut incoming = [0u8; TCP_CHUNK];
    loop {
        let count = stream
            .read(&mut incoming)
            .map_err(|_| "TLS response read failed")?;
        if count == 0 {
            return Err("Server closed connection");
        }
        headers.extend_from_slice(&incoming[..count]);
        if headers.len() > MAX_HEADER && find_headers_end(&headers).is_none() {
            return Err("HTTP header too large");
        }
        if let Some(end) = find_headers_end(&headers) {
            if parse_status(&headers[..end])? != 200 {
                return Err("Ping request failed (HTTP status)");
            }
            return Ok(now_ms().saturating_sub(start));
        }
        if interrupted() || now_ms().saturating_sub(start) >= PING_TIMEOUT_MS {
            return Err("Ping timed out");
        }
    }
}

/// Download `ckSize` MiB via garbage.php, streaming (no big heap buffer).
fn download_once(
    secure: bool,
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
) -> Result<(u64, u64), &'static str> {
    if secure {
        return download_tls_once(host, base, ip, port);
    }
    let path = alloc::format!(
        "{}{}?ckSize={}&r={}",
        base,
        DL_FILE,
        DOWNLOAD_CKSIZE_MB,
        cache_buster(0xD1)
    );
    let local = connect_wait(ip, port, CONNECT_TIMEOUT_MS)?;
    let req = build_get(host, &path);
    let start = now_ms();
    let res: Result<(u64, u64), &'static str> = (|| {
        send_all(local, &req)?;
        let mut headers: Vec<u8> = Vec::new();
        let mut header_len: Option<usize> = None;
        let mut content_len: Option<u64> = None;
        let mut body: u64 = 0;
        let mut last_data = now_ms();
        loop {
            pump();
            if let Some(chunk) = crate::net::tcp::read_data(local) {
                last_data = now_ms();
                if let Some(hl) = header_len {
                    let _ = hl;
                    body = body.saturating_add(chunk.len() as u64);
                } else {
                    headers.extend_from_slice(&chunk);
                    // Headers (without body) must fit in the first ~8 KiB.
                    // A single packet may already carry body bytes after the
                    // terminator, so allow one extra segment of slack.
                    if headers.len() > MAX_HEADER + TCP_CHUNK
                        && find_headers_end(&headers).is_none()
                    {
                        return Err("HTTP header too large");
                    }
                    if let Some(end) = find_headers_end(&headers) {
                        let code = parse_status(&headers[..end])?;
                        if code != 200 {
                            return Err("Download failed (HTTP status)");
                        }
                        if is_chunked(&headers[..end]) {
                            return Err("Chunked encoding not supported");
                        }
                        content_len = parse_content_length(&headers[..end]);
                        body = body.saturating_add((headers.len() - end) as u64);
                        header_len = Some(end);
                        // Keep only headers to bound heap use.
                        headers.truncate(end);
                    }
                }
                if let (Some(_), Some(cl)) = (header_len, content_len) {
                    if body >= cl && cl > 0 {
                        body = cl;
                        break;
                    }
                }
                // Close-delimited fallback: garbage.php always sends at
                // least ckSize MiB; stop once we have that much even if the
                // server omits Content-Length.
                if body >= DOWNLOAD_CKSIZE_MB * 1024 * 1024 {
                    break;
                }
            }
            if interrupted() {
                return Err("Cancelled");
            }
            let elapsed = now_ms().saturating_sub(start);
            if elapsed >= DOWNLOAD_TIMEOUT_MS {
                if body == 0 {
                    return Err("Download timed out (no data)");
                }
                if let Some(cl) = content_len {
                    if body < cl {
                        return Err("Download incomplete (timeout)");
                    }
                } else if body < DOWNLOAD_CKSIZE_MB * 1024 * 1024 {
                    return Err("Download incomplete (timeout)");
                }
                break;
            }
            // Server closed early: accept what we have if it meets the
            // expected size, else report incomplete.
            match crate::net::tcp::get_state(local) {
                Some(crate::net::tcp::TcpState::CloseWait)
                | Some(crate::net::tcp::TcpState::Closed) => {
                    // Drain once more, then decide after a short grace.
                    if now_ms().saturating_sub(last_data) > 800 {
                        if body == 0 {
                            return Err("Server closed connection (no data)");
                        }
                        if let Some(cl) = content_len {
                            if body < cl {
                                return Err("Download incomplete (server closed)");
                            }
                        } else if body < DOWNLOAD_CKSIZE_MB * 1024 * 1024 {
                            return Err("Download incomplete (server closed)");
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        let elapsed = now_ms().saturating_sub(start);
        if body == 0 {
            return Err("Download got 0 bytes");
        }
        Ok((body, elapsed))
    })();
    cleanup(local);
    res
}

fn download_tls_once(
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
) -> Result<(u64, u64), &'static str> {
    let path = alloc::format!(
        "{}{}?ckSize={}&r={}",
        base,
        DL_FILE,
        DOWNLOAD_CKSIZE_MB,
        cache_buster(0xD1)
    );
    let mut root_storage = Vec::new();
    root_storage.resize(2048, 0);
    let root = super::tls::load_default_root(&mut root_storage)?;
    let mut read_storage = Vec::new();
    read_storage.resize(16_640, 0);
    let mut write_storage = Vec::new();
    write_storage.resize(16_640, 0);
    let mut stream = tls_stream(host, ip, port, root, &mut read_storage, &mut write_storage)?;
    stream
        .write_all(&build_get(host, &path))
        .map_err(|_| "TLS request failed")?;
    let start = now_ms();
    let mut headers = Vec::new();
    let mut header_len = None;
    let mut content_len = None;
    let mut body = 0u64;
    let mut incoming = [0u8; TCP_CHUNK];
    loop {
        let count = stream
            .read(&mut incoming)
            .map_err(|_| "TLS response read failed")?;
        if count == 0 {
            break;
        }
        let chunk = &incoming[..count];
        if let Some(end) = header_len {
            let _ = end;
            body = body.saturating_add(chunk.len() as u64);
        } else {
            headers.extend_from_slice(chunk);
            if headers.len() > MAX_HEADER + TCP_CHUNK && find_headers_end(&headers).is_none() {
                return Err("HTTP header too large");
            }
            if let Some(end) = find_headers_end(&headers) {
                if parse_status(&headers[..end])? != 200 {
                    return Err("Download failed (HTTP status)");
                }
                if is_chunked(&headers[..end]) {
                    return Err("Chunked encoding not supported");
                }
                content_len = parse_content_length(&headers[..end]);
                body = body.saturating_add((headers.len() - end) as u64);
                header_len = Some(end);
                headers.truncate(end);
            }
        }
        if let Some(cl) = content_len {
            if body >= cl {
                body = cl;
                break;
            }
        }
        if body >= DOWNLOAD_CKSIZE_MB * 1024 * 1024 {
            break;
        }
        if interrupted() || now_ms().saturating_sub(start) >= DOWNLOAD_TIMEOUT_MS {
            return Err("Download timed out");
        }
    }
    if body == 0 {
        return Err("Download got 0 bytes");
    }
    if let Some(cl) = content_len {
        if body < cl {
            return Err("Download incomplete");
        }
    }
    Ok((body, now_ms().saturating_sub(start)))
}

/// Upload pseudo-random bytes via POST to empty.php.
fn upload_once(
    secure: bool,
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
) -> Result<(u64, u64), &'static str> {
    if secure {
        return upload_tls_once(host, base, ip, port);
    }
    let path = alloc::format!("{}{}?r={}", base, UL_FILE, cache_buster(0x51));
    let local = connect_wait(ip, port, CONNECT_TIMEOUT_MS)?;
    let hdr = build_post_headers(host, &path, UPLOAD_BYTES);
    let start = now_ms();
    let res: Result<(u64, u64), &'static str> = (|| {
        send_all(local, &hdr)?;
        let mut state: u32 = (now_ms() as u32)
            .wrapping_mul(2246822519)
            .wrapping_add(0x9E37)
            | 1;
        let mut chunk = [0u8; TCP_CHUNK];
        let mut sent: u64 = 0;
        let total = UPLOAD_BYTES as u64;
        while sent < total {
            if interrupted() {
                return Err("Cancelled");
            }
            let n = core::cmp::min(TCP_CHUNK as u64, total - sent) as usize;
            for b in chunk[..n].iter_mut() {
                *b = rand_byte(&mut state);
            }
            crate::net::tcp::send_data(local, &chunk[..n])?;
            sent += n as u64;
            pump();
            if now_ms().saturating_sub(start) >= UPLOAD_TIMEOUT_MS {
                return Err("Upload timed out (sending)");
            }
        }
        // Wait for the empty.php 200 response headers.
        let send_done = now_ms();
        let _ = send_done;
        let mut headers: Vec<u8> = Vec::new();
        loop {
            pump();
            if let Some(data) = crate::net::tcp::read_data(local) {
                headers.extend_from_slice(&data);
                if headers.len() > MAX_HEADER {
                    return Err("HTTP header too large");
                }
                if let Some(end) = find_headers_end(&headers) {
                    let code = parse_status(&headers[..end])?;
                    if code != 200 {
                        return Err("Upload failed (HTTP status)");
                    }
                    break;
                }
            }
            if interrupted() {
                return Err("Cancelled");
            }
            if now_ms().saturating_sub(start) >= UPLOAD_TIMEOUT_MS {
                return Err("Upload timed out (no response)");
            }
        }
        let elapsed = now_ms().saturating_sub(start);
        Ok((sent, elapsed))
    })();
    cleanup(local);
    res
}

fn upload_tls_once(
    host: &str,
    base: &str,
    ip: [u8; 4],
    port: u16,
) -> Result<(u64, u64), &'static str> {
    let path = alloc::format!("{}{}?r={}", base, UL_FILE, cache_buster(0x51));
    let mut root_storage = Vec::new();
    root_storage.resize(2048, 0);
    let root = super::tls::load_default_root(&mut root_storage)?;
    let mut read_storage = Vec::new();
    read_storage.resize(16_640, 0);
    let mut write_storage = Vec::new();
    write_storage.resize(16_640, 0);
    let mut stream = tls_stream(host, ip, port, root, &mut read_storage, &mut write_storage)?;
    let hdr = build_post_headers(host, &path, UPLOAD_BYTES);
    let start = now_ms();
    stream.write_all(&hdr).map_err(|_| "TLS request failed")?;
    let mut state = (now_ms() as u32)
        .wrapping_mul(2246822519)
        .wrapping_add(0x9E37)
        | 1;
    let mut chunk = [0u8; TCP_CHUNK];
    let mut sent = 0u64;
    while sent < UPLOAD_BYTES as u64 {
        if interrupted() {
            return Err("Cancelled");
        }
        let n = core::cmp::min(TCP_CHUNK as u64, UPLOAD_BYTES as u64 - sent) as usize;
        for byte in &mut chunk[..n] {
            *byte = rand_byte(&mut state);
        }
        stream
            .write_all(&chunk[..n])
            .map_err(|_| "TLS upload failed")?;
        sent += n as u64;
        if now_ms().saturating_sub(start) >= UPLOAD_TIMEOUT_MS {
            return Err("Upload timed out (sending)");
        }
    }
    let mut headers = Vec::new();
    let mut incoming = [0u8; TCP_CHUNK];
    loop {
        let count = stream
            .read(&mut incoming)
            .map_err(|_| "TLS response read failed")?;
        if count == 0 {
            return Err("Server closed connection");
        }
        headers.extend_from_slice(&incoming[..count]);
        if headers.len() > MAX_HEADER && find_headers_end(&headers).is_none() {
            return Err("HTTP header too large");
        }
        if let Some(end) = find_headers_end(&headers) {
            if parse_status(&headers[..end])? != 200 {
                return Err("Upload failed (HTTP status)");
            }
            return Ok((sent, now_ms().saturating_sub(start)));
        }
        if interrupted() || now_ms().saturating_sub(start) >= UPLOAD_TIMEOUT_MS {
            return Err("Upload timed out (no response)");
        }
    }
}

fn print_mbps(label: &str, bytes: u64, elapsed_ms: u64) {
    let elapsed = elapsed_ms.max(1);
    let bits = bytes.saturating_mul(8);
    let bps = bits.saturating_mul(1000) / elapsed;
    let whole = bps / 1_000_000;
    let frac = (bps % 1_000_000) / 10_000; // two decimals
    crate::println!(
        "{}: {}.{:02} Mbps ({} bytes in {} ms)",
        label,
        whole,
        frac,
        bytes,
        elapsed_ms
    );
}

/// Entry point for `speedtest [server-override]`.
pub fn cmd_run(args: &str) {
    let (_net_dbg, args_owned) = super::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    let args = args.trim();
    if args == "--help" || args == "-h" || args == "help" {
        crate::println!("Usage: speedtest [-d|--debug] [server]");
        crate::println!("  Runs latency + download + upload against a LibreSpeed");
        crate::println!("  LibreSpeed backend over HTTP (HTTPS requires net_tls build feature).");
        crate::println!("  server: <host>[:port][/base/], e.g.:");
        crate::println!("    speedtest");
        crate::println!("    speedtest fra.speedtest.clouvider.net");
        crate::println!("    speedtest 10.0.2.2:8080 /");
        crate::println!("  Configure default: speedtest-server <server>");
        crate::println!("  Needs backend/empty.php + backend/garbage.php.");
        crate::println!("  Ctrl+C cancels. Clock granularity is 10 ms.");
        return;
    }
    if crate::net::ip::get_ip_address().is_none() {
        crate::println!("Network not configured. Run 'ifconfig 10.0.2.15' first.");
        return;
    }
    // Resolve effective config (stored + optional one-shot override).
    let (secure, host, port, base): (bool, String, u16, String) = {
        let cur = CONFIG.lock().clone();
        if args.is_empty() {
            (cur.secure, cur.host, cur.port, cur.base)
        } else {
            match parse_server_arg(args, Some((cur.secure, cur.port, cur.base))) {
                Ok(v) => v,
                Err(e) => {
                    crate::println!("speedtest: {}: '{}'", e, args);
                    crate::println!("Try 'speedtest --help' or 'speedtest-server'.");
                    return;
                }
            }
        }
    };
    crate::shell::clear_interrupt();
    crate::println!(
        "Speedtest: {}://{}:{}{}",
        if secure { "https" } else { "http" },
        host,
        port,
        base
    );
    crate::println!("Resolving {}...", host);
    let ip = match resolve_host(&host) {
        Ok(ip) => {
            crate::println!(
                "Server {}:{}, IP {}.{}.{}.{}",
                host,
                port,
                ip[0],
                ip[1],
                ip[2],
                ip[3]
            );
            ip
        }
        Err(e) => {
            crate::println!("DNS failed for '{}': {}", host, e);
            return;
        }
    };

    // ── Latency ──
    crate::println!("Latency: {}x GET {}empty.php ...", PING_SAMPLES, base);
    let mut samples: Vec<u64> = Vec::new();
    let mut fails = 0;
    for i in 0..PING_SAMPLES {
        if interrupted() {
            crate::println!("Speedtest cancelled.");
            crate::shell::clear_interrupt();
            return;
        }
        match ping_once(secure, &host, &base, ip, port, cache_buster(i as u64)) {
            Ok(ms) => {
                crate::println!("  ping {}: {} ms", i + 1, ms);
                samples.push(ms);
            }
            Err(e) => {
                fails += 1;
                crate::println!("  ping {}: {}", i + 1, e);
            }
        }
    }
    if samples.is_empty() {
        crate::println!("Latency: failed ({} errors). Aborting.", fails);
        crate::println!("Check server, DNS (10.0.2.3) and route via 10.0.2.2.");
        return;
    }
    let mut min = samples[0];
    let mut sum: u64 = 0;
    for &s in samples.iter() {
        if s < min {
            min = s;
        }
        sum = sum.saturating_add(s);
    }
    let avg = sum / (samples.len() as u64);
    crate::println!(
        "Latency: min {} ms, avg {} ms ({}/{} ok)",
        min,
        avg,
        samples.len(),
        PING_SAMPLES
    );

    // ── Download ──
    crate::println!(
        "Download: GET {}garbage.php?ckSize={} ...",
        base,
        DOWNLOAD_CKSIZE_MB
    );
    match download_once(secure, &host, &base, ip, port) {
        Ok((bytes, elapsed)) => print_mbps("Download", bytes, elapsed),
        Err(e) => {
            crate::println!("Download failed: {}", e);
            if interrupted() {
                crate::println!("Speedtest cancelled.");
                crate::shell::clear_interrupt();
                return;
            }
        }
    }

    // ── Upload ──
    crate::println!(
        "Upload: POST {}empty.php ({} bytes) ...",
        base,
        UPLOAD_BYTES
    );
    match upload_once(secure, &host, &base, ip, port) {
        Ok((bytes, elapsed)) => print_mbps("Upload", bytes, elapsed),
        Err(e) => crate::println!("Upload failed: {}", e),
    }

    if interrupted() {
        crate::shell::clear_interrupt();
    }
    crate::println!(
        "Speedtest done. Server {}://{}:{}{}",
        if secure { "https" } else { "http" },
        host,
        port,
        base
    );
}
