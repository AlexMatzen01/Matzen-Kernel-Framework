//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Minimal HTTP/HTTPS downloader (`wget`).
//!
//! Reuses the shared helpers in `net::http` (DNS, TCP, packet pump,
//! header parsing) — no second stack. Saves the response body
//! to SimplFS via `shell::write_file_contents`.
//!
//! Usage: `wget [-td=5s|--timeout=5s] <http-url> <local-file>`
//! URL format: `http(s)://<host>[:port][/path]` (default ports 80/443).
//! Up to 3 HTTP(S) redirects are followed.
//! Limits: response body capped at the SimplFS max file size
//! (`INODE_DIRECT_BLOCKS * FS_BLOCK_SIZE`, currently 6144 bytes);
//! larger files are refused gracefully before writing anything.

use super::http::{
    build_get, cleanup, connect_wait, find_headers_end, interrupted, is_chunked, now_ms,
    parse_content_length, parse_location, parse_status, pump, resolve_host, send_all,
    validate_host, CONNECT_TIMEOUT_MS, MAX_HEADER,
};
use alloc::string::String;
use alloc::vec::Vec;
use embedded_io::{Read, Write};

/// Max savable body: SimplFS files are direct-blocks only.
const MAX_FILE_BYTES: usize = crate::fs::INODE_DIRECT_BLOCKS * crate::fs::FS_BLOCK_SIZE;
const WGET_TIMEOUT_MS: u64 = 25000;
const MAX_PATH_LEN: usize = 128;
/// Same-scheme redirects followed per invocation.
const MAX_REDIRECTS: usize = 3;

fn parse_timeout(value: &str) -> Result<u64, &'static str> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1000u64)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000u64)
    } else {
        return Err("timeout must end in ms, s, or m");
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "timeout value is invalid")?;
    if number == 0 {
        return Err("timeout must be greater than zero");
    }
    number
        .checked_mul(multiplier)
        .ok_or("timeout value is too large")
}

fn parse_wget_args(args: &str) -> Result<(u64, String), &'static str> {
    let mut timeout = WGET_TIMEOUT_MS;
    let mut positional = String::new();
    for token in args.split_whitespace() {
        let value = token
            .strip_prefix("-td=")
            .or_else(|| token.strip_prefix("--timeout="));
        if let Some(value) = value {
            timeout = parse_timeout(value)?;
            continue;
        }
        if token == "-td" || token == "--timeout" {
            return Err("timeout must use =, e.g. --timeout=5s");
        }
        if !positional.is_empty() {
            positional.push(' ');
        }
        positional.push_str(token);
    }
    Ok((timeout, positional))
}

/// Validate a request path (`/…`, see `parse_http_url` charset).
fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.len() > MAX_PATH_LEN {
        return Err("URL path too long");
    }
    if !path.starts_with('/') {
        return Err("Invalid URL path");
    }
    for b in path.bytes() {
        if !(b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'/' | b'-'
                    | b'_'
                    | b'.'
                    | b'~'
                    | b'?'
                    | b'%'
                    | b'&'
                    | b'='
                    | b'+'
                    | b'#'
                    | b':'
                    | b'@'
            ))
        {
            return Err("Invalid URL path");
        }
    }
    Ok(())
}

/// Split an HTTP(S) URL into `(secure, host, port, path)`.
fn parse_http_url(url: &str) -> Result<(bool, String, u16, String), &'static str> {
    let mut s = url.trim();
    if s.is_empty() {
        return Err("Empty URL");
    }
    if s.len() > 200 {
        return Err("URL too long");
    }
    let secure = if s.starts_with("http://") || s.starts_with("HTTP://") {
        s = &s[7..];
        false
    } else if s.starts_with("https://") || s.starts_with("HTTPS://") {
        s = &s[8..];
        true
    } else {
        return Err("URL must start with http:// or https://");
    };
    // hostport ends at first '/'.
    let split = s.find('/').unwrap_or(s.len());
    let (hostport, path) = (&s[..split], &s[split..]);
    if hostport.is_empty() {
        return Err("Missing hostname");
    }
    let (host, port) = match hostport.rfind(':') {
        Some(colon) => {
            let (h, p) = (&hostport[..colon], &hostport[colon + 1..]);
            if h.is_empty() {
                return Err("Missing hostname");
            }
            if p.is_empty() {
                return Err("Invalid port");
            }
            if !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Invalid port");
            }
            let port: u16 = p.parse().map_err(|_| "Invalid port")?;
            if port == 0 {
                return Err("Invalid port");
            }
            (h, port)
        }
        None => (hostport, if secure { 443 } else { 80 }),
    };
    validate_host(host)?;
    let path = if path.is_empty() { "/" } else { path };
    validate_path(path)?;
    Ok((secure, String::from(host), port, String::from(path)))
}

/// Resolve a `Location:` value against the current request URL.
///
/// Returns the next `(secure, host, port, path)`.
fn resolve_location(
    cur_secure: bool,
    cur_host: &str,
    cur_port: u16,
    cur_path: &str,
    loc: &[u8],
) -> Result<(bool, String, u16, String), &'static str> {
    let loc = core::str::from_utf8(loc).map_err(|_| "Invalid redirect")?;
    let loc = loc.trim();
    if loc.is_empty() || loc.len() > 200 {
        return Err("Invalid redirect");
    }
    if loc.starts_with("http://") || loc.starts_with("HTTP://") {
        return parse_http_url(loc);
    }
    if loc.starts_with("https://") || loc.starts_with("HTTPS://") {
        return parse_http_url(loc);
    }
    if loc.starts_with('/') {
        validate_path(loc)?;
        return Ok((
            cur_secure,
            String::from(cur_host),
            cur_port,
            String::from(loc),
        ));
    }
    // Bare relative reference (`other.html`, `./other.html`): merge with
    // the current path's directory.
    let loc = loc.strip_prefix("./").unwrap_or(loc);
    if loc.is_empty() || loc.contains("://") {
        return Err("Invalid redirect");
    }
    let dir_end = cur_path.rfind('/').map(|p| p + 1).unwrap_or(0);
    let mut merged = String::from(&cur_path[..dir_end]);
    merged.push_str(loc);
    validate_path(&merged)?;
    Ok((cur_secure, String::from(cur_host), cur_port, merged))
}

/// Outcome of one HTTP request: a complete body, or a redirect target.
enum FetchResult {
    Body(Vec<u8>),
    Redirect(Vec<u8>),
}

fn is_redirect(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

/// Fetch a URL body into memory (capped), following no redirects.
/// Redirect responses return `FetchResult::Redirect` for the caller.
fn fetch_once(
    secure: bool,
    host: &str,
    port: u16,
    path: &str,
    ip: [u8; 4],
    timeout_ms: u64,
) -> Result<FetchResult, &'static str> {
    if secure {
        return fetch_https_once(host, port, path, ip, timeout_ms);
    }
    let local = connect_wait(ip, port, core::cmp::min(CONNECT_TIMEOUT_MS, timeout_ms))?;
    let req = build_get(host, path);
    let start = now_ms();
    let res: Result<FetchResult, &'static str> = (|| {
        send_all(local, &req)?;
        let mut headers: Vec<u8> = Vec::new();
        let mut header_len: Option<usize> = None;
        let mut content_len: Option<u64> = None;
        let mut body: Vec<u8> = Vec::new();
        let mut last_data = now_ms();
        loop {
            pump();
            if let Some(chunk) = crate::net::tcp::read_data(local) {
                last_data = now_ms();
                if header_len.is_some() {
                    if body.len() + chunk.len() > MAX_FILE_BYTES {
                        return Err("File too large (limit 6144 bytes)");
                    }
                    body.extend_from_slice(&chunk);
                } else {
                    headers.extend_from_slice(&chunk);
                    if headers.len() > MAX_HEADER + super::http::TCP_CHUNK
                        && find_headers_end(&headers).is_none()
                    {
                        return Err("HTTP header too large");
                    }
                    if let Some(end) = find_headers_end(&headers) {
                        let code = parse_status(&headers[..end])?;
                        if is_redirect(code) {
                            match parse_location(&headers[..end]) {
                                Some(loc) => return Ok(FetchResult::Redirect(loc)),
                                None => return Err("Redirect without Location"),
                            }
                        }
                        if code != 200 {
                            return Err("Download failed (HTTP status)");
                        }
                        if is_chunked(&headers[..end]) {
                            return Err("Chunked encoding not supported");
                        }
                        content_len = parse_content_length(&headers[..end]);
                        if let Some(cl) = content_len {
                            if cl > MAX_FILE_BYTES as u64 {
                                return Err("File too large (limit 6144 bytes)");
                            }
                        }
                        let rest = &headers[end..];
                        if body.len() + rest.len() > MAX_FILE_BYTES {
                            return Err("File too large (limit 6144 bytes)");
                        }
                        body.extend_from_slice(rest);
                        header_len = Some(end);
                        headers.truncate(end);
                    }
                }
                if let Some(cl) = content_len {
                    if (body.len() as u64) >= cl && cl > 0 {
                        body.truncate(cl as usize);
                        break;
                    }
                }
            }
            if interrupted() {
                return Err("Cancelled");
            }
            let elapsed = now_ms().saturating_sub(start);
            if elapsed >= timeout_ms {
                if header_len.is_none() {
                    return Err("Download timed out (no data)");
                }
                if let Some(cl) = content_len {
                    if (body.len() as u64) < cl {
                        return Err("Download incomplete (timeout)");
                    }
                }
                break;
            }
            match crate::net::tcp::get_state(local) {
                Some(crate::net::tcp::TcpState::CloseWait)
                | Some(crate::net::tcp::TcpState::Closed) => {
                    if now_ms().saturating_sub(last_data) > 800 {
                        if header_len.is_none() {
                            return Err("Server closed connection (no data)");
                        }
                        if let Some(cl) = content_len {
                            if (body.len() as u64) < cl {
                                return Err("Download incomplete (server closed)");
                            }
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        Ok(FetchResult::Body(body))
    })();
    cleanup(local);
    res
}

fn fetch_https_once(
    host: &str,
    port: u16,
    path: &str,
    ip: [u8; 4],
    timeout_ms: u64,
) -> Result<FetchResult, &'static str> {
    let mut root_storage = Vec::new();
    root_storage.resize(2048, 0);
    let root = super::tls::load_default_root(&mut root_storage)?;
    let mut read_storage = Vec::new();
    read_storage.resize(16_640, 0);
    let mut write_storage = Vec::new();
    write_storage.resize(16_640, 0);
    let mut stream = super::tls::TlsStream::connect_with_timeout(
        host,
        ip,
        port,
        root,
        &mut read_storage,
        &mut write_storage,
        timeout_ms,
    )
    .map_err(|_| "TLS handshake or certificate verification failed")?;
    let request = build_get(host, path);
    stream
        .write_all(&request)
        .map_err(|_| "TLS request failed")?;

    let mut headers = Vec::new();
    let mut body = Vec::new();
    let mut header_len = None;
    let mut content_len = None;
    let start = now_ms();
    let mut incoming = Vec::new();
    incoming.resize(super::http::TCP_CHUNK, 0);
    loop {
        let count = stream
            .read(&mut incoming)
            .map_err(|_| "TLS response read failed")?;
        if count == 0 {
            break;
        }
        let chunk = &incoming[..count];
        if header_len.is_some() {
            if body.len() + chunk.len() > MAX_FILE_BYTES {
                return Err("File too large (limit 6144 bytes)");
            }
            body.extend_from_slice(chunk);
        } else {
            headers.extend_from_slice(chunk);
            if headers.len() > MAX_HEADER + super::http::TCP_CHUNK
                && find_headers_end(&headers).is_none()
            {
                return Err("HTTP header too large");
            }
            if let Some(end) = find_headers_end(&headers) {
                let code = parse_status(&headers[..end])?;
                if is_redirect(code) {
                    return parse_location(&headers[..end])
                        .map(FetchResult::Redirect)
                        .ok_or("Redirect without Location");
                }
                if code != 200 {
                    return Err("Download failed (HTTP status)");
                }
                if is_chunked(&headers[..end]) {
                    return Err("Chunked encoding not supported");
                }
                content_len = parse_content_length(&headers[..end]);
                if let Some(cl) = content_len {
                    if cl > MAX_FILE_BYTES as u64 {
                        return Err("File too large (limit 6144 bytes)");
                    }
                }
                let rest = &headers[end..];
                if rest.len() > MAX_FILE_BYTES {
                    return Err("File too large (limit 6144 bytes)");
                }
                body.extend_from_slice(rest);
                header_len = Some(end);
            }
        }
        if let Some(cl) = content_len {
            if body.len() as u64 >= cl {
                body.truncate(cl as usize);
                break;
            }
        }
        if interrupted() {
            return Err("Cancelled");
        }
        if now_ms().saturating_sub(start) >= timeout_ms {
            return Err("Download timed out");
        }
    }
    if header_len.is_none() {
        return Err("TLS response contained no HTTP headers");
    }
    if let Some(cl) = content_len {
        if (body.len() as u64) < cl {
            return Err("Download incomplete");
        }
    }
    Ok(FetchResult::Body(body))
}

/// Entry point for `wget <http-url> <local-file>`.
pub fn cmd_run(args: &str) {
    let (_net_dbg, args_owned) = super::debug::DebugGuard::acquire(args);
    let (timeout_ms, args_cleaned) = match parse_wget_args(args_owned.as_str()) {
        Ok(value) => value,
        Err(error) => {
            crate::println!("wget: {}", error);
            return;
        }
    };
    let args = args_cleaned.as_str();
    let args = args.trim();
    if args.is_empty() || args == "--help" || args == "-h" || args == "help" {
        crate::println!("Usage: wget [-d|--debug] [-td=5s|--timeout=5s] <http-url> <local-file>");
        crate::println!("  e.g. wget http://10.0.2.2:8000/hello.txt /docs/hello.txt");
        crate::println!("  Supports HTTP and TLS 1.3 HTTPS. Requires mounted FS ('mount').");
        crate::println!("  Follows up to 3 HTTP(S) redirects.");
        crate::println!("  Timeout: -td=5s or --timeout=5s (also ms and m; default 25s).");
        crate::println!("  Max file size 6144 bytes; larger files are refused.");
        crate::println!("  Ctrl+C cancels. Clock granularity is 10 ms.");
        return;
    }
    let mut parts = args.splitn(2, ' ');
    let url = parts.next().unwrap_or("").trim();
    let dest = parts.next().unwrap_or("").trim();
    if url.is_empty() || dest.is_empty() {
        crate::println!("Usage: wget <http(s)-url> <local-file>");
        return;
    }
    if crate::net::ip::get_ip_address().is_none() {
        crate::println!("Network not configured. Run 'ifconfig 10.0.2.15' first.");
        return;
    }
    if !crate::shell::is_mounted() {
        crate::println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let (mut secure, mut host, mut port, mut path) = match parse_http_url(url) {
        Ok(v) => v,
        Err(e) => {
            crate::println!("wget: {}: '{}'", e, url);
            return;
        }
    };
    crate::shell::clear_interrupt();
    let start = now_ms();
    crate::println!("Resolving {}...", host);
    let mut ip = match resolve_host(&host) {
        Ok(ip) => ip,
        Err(e) => {
            crate::println!("DNS failed for '{}': {}", host, e);
            return;
        }
    };
    crate::println!(
        "Downloading {}://{}:{}{} ...",
        if secure { "https" } else { "http" },
        host,
        port,
        path
    );
    let mut body: Option<Vec<u8>> = None;
    for hop in 0..=MAX_REDIRECTS {
        if interrupted() {
            crate::println!("wget cancelled.");
            crate::shell::clear_interrupt();
            return;
        }
        let elapsed = now_ms().saturating_sub(start);
        if elapsed >= timeout_ms {
            crate::println!("wget failed: operation timed out");
            return;
        }
        match fetch_once(
            secure,
            &host,
            port,
            &path,
            ip,
            timeout_ms.saturating_sub(elapsed),
        ) {
            Ok(FetchResult::Body(data)) => {
                body = Some(data);
                break;
            }
            Ok(FetchResult::Redirect(loc)) => {
                if hop >= MAX_REDIRECTS {
                    crate::println!("wget failed: Too many redirects");
                    return;
                }
                match resolve_location(secure, &host, port, &path, &loc) {
                    Ok((tls, h, p, pa)) => {
                        secure = tls;
                        host = h;
                        port = p;
                        path = pa;
                        crate::println!(
                            "Redirect {} -> {}://{}:{}{} ...",
                            hop + 1,
                            if secure { "https" } else { "http" },
                            host,
                            port,
                            path
                        );
                        crate::println!("Resolving {}...", host);
                        ip = match resolve_host(&host) {
                            Ok(ip) => ip,
                            Err(e) => {
                                crate::println!("DNS failed for '{}': {}", host, e);
                                return;
                            }
                        };
                        continue;
                    }
                    Err(e) => {
                        crate::println!("wget failed: {}", e);
                        return;
                    }
                }
            }
            Err(e) => {
                if interrupted() {
                    crate::println!("wget cancelled.");
                    crate::shell::clear_interrupt();
                } else {
                    crate::println!("wget failed: {}", e);
                }
                return;
            }
        }
    }
    let body = match body {
        Some(b) => b,
        None => {
            crate::println!("wget failed: Too many redirects");
            return;
        }
    };
    if interrupted() {
        crate::println!("wget cancelled.");
        crate::shell::clear_interrupt();
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    match crate::shell::write_file_contents(dest, &body, &mut device) {
        Ok(()) => crate::println!("Saved {} bytes to '{}'", body.len(), dest),
        Err(e) => crate::println!("Failed to save '{}': {}", dest, e),
    }
    if interrupted() {
        crate::shell::clear_interrupt();
    }
}
