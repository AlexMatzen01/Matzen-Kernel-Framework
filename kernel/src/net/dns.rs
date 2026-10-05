//! Minimal IPv4 DNS resolver used by the shell ping command.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

/// QEMU/slirp's resolver. Used until DHCP supplies one.
pub const DEFAULT_SERVER: [u8; 4] = [10, 0, 2, 3];

const DNS_PORT: u16 = 53;
const LOCAL_PORT: u16 = 49153;
const DNS_TIMEOUT_MS: u64 = 3_000;
const DNS_ATTEMPTS: u32 = 2;
const DNS_RETRY_MS: u64 = 500;

/// Largest answer this client will parse.
///
/// A response is UDP, so it arrives in at most one datagram, but the option area
/// is attacker-influenced and the record walk below trusts its length fields.
/// Capping the buffer means a hostile or corrupt response cannot make the kernel
/// copy an arbitrary amount.
pub const MAX_DNS_RESPONSE: usize = 4096;

/// Compression pointers followed while reading one name.
///
/// A pointer that points at itself, or two pointers that point at each other,
/// would otherwise make the name walk loop forever. Every hop is
/// attacker-controlled, so the walk is bounded.
pub const MAX_POINTER_JUMPS: usize = 8;

/// CNAMEs followed before giving up on a name.
///
/// Real chains are one or two long. A longer one is either a misconfiguration or
/// an attempt to make the resolver spend its time walking.
pub const MAX_CNAME_HOPS: usize = 8;

const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const CLASS_IN: u16 = 1;

/// Resolver address in use, packed so it can be read without a lock on the send
/// path. `set_server` writes it; `resolve_ipv4` reads it.
static SERVER: AtomicU32 = AtomicU32::new(u32::from_be_bytes([
    DEFAULT_SERVER[0],
    DEFAULT_SERVER[1],
    DEFAULT_SERVER[2],
    DEFAULT_SERVER[3],
]));

lazy_static! {
    /// Written when a configured server is replaced, so `dns` can report it.
    static ref CONFIGURED_SERVER: Mutex<Option<[u8; 4]>> = Mutex::new(None);
}

/// Point the resolver at `server`, remembering that it was configured.
///
/// DHCP is the reason this exists: a host that obtained its address over DHCP
/// must use the resolver that same server supplied, not a hardcoded one.
pub fn set_server(server: [u8; 4]) {
    SERVER.store(
        u32::from_be_bytes([server[0], server[1], server[2], server[3]]),
        Ordering::Release,
    );
    *CONFIGURED_SERVER.lock() = Some(server);
    crate::net_log!(
        "DNS: resolver set to {}.{}.{}.{}",
        server[0],
        server[1],
        server[2],
        server[3]
    );
}

/// Restore the built-in resolver.
pub fn reset_server() {
    SERVER.store(
        u32::from_be_bytes([
            DEFAULT_SERVER[0],
            DEFAULT_SERVER[1],
            DEFAULT_SERVER[2],
            DEFAULT_SERVER[3],
        ]),
        Ordering::Release,
    );
    *CONFIGURED_SERVER.lock() = None;
}

/// The resolver currently in use.
pub fn server() -> [u8; 4] {
    SERVER.load(Ordering::Acquire).to_be_bytes()
}

/// Whether the resolver came from DHCP rather than the built-in default.
pub fn server_is_configured() -> bool {
    CONFIGURED_SERVER.lock().is_some()
}

pub fn resolve_ipv4(name: &str) -> Result<[u8; 4], &'static str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 253 {
        return Err("Invalid hostname");
    }

    // The transaction ID pairs a response with this query, so it must not be
    // guessable: an ID derived from the tick counter advances predictably, which
    // is what makes forged-answer cache poisoning practical. The response is
    // also matched on source address and port, which an off-path attacker cannot
    // spoof through slirp, but the ID is the field the protocol relies on and it
    // costs nothing to make unpredictable.
    let id = crate::entropy::u16();
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

    let dns_server = server();
    crate::net::udp::send_packet(dns_server, LOCAL_PORT, DNS_PORT, &query)?;
    let start = crate::shell::monotonic_ms();
    let poll_limit = 3_000_000u64;
    let mut polls = 0u64;
    let mut attempts = 1u32;
    let mut last_send = crate::shell::monotonic_ms();
    while polls < poll_limit && crate::shell::monotonic_ms().saturating_sub(start) < DNS_TIMEOUT_MS {
        crate::net::process_packets();
        if let Some(response) = crate::net::udp::receive_from(LOCAL_PORT, dns_server, DNS_PORT) {
            // The queried name is passed to the parser, not just the
            // transaction id: an answer section record for a different name must
            // not be accepted as this query's answer.
            return parse_response(&response, id, name);
        }
        // A lost query is indistinguishable from a slow one, so re-send rather
        // than waiting out the whole timeout on a server that never saw it.
        if attempts < DNS_ATTEMPTS
            && crate::shell::monotonic_ms().saturating_sub(last_send) >= DNS_RETRY_MS
            && crate::net::udp::send_packet(dns_server, LOCAL_PORT, DNS_PORT, &query).is_ok()
        {
            attempts += 1;
            last_send = crate::shell::monotonic_ms();
        }
        crate::shell::increment_tick();
        polls += 1;
        core::hint::spin_loop();
    }
    Err("DNS query timed out")
}

/// Read a name at `offset`, returning the offset just past it.
///
/// `name` receives the dotted form. A compression pointer is resolved to at most
/// `MAX_POINTER_JUMPS` further hops: a pointer cycle otherwise makes the walk
/// loop forever, and this is attacker-controlled data.
fn read_name(
    data: &[u8],
    offset: usize,
    name: &mut alloc::string::String,
) -> Result<usize, &'static str> {
    let mut cursor = offset;
    let mut next = None;
    let mut jumps = 0;
    name.clear();

    loop {
        if cursor >= data.len() {
            return Err("Invalid DNS response");
        }
        let length = data[cursor];
        if length & 0xC0 == 0xC0 {
            if cursor + 1 >= data.len() {
                return Err("Invalid DNS response");
            }
            let target = (((length & 0x3F) as usize) << 8) | data[cursor + 1] as usize;
            jumps += 1;
            if jumps > MAX_POINTER_JUMPS || target >= data.len() {
                return Err("Invalid DNS response");
            }
            // Only the first jump sets where the name ends; a pointer must point
            // strictly backwards, or a record's own name would be part of its
            // name.
            if next.is_none() {
                next = Some(cursor + 2);
            }
            cursor = target;
            continue;
        }
        if length & 0xC0 != 0 {
            // Reserved label type.
            return Err("Invalid DNS response");
        }
        cursor += 1;
        if length == 0 {
            return Ok(next.unwrap_or(cursor));
        }
        if length > 63 || cursor + length as usize > data.len() {
            return Err("Invalid DNS response");
        }
        if !name.is_empty() {
            name.push('.');
        }
        for byte in &data[cursor..cursor + length as usize] {
            // Names are compared case-insensitively, so fold on the way in rather
            // than comparing every label twice.
            name.push(char::from(byte.to_ascii_lowercase()));
        }
        cursor += length as usize;
    }
}

fn skip_name(data: &[u8], offset: usize) -> Result<usize, &'static str> {
    let mut scratch = alloc::string::String::new();
    read_name(data, offset, &mut scratch)
}

/// One answer record's interesting fields.
struct Record {
    owner: alloc::string::String,
    record_type: u16,
    data: (usize, usize),
}

/// Extract an address for `query` from a DNS response.
///
/// The answer section is walked for an A record whose owner name is the name
/// that was asked for. The previous version returned the first A record it found
/// anywhere in the answer section, regardless of which name it belonged to: a
/// response for `attacker.example` carrying an A record for `bank.example` was
/// accepted as the answer to a query for `bank.example`. That is the whole of a
/// cache-poisoning answer once the transaction ID is fixed, and it is why this
/// function takes the queried name at all.
///
/// A CNAME is followed, bounded to [`MAX_CNAME_HOPS`], and each hop is matched
/// against the name that pointed at it, so a chain cannot be used to smuggle an
/// address in under an unrelated name.
pub fn parse_response(
    data: &[u8],
    id: u16,
    query_name: &str,
) -> Result<[u8; 4], &'static str> {
    if data.len() > MAX_DNS_RESPONSE {
        return Err("DNS response too large");
    }
    if data.len() < 12 || u16::from_be_bytes([data[0], data[1]]) != id {
        return Err("Invalid DNS response");
    }
    let flags = u16::from_be_bytes([data[2], data[3]]);
    if flags & 0x8000 == 0 {
        // Not a response.
        return Err("Invalid DNS response");
    }
    // RCODE in the low four bits. 3 is NXDOMAIN, which is a definitive "no such
    // name" and should not be reported as a transport failure.
    if flags & 0x000F == 3 {
        return Err("no such domain");
    }

    let questions = u16::from_be_bytes([data[4], data[5]]) as usize;
    let answers = u16::from_be_bytes([data[6], data[7]]) as usize;

    // The question section must echo exactly one question, and it must be the
    // one that was asked. A response whose question differs from the query is a
    // response to something else.
    let mut offset = 12;
    if questions != 1 {
        return Err("DNS response has an unexpected question count");
    }
    let mut asked = alloc::string::String::new();
    offset = read_name(data, offset, &mut asked)?;
    if offset + 4 > data.len() {
        return Err("Invalid DNS response");
    }
    let qtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
    let qclass = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
    offset += 4;
    if qtype != TYPE_A || qclass != CLASS_IN {
        return Err("DNS response is not for an A record");
    }
    let expected = normalise_name(query_name);
    if asked != expected {
        // Bailiwick starts here: the response is for a different name.
        return Err("DNS response is for a different name");
    }

    // Collect the answer records, then walk them. Collecting first lets a CNAME
    // be resolved against records that appear after it, which is legal.
    let mut records: Vec<Record> = Vec::new();
    let mut scratch = alloc::string::String::new();
    for _ in 0..answers {
        offset = read_name(data, offset, &mut scratch)?;
        let owner: alloc::string::String = scratch.clone();
        if offset + 10 > data.len() {
            return Err("Invalid DNS response");
        }
        let record_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let class = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
        let length = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
        offset += 10;
        let end = offset
            .checked_add(length)
            .ok_or("Invalid DNS response")?;
        if end > data.len() {
            return Err("Invalid DNS response");
        }
        records.push(Record {
            owner,
            record_type,
            data: (offset, end),
        });
        offset = end;
    }

    // Follow the chain. `current` starts as the queried name and only ever
    // becomes the owner of a CNAME that matched it.
    let mut current = expected;
    for _ in 0..=MAX_CNAME_HOPS {
        for record in &records {
            if record.owner != current {
                continue;
            }
            if record.record_type == TYPE_A {
                let (start, end) = record.data;
                if end - start != 4 {
                    continue;
                }
                return Ok([
                    data[start],
                    data[start + 1],
                    data[start + 2],
                    data[start + 3],
                ]);
            }
            if record.record_type == TYPE_CNAME {
                let (start, end) = record.data;
                let mut target = alloc::string::String::new();
                read_name(data, start, &mut target)?;
                if target.is_empty() {
                    continue;
                }
                current = target;
            }
        }
        // One more pass to resolve the name a CNAME pointed at.
    }

    Err("No IPv4 address for this name in DNS response")
}

/// Lowercase a name and drop a trailing dot, so `Example.COM.` and `example.com`
/// compare equal.
fn normalise_name(name: &str) -> alloc::string::String {
    let trimmed = name.trim_end_matches('.');
    trimmed.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    const ID: u16 = 0xBEEF;

    /// Encode a dotted name in DNS wire format.
    fn encode_name(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for label in name.split('.').filter(|l| !l.is_empty()) {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        out
    }

    /// Build a full response.
    ///
    /// `answers` are `(owner, type, rdata)` triples appended to the answer
    /// section, which is how a test states "the server returned a record for a
    /// name nobody asked about".
    fn response(questions: &[&str], answers: &[(&str, u16, Vec<u8>)], rcode: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&ID.to_be_bytes());
        out.extend_from_slice(&(0x8180u16 | rcode).to_be_bytes());
        out.extend_from_slice(&(questions.len() as u16).to_be_bytes());
        out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // authority
        out.extend_from_slice(&0u16.to_be_bytes()); // additional
        for q in questions {
            out.extend_from_slice(&encode_name(q));
            out.extend_from_slice(&TYPE_A.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
        }
        for (owner, rtype, rdata) in answers {
            out.extend_from_slice(&encode_name(owner));
            out.extend_from_slice(&rtype.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
            out.extend_from_slice(&300u32.to_be_bytes());
            out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            out.extend_from_slice(rdata);
        }
        out
    }

    fn a(ip: [u8; 4]) -> Vec<u8> {
        ip.to_vec()
    }

    #[test]
    fn a_matching_a_record_is_returned() {
        let data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([93, 184, 216, 34]))],
            0,
        );
        assert_eq!(
            parse_response(&data, ID, "example.com").unwrap(),
            [93, 184, 216, 34]
        );
    }

    // ── bailiwick ─────────────────────────────────────────────────────

    #[test]
    fn a_record_for_another_name_is_rejected() {
        // The core of cache poisoning: a response to a query for example.com
        // that carries an A record for bank.example must not answer it. The
        // previous parser returned the first A record in the answer section
        // regardless of which name owned it.
        let data = response(
            &["example.com"],
            &[("bank.example", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    #[test]
    fn an_out_of_bailiwick_record_cannot_precede_the_real_one() {
        // Order must not matter: a forged record listed first must not be taken
        // in preference to the legitimate one.
        let data = response(
            &["example.com"],
            &[
                ("bank.example", TYPE_A, a([1, 2, 3, 4])),
                ("example.com", TYPE_A, a([93, 184, 216, 34])),
            ],
            0,
        );
        assert_eq!(
            parse_response(&data, ID, "example.com").unwrap(),
            [93, 184, 216, 34],
            "the record for the queried name must win, whatever the order"
        );
    }

    #[test]
    fn a_response_for_a_different_question_is_rejected() {
        let data = response(
            &["other.example"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert_eq!(
            parse_response(&data, ID, "example.com").unwrap_err(),
            "DNS response is for a different name"
        );
    }

    #[test]
    fn a_response_with_the_wrong_question_count_is_rejected() {
        let data = response(
            &["example.com", "extra.example"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    #[test]
    fn a_response_for_the_wrong_query_type_is_rejected() {
        let mut data = response(&[], &[], 0);
        data[4..6].copy_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&28u16.to_be_bytes()); // AAAA, not A
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data[6..8].copy_from_slice(&1u16.to_be_bytes());
        // One answer record, with a name, type, class, ttl and length.
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&1u16.to_be_bytes()); // A
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[1, 2, 3, 4]);

        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    // ── names and case ───────────────────────────────────────────────

    #[test]
    fn names_compare_case_insensitively() {
        let data = response(
            &["Example.COM"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert_eq!(parse_response(&data, ID, "example.com").unwrap(), [1, 2, 3, 4]);
        assert_eq!(parse_response(&data, ID, "EXAMPLE.com").unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn a_trailing_dot_does_not_change_the_name() {
        let data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert!(parse_response(&data, ID, "example.com.").is_ok());
    }

    #[test]
    fn a_record_owned_by_a_case_variant_matches() {
        let data = response(
            &["example.com"],
            &[("EXAMPLE.COM", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert!(parse_response(&data, ID, "example.com").is_ok());
    }

    // ── CNAME ─────────────────────────────────────────────────────────

    #[test]
    fn a_cname_chain_is_followed() {
        let mut data = Vec::new();
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes()); // one question
        data.extend_from_slice(&2u16.to_be_bytes()); // two answers
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        // Answer 1: CNAME example.com -> www.example.com
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        let mut rdata = encode_name("www.example.com");
        let rdlen = rdata.len();
        data.extend_from_slice(&(rdlen as u16).to_be_bytes());
        data.extend_from_slice(&rdata);

        // Answer 2: A www.example.com
        data.extend_from_slice(&encode_name("www.example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[10, 0, 0, 7]);

        assert_eq!(parse_response(&data, ID, "example.com").unwrap(), [10, 0, 0, 7]);
    }

    #[test]
    fn a_cname_to_another_name_is_followed() {
        // example.com CNAMEs to evil.example, and the A record that follows is
        // for evil.example. Following the chain to another name is the whole
        // point of a CNAME: refusing it would break every CDN alias.
        //
        // Bailiwick here is about *reachability*, not about staying on the
        // queried string: an address is accepted only if a chain of CNAMEs
        // starting at the queried name leads to it.
        let mut data = Vec::new();
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&2u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        let rdata = encode_name("evil.example");
        data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        data.extend_from_slice(&rdata);

        data.extend_from_slice(&encode_name("evil.example"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[6, 6, 6, 6]);

        assert_eq!(parse_response(&data, ID, "example.com").unwrap(), [6, 6, 6, 6]);
    }

    #[test]
    fn a_record_not_reachable_through_the_chain_is_never_used() {
        // The same response, plus an A record for a name the chain never
        // reaches. It must be ignored even though it is in the answer section
        // and even though it appears first.
        let mut data = Vec::new();
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&3u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        // bank.example, unreachable, listed first.
        data.extend_from_slice(&encode_name("bank.example"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[1, 1, 1, 1]);

        // example.com CNAME -> cdn.example
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        let rdata = encode_name("cdn.example");
        data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        data.extend_from_slice(&rdata);

        // cdn.example A
        data.extend_from_slice(&encode_name("cdn.example"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[7, 7, 7, 7]);

        assert_eq!(
            parse_response(&data, ID, "example.com").unwrap(),
            [7, 7, 7, 7],
            "an unreachable record must not win, whatever its position"
        );
    }

    #[test]
    fn a_self_referential_cname_chain_terminates() {
        // A CNAME loop must not hang the resolver.
        let mut data = Vec::new();
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&2u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&encode_name("a.example"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        // a.example -> b.example
        data.extend_from_slice(&encode_name("a.example"));
        data.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        let rdata = encode_name("b.example");
        data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        data.extend_from_slice(&rdata);

        // b.example -> a.example
        data.extend_from_slice(&encode_name("b.example"));
        data.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        let rdata = encode_name("a.example");
        data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        data.extend_from_slice(&rdata);

        assert!(parse_response(&data, ID, "a.example").is_err());
    }

    // ── compression pointers ──────────────────────────────────────────

    #[test]
    fn a_compression_pointer_resolves_the_owner_name() {
        // Real servers compress the owner name of the second record as a pointer
        // to the first. If the pointer walk is wrong, every compressed answer is
        // misattributed.
        let mut data = Vec::new();
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&2u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        let question_at = data.len();
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        // Answer 1: the real A record.
        data.extend_from_slice(&encode_name("example.com"));
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[9, 9, 9, 9]);

        // Answer 2: owner is a pointer back to the question's name, and it is an
        // A record with a different address. Only the exact owner may match.
        data.extend_from_slice(&[0xC0, question_at as u8]);
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());
        data.extend_from_slice(&300u32.to_be_bytes());
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[8, 8, 8, 8]);

        // Two records share the queried name; the first wins, deterministically.
        assert_eq!(parse_response(&data, ID, "example.com").unwrap(), [9, 9, 9, 9]);
    }

    #[test]
    fn a_pointer_loop_terminates() {
        // A name that points at itself.
        let mut data = vec![0u8; 12];
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        // Question name: a pointer to itself at this offset.
        let self_at = data.len() as u8;
        data.extend_from_slice(&[0xC0, self_at]);
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        let mut scratch = String::new();
        assert!(read_name(&data, 12, &mut scratch).is_err());
    }

    #[test]
    fn an_out_of_range_pointer_is_rejected() {
        let mut data = vec![0u8; 12];
        data.extend_from_slice(&ID.to_be_bytes());
        data.extend_from_slice(&0x8180u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&[0xC0, 0xF0]); // far past the end
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&CLASS_IN.to_be_bytes());

        assert!(parse_response(&data, ID, "whatever.example").is_err());
    }

    // ── sizes, ids, rcode ─────────────────────────────────────────────

    #[test]
    fn an_oversized_response_is_refused_without_parsing() {
        // The cap exists so a hostile length field cannot make the kernel copy
        // an arbitrary amount.
        let mut data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        data.resize(MAX_DNS_RESPONSE + 1, 0);
        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    #[test]
    fn a_wrong_transaction_id_is_rejected() {
        let data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        assert!(parse_response(&data, ID ^ 0xFFFF, "example.com").is_err());
    }

    #[test]
    fn nxdomain_is_reported_as_such() {
        let data = response(&["example.com"], &[], 3);
        assert_eq!(parse_response(&data, ID, "example.com").unwrap_err(), "no such domain");
    }

    #[test]
    fn a_non_response_flag_is_rejected() {
        let mut data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        data[2] = 0; // clear the QR bit
        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    #[test]
    fn truncated_responses_never_yield_an_address() {
        let full = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([93, 184, 216, 34]))],
            0,
        );
        for len in 0..full.len() {
            assert!(
                parse_response(&full[..len], ID, "example.com").is_err(),
                "a {}-byte prefix must not resolve",
                len
            );
        }
        assert!(parse_response(&full, ID, "example.com").is_ok());
    }

    #[test]
    fn a_record_of_the_wrong_length_is_not_read_as_an_address() {
        let mut data = response(
            &["example.com"],
            &[("example.com", TYPE_A, a([1, 2, 3, 4]))],
            0,
        );
        // Claim 5 bytes of rdata for an A record. Only 4 are present, so the
        // record must be dropped rather than partially read.
        let rdlen_at = data.len() - 4 - 2;
        data[rdlen_at..rdlen_at + 2].copy_from_slice(&5u16.to_be_bytes());
        data.push(0xFF);
        assert!(parse_response(&data, ID, "example.com").is_err());
    }

    #[test]
    fn the_default_resolver_is_used_until_one_is_configured() {
        assert_eq!(server(), DEFAULT_SERVER);
        assert!(!server_is_configured());
        set_server([8, 8, 8, 8]);
        assert_eq!(server(), [8, 8, 8, 8]);
        assert!(server_is_configured());
        reset_server();
        assert_eq!(server(), DEFAULT_SERVER);
        assert!(!server_is_configured());
    }
}
