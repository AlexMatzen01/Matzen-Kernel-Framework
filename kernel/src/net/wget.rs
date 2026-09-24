//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Minimal HTTP/HTTPS downloader (`wget`).
//!
//! Reuses the shared helpers in `net::http` (DNS, TCP, packet pump,
//! header parsing) — no second stack. Saves the response body
//! to SimplFS via `shell::write_file_contents`.
//!
//! Usage: `wget [-td=5s|--td=5s|--timeout=5s] <http-url> <local-file>`
//! URL format: `http(s)://<host>[:port][/path]` (default ports 80/443).
//! Up to 3 HTTP(S) redirects are followed.
//! Response bodies stream to a staged SimplFS file. The mounted disk's
//! available blocks, rather than a fixed application limit, bound downloads.

use super::http::{
    build_get, cleanup, connect_wait, find_headers_end, interrupted, is_chunked, now_ms,
    parse_content_length, parse_location, parse_status, pump, resolve_host, send_all,
    validate_host, CONNECT_TIMEOUT_MS, MAX_HEADER,
};
use alloc::string::String;
use alloc::vec::Vec;
use embedded_io::{Read, Write};

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
            .or_else(|| token.strip_prefix("--td="))
            .or_else(|| token.strip_prefix("--timeout="));
        if let Some(value) = value {
            timeout = parse_timeout(value)?;
            continue;
        }
        if token == "-td" || token == "--td" || token == "--timeout" {
            return Err("timeout must use =, e.g. --timeout=5s");
        }
        // Anything else starting with '-' is an unknown option. Reject it
        // instead of silently treating it as the URL/destination (that once
        // swallowed `--td=120s`, leaving the default 25 s timeout in place
        // and appending junk to the destination path).
        if token.starts_with('-') {
            return Err("unknown option (see 'wget --help')");
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
    Complete,
    Redirect(Vec<u8>),
}

#[derive(Clone, Copy)]
enum ChunkState {
    SizeLine,
    Data(usize),
    DataTerminator(u8),
    Trailers,
    Done,
}

struct ChunkedDecoder {
    state: ChunkState,
    line: Vec<u8>,
}

impl ChunkedDecoder {
    fn new() -> Self {
        Self {
            state: ChunkState::SizeLine,
            line: Vec::new(),
        }
    }

    fn feed<F>(&mut self, bytes: &[u8], sink: &mut F) -> Result<(usize, bool), &'static str>
    where
        F: FnMut(&[u8]) -> Result<(), &'static str>,
    {
        let mut cursor = 0;
        let mut output_bytes = 0usize;
        while cursor < bytes.len() {
            match self.state {
                ChunkState::SizeLine => {
                    let byte = bytes[cursor];
                    cursor += 1;
                    if byte == b'\n' {
                        if self.line.last() == Some(&b'\r') {
                            self.line.pop();
                        }
                        let line =
                            core::str::from_utf8(&self.line).map_err(|_| "Invalid chunk size")?;
                        let size = line.split(';').next().ok_or("Invalid chunk size")?.trim();
                        if size.is_empty() || size.len() > 16 {
                            return Err("Invalid chunk size");
                        }
                        let size =
                            usize::from_str_radix(size, 16).map_err(|_| "Invalid chunk size")?;
                        self.line.clear();
                        self.state = if size == 0 {
                            ChunkState::Trailers
                        } else {
                            ChunkState::Data(size)
                        };
                    } else if self.line.len() < 128 {
                        self.line.push(byte);
                    } else {
                        return Err("HTTP chunk header too large");
                    }
                }
                ChunkState::Data(remaining) => {
                    let count = remaining.min(bytes.len() - cursor);
                    sink(&bytes[cursor..cursor + count])?;
                    cursor += count;
                    output_bytes = output_bytes.saturating_add(count);
                    self.state = if count == remaining {
                        ChunkState::DataTerminator(0)
                    } else {
                        ChunkState::Data(remaining - count)
                    };
                }
                ChunkState::DataTerminator(stage) => {
                    let expected = if stage == 0 { b'\r' } else { b'\n' };
                    if bytes[cursor] != expected {
                        return Err("Invalid HTTP chunk terminator");
                    }
                    cursor += 1;
                    self.state = if stage == 0 {
                        ChunkState::DataTerminator(1)
                    } else {
                        ChunkState::SizeLine
                    };
                }
                ChunkState::Trailers => {
                    let byte = bytes[cursor];
                    cursor += 1;
                    if byte == b'\n' {
                        if self.line.last() == Some(&b'\r') {
                            self.line.pop();
                        }
                        if self.line.is_empty() {
                            self.state = ChunkState::Done;
                        } else {
                            self.line.clear();
                        }
                    } else if self.line.len() < 4096 {
                        self.line.push(byte);
                    } else {
                        return Err("HTTP trailers too large");
                    }
                }
                ChunkState::Done => break,
            }
        }
        Ok((output_bytes, matches!(self.state, ChunkState::Done)))
    }

    fn is_done(&self) -> bool {
        matches!(self.state, ChunkState::Done)
    }
}

fn stream_body<F>(
    bytes: &[u8],
    chunked: bool,
    decoder: &mut Option<ChunkedDecoder>,
    content_length: Option<u64>,
    body_written: &mut u64,
    sink: &mut F,
) -> Result<(), &'static str>
where
    F: FnMut(&[u8]) -> Result<(), &'static str>,
{
    if chunked {
        let decoder = decoder.get_or_insert_with(ChunkedDecoder::new);
        let (written, _) = decoder.feed(bytes, sink)?;
        *body_written = body_written.saturating_add(written as u64);
    } else {
        let available = content_length
            .map(|length| length.saturating_sub(*body_written).min(usize::MAX as u64) as usize)
            .unwrap_or(bytes.len());
        let count = bytes.len().min(available);
        if count > 0 {
            sink(&bytes[..count])?;
            *body_written = body_written.saturating_add(count as u64);
        }
    }
    Ok(())
}

fn discard_staging_file(
    staging: &mut Option<String>,
    device: &mut dyn crate::drivers::block::BlockDevice,
) {
    if let Some(path) = staging.take() {
        let _ = crate::shell::remove_file_contents(&path, device);
    }
}

fn is_redirect(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

fn fetch_once<F>(
    secure: bool,
    host: &str,
    port: u16,
    path: &str,
    ip: [u8; 4],
    timeout_ms: u64,
    sink: &mut F,
) -> Result<FetchResult, &'static str>
where
    F: FnMut(&[u8]) -> Result<(), &'static str>,
{
    if secure {
        return fetch_https_once(host, port, path, ip, timeout_ms, sink);
    }
    let local = connect_wait(ip, port, core::cmp::min(CONNECT_TIMEOUT_MS, timeout_ms))?;
    let req = build_get(host, path);
    let start = now_ms();
    let res: Result<FetchResult, &'static str> = (|| {
        send_all(local, &req)?;
        let mut headers: Vec<u8> = Vec::new();
        let mut header_len: Option<usize> = None;
        let mut content_len: Option<u64> = None;
        let mut chunked = false;
        let mut chunk_decoder: Option<ChunkedDecoder> = None;
        let mut body_written = 0u64;
        let mut last_data = now_ms();
        loop {
            pump();
            if let Some(chunk) = crate::net::tcp::read_data(local) {
                last_data = now_ms();
                if header_len.is_some() {
                    stream_body(
                        &chunk,
                        chunked,
                        &mut chunk_decoder,
                        content_len,
                        &mut body_written,
                        sink,
                    )?;
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
                        chunked = is_chunked(&headers[..end]);
                        content_len = if chunked {
                            None
                        } else {
                            parse_content_length(&headers[..end])
                        };
                        let rest = &headers[end..];
                        stream_body(
                            rest,
                            chunked,
                            &mut chunk_decoder,
                            content_len,
                            &mut body_written,
                            sink,
                        )?;
                        header_len = Some(end);
                        headers.truncate(end);
                    }
                }
                if chunked
                    && chunk_decoder
                        .as_ref()
                        .map(ChunkedDecoder::is_done)
                        .unwrap_or(false)
                    || content_len.map(|cl| body_written >= cl).unwrap_or(false)
                {
                    break;
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
                if chunked
                    && !chunk_decoder
                        .as_ref()
                        .map(ChunkedDecoder::is_done)
                        .unwrap_or(false)
                    || (!chunked && content_len.map(|cl| body_written < cl).unwrap_or(true))
                {
                    return Err("Download incomplete (timeout)");
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
                        if chunked
                            && !chunk_decoder
                                .as_ref()
                                .map(ChunkedDecoder::is_done)
                                .unwrap_or(false)
                            || content_len.map(|cl| body_written < cl).unwrap_or(false)
                        {
                            return Err("Download incomplete (server closed)");
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        Ok(FetchResult::Complete)
    })();
    cleanup(local);
    res
}

fn fetch_https_once<F>(
    host: &str,
    port: u16,
    path: &str,
    ip: [u8; 4],
    timeout_ms: u64,
    sink: &mut F,
) -> Result<FetchResult, &'static str>
where
    F: FnMut(&[u8]) -> Result<(), &'static str>,
{
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
    let mut header_len = None;
    let mut content_len = None;
    let mut chunked = false;
    let mut chunk_decoder: Option<ChunkedDecoder> = None;
    let mut body_written = 0u64;
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
            stream_body(
                chunk,
                chunked,
                &mut chunk_decoder,
                content_len,
                &mut body_written,
                sink,
            )?;
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
                chunked = is_chunked(&headers[..end]);
                content_len = if chunked {
                    None
                } else {
                    parse_content_length(&headers[..end])
                };
                let rest = &headers[end..];
                stream_body(
                    rest,
                    chunked,
                    &mut chunk_decoder,
                    content_len,
                    &mut body_written,
                    sink,
                )?;
                header_len = Some(end);
                headers.truncate(end);
            }
        }
        if chunked
            && chunk_decoder
                .as_ref()
                .map(ChunkedDecoder::is_done)
                .unwrap_or(false)
            || content_len.map(|cl| body_written >= cl).unwrap_or(false)
        {
            break;
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
    if chunked
        && !chunk_decoder
            .as_ref()
            .map(ChunkedDecoder::is_done)
            .unwrap_or(false)
        || content_len.map(|cl| body_written < cl).unwrap_or(false)
    {
        return Err("Download incomplete");
    }
    Ok(FetchResult::Complete)
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
        crate::println!("Usage: wget [-d|--debug] [-td=5s|--td=5s|--timeout=5s] <http-url> <local-file>");
        crate::println!("  e.g. wget http://10.0.2.2:8000/hello.txt /docs/hello.txt");
        crate::println!("  HTTP works by default; HTTPS requires a net_tls-enabled build.");
        crate::println!("  Requires mounted FS ('mount').");
        crate::println!("  Follows up to 3 HTTP(S) redirects.");
        crate::println!("  Timeout: -td=5s, --td=5s or --timeout=5s (also ms and m; default 25s).");
        crate::println!("  Downloads stream to disk and are limited by free filesystem space.");
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
    let mut device = crate::shell::mounted_device();
    let mut staging: Option<String> = None;
    let mut downloaded = 0u64;
    let mut complete = false;
    // Create the staging file up front so an unwritable destination fails
    // fast (before any networking) with the drive in the message.
    match crate::shell::create_download_staging_file(dest, &mut device) {
        Ok(path) => staging = Some(path),
        Err(error) => {
            crate::println!(
                "wget: cannot write '{}' on drive {}: {}",
                dest,
                crate::shell::mounted_drive(),
                error
            );
            return;
        }
    }
    for hop in 0..=MAX_REDIRECTS {
        if interrupted() {
            discard_staging_file(&mut staging, &mut device);
            crate::println!("wget cancelled.");
            crate::shell::clear_interrupt();
            return;
        }
        let elapsed = now_ms().saturating_sub(start);
        if elapsed >= timeout_ms {
            discard_staging_file(&mut staging, &mut device);
            crate::println!("wget failed: operation timed out");
            return;
        }
        let response = {
            let mut sink = |bytes: &[u8]| {
                if staging.is_none() {
                    staging = Some(crate::shell::create_download_staging_file(
                        dest,
                        &mut device,
                    )?);
                }
                let path = staging.as_ref().ok_or("Staging file unavailable")?;
                crate::shell::append_file_contents(path, bytes, &mut device)?;
                downloaded = downloaded.saturating_add(bytes.len() as u64);
                Ok(())
            };
            fetch_once(
                secure,
                &host,
                port,
                &path,
                ip,
                timeout_ms.saturating_sub(elapsed),
                &mut sink,
            )
        };
        match response {
            Ok(FetchResult::Complete) => {
                complete = true;
                break;
            }
            Ok(FetchResult::Redirect(loc)) => {
                if hop >= MAX_REDIRECTS {
                    crate::println!("wget failed: Too many redirects");
                    discard_staging_file(&mut staging, &mut device);
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
                                discard_staging_file(&mut staging, &mut device);
                                return;
                            }
                        };
                        continue;
                    }
                    Err(e) => {
                        crate::println!("wget failed: {}", e);
                        discard_staging_file(&mut staging, &mut device);
                        return;
                    }
                }
            }
            Err(e) => {
                if interrupted() {
                    discard_staging_file(&mut staging, &mut device);
                    crate::println!("wget cancelled.");
                    crate::shell::clear_interrupt();
                } else {
                    discard_staging_file(&mut staging, &mut device);
                    crate::println!("wget failed: {}", e);
                }
                return;
            }
        }
    }
    if !complete {
        discard_staging_file(&mut staging, &mut device);
        crate::println!("wget failed: Too many redirects");
        return;
    }
    if interrupted() {
        discard_staging_file(&mut staging, &mut device);
        crate::println!("wget cancelled.");
        crate::shell::clear_interrupt();
        return;
    }
    if staging.is_none() {
        match crate::shell::create_download_staging_file(dest, &mut device) {
            Ok(path) => staging = Some(path),
            Err(error) => {
                crate::println!(
                    "Failed to create staging file for '{}' on drive {}: {}",
                    dest,
                    crate::shell::mounted_drive(),
                    error
                );
                return;
            }
        }
    }
    let staged_path = staging.take().unwrap();
    match crate::shell::promote_download_file(&staged_path, dest, &mut device) {
        Ok(()) => crate::println!("Saved {} bytes to '{}'", downloaded, dest),
        Err(error) => {
            let _ = crate::shell::remove_file_contents(&staged_path, &mut device);
            crate::println!("Failed to save '{}': {}", dest, error);
        }
    }
    if interrupted() {
        crate::shell::clear_interrupt();
    }
}

#[cfg(test)]
mod tests {
    use super::ChunkedDecoder;
    use super::{parse_wget_args, WGET_TIMEOUT_MS};
    use alloc::vec::Vec;

    #[test]
    fn chunked_decoder_handles_split_framing_and_trailers() {
        let wire = b"4;ext=x\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Test: ok\r\n\r\n";
        let mut decoder = ChunkedDecoder::new();
        let mut decoded = Vec::new();
        for byte in wire {
            let (written, _) = decoder
                .feed(core::slice::from_ref(byte), &mut |part| {
                    decoded.extend_from_slice(part);
                    Ok(())
                })
                .unwrap();
            assert!(written <= 1);
        }
        assert!(decoder.is_done());
        assert_eq!(decoded, b"Wikipedia");
    }

    #[test]
    fn wget_timeout_flags_are_parsed() {
        let (timeout, rest) = parse_wget_args("http://h/f /f").unwrap();
        assert_eq!(timeout, WGET_TIMEOUT_MS);
        assert_eq!(rest, "http://h/f /f");
        let (timeout, rest) = parse_wget_args("-td=120s http://h/f /f").unwrap();
        assert_eq!(timeout, 120_000);
        assert_eq!(rest, "http://h/f /f");
        let (timeout, rest) = parse_wget_args("http://h/f /f --td=2m").unwrap();
        assert_eq!(timeout, 120_000);
        assert_eq!(rest, "http://h/f /f");
        let (timeout, _) = parse_wget_args("--timeout=500ms http://h/f /f").unwrap();
        assert_eq!(timeout, 500);
    }

    #[test]
    fn wget_unknown_option_is_rejected_not_swallowed() {
        // `--td=` used to slip into the positional args: the timeout stayed
        // at its default and junk got appended to the destination path.
        assert!(parse_wget_args("http://h/f /f --td=120s").is_ok());
        assert_eq!(
            parse_wget_args("http://h/f /f --bogus=1"),
            Err("unknown option (see 'wget --help')")
        );
        assert_eq!(
            parse_wget_args("wget --td 5s").map(|_| ()),
            Err("timeout must use =, e.g. --timeout=5s")
        );
    }
}
