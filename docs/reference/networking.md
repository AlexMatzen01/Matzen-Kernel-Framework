# Networking Deep Dive

In-depth guide to MFK's network stack.

## Network Stack Overview

MFK implements a simplified TCP/IP stack:

```
┌─────────────────────────────────────┐
│  Application Layer (Shell Commands) │
│  - ping, ifconfig, netstat          │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│  ICMP Layer (Protocol)              │
│  - Echo request/reply (ping)        │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│  IP Layer (Routing, Addressing)     │
│  - Packet headers, checksums        │
│  - ARP (address resolution)         │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│  Ethernet Layer (Hardware)          │
│  - Frame formatting, MAC addresses  │
│  - E1000 driver                     │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│  Physical Layer (Hardware)          │
│  - Network cable to gateway         │
└─────────────────────────────────────┘
```

## Ethernet Frame Format

```
Bytes   Field               Purpose
─────────────────────────────────────────
6       Destination MAC     Target hardware address
6       Source MAC          Sender hardware address
2       EtherType           Protocol identifier
46-1500 Payload            Data (IP packet)
4       FCS                 Frame check sequence
─────────────────────────────────────────
64-1518 Total               Minimum 64, maximum 1518 bytes
```

**Example Ethernet II Frame:**

```
Destination MAC: 52:54:00:12:34:56
Source MAC:      08:00:27:00:00:00
EtherType:       0x0800 (IPv4)
IPv4 Packet:     [IP header + ICMP echo]
FCS:             [CRC checksum]
```

## IP Header Format

```
Offset  Bits    Field           Purpose
─────────────────────────────────────────────
0       0-3     Version         IPv4 = 4
        4-7     IHL             Header length / 4
1       0-7     DSCP            Quality of service
        8-15    ECN             Explicit congestion
2-3     0-15    Total Length    Header + payload bytes
4-5     0-15    Identification  For fragmentation
6       0-2     Flags           Don't fragment, More fragments
        3-15    Fragment Offset Byte offset
7       0-7     TTL             Time to live (hops)
8       0-7     Protocol        6=TCP, 17=UDP, 1=ICMP
9-10    0-15    Header Checksum Sum of header words
11-14   0-31    Source IP       Sender's IP address
15-18   0-31    Dest IP         Recipient's IP address
19+     Variable Options        Optional extensions
```

**MFK implementation:**

```rust
#[repr(C, packed)]
pub struct IpHeader {
    pub version_ihl: u8,           // Version (4) + IHL (5)
    pub dscp_ecn: u8,              // DSCP (0) + ECN (0)
    pub total_length: u16,         // Header + payload
    pub identification: u16,       // Fragmentation ID
    pub flags_fragment: u16,       // Flags + offset
    pub ttl: u8,                   // Time to live
    pub protocol: u8,              // 1=ICMP, 6=TCP, 17=UDP
    pub header_checksum: u16,      // Calculated checksum
    pub source_ip: u32,            // Source address
    pub dest_ip: u32,              // Destination address
}
```

## IP Address Handling

### Address Format

IP addresses are 32-bit values typically written in dotted decimal:

```
10.0.2.15 = 0x0F02000A (little-endian)
            = 0x0A000A0F (big-endian)
```

**MFK representation:**

```rust
pub struct IpAddress {
    pub octets: [u8; 4],
}

impl IpAddress {
    pub fn from_bytes(a: u8, b: u8, c: u8, d: u8) -> Self {
        IpAddress { octets: [a, b, c, d] }
    }
    
    pub fn as_u32(&self) -> u32 {
        u32::from_le_bytes(self.octets)
    }
}
```

### Routing

Simple routing logic:

```rust
pub fn route_packet(dest_ip: IpAddress) -> Result<RoutingDecision, &'static str> {
    if dest_ip == OUR_IP {
        // Local packet, not routed
        return Ok(RoutingDecision::Local);
    }
    
    if is_broadcast(dest_ip) {
        // Send to broadcast address
        return Ok(RoutingDecision::Broadcast);
    }
    
    if is_on_local_subnet(dest_ip) {
        // Same subnet, send directly
        return Ok(RoutingDecision::Direct);
    }
    
    if let Some(gateway) = DEFAULT_GATEWAY {
        // Different subnet, use gateway
        return Ok(RoutingDecision::Gateway(gateway));
    }
    
    Err("No route to destination")
}
```

**QEMU routing:**
```
Host network:     10.0.2.0/24
Gateway:          10.0.2.2
QEMU kernel:      10.0.2.15
Host itself:      10.0.2.1
```

## ARP (Address Resolution Protocol)

### Purpose

Maps IP addresses to Ethernet (MAC) addresses.

### Packet Format

```
Bytes   Field           Purpose
──────────────────────────────────────
0-1     Hardware Type   1 = Ethernet
2-3     Protocol Type   0x0800 = IPv4
4       HW Addr Len     6 (MAC)
5       Proto Addr Len  4 (IP)
6-7     Operation       1 = Request, 2 = Reply
8-13    Sender MAC      Hardware address of sender
14-17   Sender IP       IP address of sender
18-23   Target MAC      Hardware address of target (0 if request)
24-27   Target IP       IP address of target
```

### ARP Request/Reply Flow

**Scenario: Kernel needs to ping gateway 10.0.2.2**

```
Step 1: Kernel has dest IP, needs MAC
        ↓
Step 2: Check ARP cache for 10.0.2.2
        ↓
Step 3: Not cached, create ARP Request
        - "Who has 10.0.2.2? I am 10.0.2.15"
        ↓
Step 4: Send request as Ethernet broadcast
        ↓
Step 5: Gateway receives request
        ↓
Step 6: Gateway sends ARP Reply
        - "I am 10.0.2.2, I have MAC 52:54:00:12:34:56"
        ↓
Step 7: Kernel receives reply
        ↓
Step 8: Cache MAC for this IP
        ↓
Step 9: Now can send IP packet to gateway
```

**Implementation:**

```rust
pub fn resolve(ip: [u8; 4], timeout_ms: u64) -> Result<[u8; 6], &'static str> {
    if let Some(mac) = lookup(ip) {
        return Ok(mac);
    }
    send_arp_request(ip)?;
    // Re-send while waiting: a single request sent before the link is ready is
    // simply lost, and the caller then sees a timeout for a host that was there
    // all along.
    // ...
    Err("ARP resolution timed out")
}
```

### Cache bounds and expiry

ARP has no authentication: any frame that reaches the NIC can assert any
mapping. The cache is therefore treated as untrusted input, and every limit
below exists because the previous implementation lacked it.

| Bound | Value | Without it |
| --- | --- | --- |
| `ARP_MAX_ENTRIES` | 64 | A flood of frames with distinct sender addresses grew the map for the life of the boot, and `arp -a` then allocated a vector of all of them |
| `ARP_TTL_MS` | 120 s | A mapping was trusted forever, so a reassigned address kept sending traffic to the previous host |
| `ARP_STALE_GRACE_MS` | 30 s | Dropping an entry the instant it ages out breaks every connection whose peer is merely quiet |
| `ARP_REPLY_MIN_INTERVAL_MS` | 1000 ms | Every request was answered, so the host could be used as a reflector to amplify a flood at its own address |
| `ARP_RESOLVE_ATTEMPTS` | 3 | One request, one chance |

Entries are stored as `{mac, confirmed_ms, stale}`. Once past the TTL the entry
is marked stale: it stays usable for the grace window so existing connections
keep working, but every use is a signal to re-resolve, so a wrong mapping
corrects itself instead of silently blackholing traffic. A different MAC for a
known IP is treated as a re-learning rather than a refresh.

When the table is full, the entry confirmed longest ago is evicted. Age is the
best available proxy for usefulness: a recent entry was in active use, and there
is no traffic signal at eviction time. `netstat` separates eviction from expiry,
because one means the network is producing more distinct senders than the table
holds and the other is routine.

`ifconfig` flushes the cache when the address changes. The neighbours of the old
network are not the neighbours of the new one, and keeping the entries routes
the first packets of every new connection through the previous network's last
neighbour.

### Rejecting forged claims

An ARP frame carries its own idea of who sent it. If that disagrees with the
Ethernet source address, the frame was sent by one host while claiming to be
another, and its claim is worthless:

```rust
if sender_mac != src_mac {
    // Accepting this is the whole ARP spoofing attack: one frame redirects
    // every packet the kernel sends to that address, including the gateway.
    state.counters.spoof_rejected += 1;
    return;
}
```

A frame claiming our own address from a foreign MAC is also rejected — either a
duplicate-address mistake or an attempt to attract traffic meant for us.

The check is not redundant with the Ethernet layer: the NIC accepts a frame from
any source, and Ethernet itself has no notion of who should be on the wire.

## ICMP (Internet Control Message Protocol)

### Echo Request (Ping)

Used to test connectivity. Format:

```
Bytes   Field           Purpose
──────────────────────────────────
0       Type            8 = Echo Request
1       Code            0
2-3     Checksum        Calculated
4-5     Identifier      Matches request to reply
6-7     Sequence        Increments for each ping
8+      Data            Arbitrary payload
```

**Example: Ping 10.0.2.2 twice**

```
Kernel                          Gateway
│                               │
├─ IP: 10.0.2.15 → 10.0.2.2   │
│  ICMP Echo Request (seq=1)   │
├──────────────────────────────>│
│                               │
│                         Replies
│                               │
│  IP: 10.0.2.2 → 10.0.2.15   │
│  ICMP Echo Reply (seq=1)     │<────────┐
│<──────────────────────────────│         │
│                               │         │
├─ IP: 10.0.2.15 → 10.0.2.2   │    Received!
│  ICMP Echo Request (seq=2)   │
├──────────────────────────────>│
│                               │
│  IP: 10.0.2.2 → 10.0.2.15   │
│  ICMP Echo Reply (seq=2)     │
│<──────────────────────────────│
```

**MFK Implementation:**

```rust
pub fn send_ping(target_ip: IpAddress, count: u32) -> Result<(), &'static str> {
    for i in 0..count {
        // Create ICMP echo request
        let icmp_packet = IcmpEchoRequest {
            icmp_type: 8,              // Echo request
            code: 0,
            checksum: 0,               // Calculated later
            identifier: 1234,
            sequence: i as u16,
            data: b"ping",
        };
        
        // Calculate checksum
        let checksum = calculate_icmp_checksum(&icmp_packet);
        
        // Wrap in IP packet
        let ip_packet = create_ip_packet(
            OUR_IP,
            target_ip,
            1,  // ICMP protocol
            &icmp_packet,
        )?;
        
        // Send via Ethernet
        send_packet(&ip_packet)?;
        
        // Wait for reply (simple implementation)
        for _ in 0..100000 {
            if let Some(reply) = receive_icmp_reply(i as u16) {
                serial_println!("Reply from {}: seq={} time=?ms",
                    target_ip, reply.sequence);
            }
        }
    }
    Ok(())
}

fn receive_icmp_reply(sequence: u16) -> Option<IcmpEchoReply> {
    // Process received packets
    if let Some(packet) = receive_ethernet_frame() {
        if let Ok(ip_packet) = parse_ip_packet(&packet) {
            if ip_packet.protocol == 1 {  // ICMP
                if let Ok(icmp) = parse_icmp(&ip_packet.payload) {
                    if icmp.icmp_type == 0 && icmp.sequence == sequence {
                        // Found our reply!
                        return Some(icmp);
                    }
                }
            }
        }
    }
    None
}
```

## Packet Reception

### Receive Flow

Delivery is interrupt-announced and pump-executed:

```
1. Ethernet frame arrives
   ↓
2. E1000 DMA transfers to an RX descriptor's buffer
   ↓
3. Card raises the cause register and asserts its INTx line
   ↓
4. IRQ handler reads ICR, which clears the cause, and sets a pending flag
   ↓
5. Shell/pump calls process_packets(), which drains the ring
   ↓
6. Each frame is copied into the bounded RX queue
   ↓
7. Queued frames are dispatched to ARP, IP, TCP, etc.
```

Step 4 does not walk the ring. The handler touches no lock, allocates nothing,
and copies no packets: on a uniprocessor it can interrupt `receive_packet`
mid-update, and taking the driver lock there would deadlock against the very
code that raised the interrupt. Allocating in interrupt context would be worse
still. The handler therefore only acknowledges and flags; the pump does the work.

The MMIO base and the counters are plain atomics for the same reason: anything
the handler touches must be reachable without a lock.

When the card has no INTx line assigned, `interrupt_vector()` returns `None`,
`netstat` reports polling mode, and the pump drains the ring unconditionally as
before. Both modes read the same descriptors through the same code path, so the
polling fallback is not a second implementation.

### E1000 Reception

```rust
// Interrupt context: acknowledge and flag only.
pub fn handle_interrupt() {
    let base = MMIO_BASE.load(Ordering::Relaxed);
    let cause = unsafe { read_volatile((base + REG_ICR) as *const u32) };
    // A read of ICR clears the latched cause, letting the line deassert.
    if cause & IMS_RECEIVE != 0 {
        RX_PENDING.store(true, Ordering::Release);
    }
    unsafe { PICS.lock().notify_end_of_interrupt(PIC_VECTOR.load(Relaxed) as u8) }
}

// Pump context: copy the frame out and hand the descriptor back.
pub fn receive_packet(&mut self) -> Option<Vec<u8>> {
    // Compare our index against the card's RDTCH, then check the descriptor's
    // DD bit. A zero length or non-zero error bits drops the frame and still
    // returns the descriptor, so one bad frame cannot wedge the ring.
    // ...
}
```

`PciDevice::interrupt_pin_line` reads config offset 0x3C, where the firmware
records the interrupt pin and line. QEMU's legacy `pc` machine places the E1000
on IRQ11 (vector 43); other configurations use IRQ10 (vector 42). Both vectors
are wired to the driver and only the assigned line is unmasked, so the unused
one stays silent. A card landing on a line with no installed handler stays in
polling mode — unmasking its cause register with no vector behind it would leave
the PIC line asserted forever.

`netstat` reports the assigned vector, interrupt count, transmit completions,
transmit FIFO underflows, and the last raw cause value, so a driver that stops
delivering is distinguishable from one that was never enabled.

## Checksum Calculation

### IP Header Checksum

The header checksum is verified on receive. Every field in it — the protocol
number, both addresses, the total length — decides what the packet is and where
it goes, so a corrupt header is discarded before any of it is acted on rather
than after. `checksum_valid` accepts `0xFFFF` as well as `0`, because RFC 1071
says a computed zero is transmitted as all-ones and a peer that does not
normalise it is still correct.

Computing and verifying are deliberately separate functions:
`IpHeader::checksum_bytes` treats the field at offset 10..12 as zero and is used
to *produce* a checksum, while `verify_checksum_bytes` sums the stored field in and
is used to *check* one. Using one for both is a silent failure: the compute form
can never detect a corrupt field, because it excludes it from the sum.

```
1. Set checksum field to 0
2. Sum all 16-bit words in header
3. If overflow (carry), add back to sum
4. Take one's complement (invert all bits)
5. Result is checksum
```

**Implementation:**

```rust
pub fn calculate_ip_checksum(header: &IpHeader) -> u16 {
    let mut sum = 0u32;
    
    // Sum all 16-bit words in header (20 bytes)
    for i in 0..10 {
        let word = u16::from_be_bytes([
            header_bytes[i * 2],
            header_bytes[i * 2 + 1],
        ]);
        sum += word as u32;
    }
    
    // Handle overflow
    while (sum >> 16) > 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    
    // One's complement
    !sum as u16
}
```

### ICMP Checksum

Same algorithm as IP, but applied to ICMP packet.

## Network Configuration

### Setting IP Address

```rust
pub fn set_ip_address(octets: [u8; 4]) {
    OUR_IP = IpAddress { octets };
}
```

Called at boot:
```rust
// kernel/src/main.rs
net::ip::set_ip_address([10, 0, 2, 15]);
```

### Getting IP Address

```rust
pub fn get_ip_address() -> IpAddress {
    OUR_IP
}
```

### Getting MAC Address

Read from E1000:
```rust
pub fn get_mac_address() -> [u8; 6] {
    OUR_MAC
}
```

## Fragmentation

`MTU` is 1500, the Ethernet payload size the link carries. A datagram larger
than `MAX_PAYLOAD` (1480) is split by `send_packet_with_flags` and reassembled by
`ip::process_packet` before any upper layer sees it.

This was absent. An oversize datagram went to the NIC, which refuses anything past
its 2048-byte buffer, so a large UDP payload failed with "Packet too large" — an
error from the driver about a datagram IP fragmentation is defined to carry. TCP
never noticed, because it segments to 1400 bytes; only UDP was affected, and
silently.

### Sending

- A non-final fragment is rounded *down* to a multiple of 8, since the offset
  field counts 8-byte units. The final fragment carries the remainder, which is
  routinely not a multiple of 8.
- All pieces of one datagram share an identification value, drawn from the same
  entropy source as a TCP ISN.
- With `FLAG_DONT_FRAGMENT` set, an oversize datagram is refused rather than
  split. Silently fragmenting would violate the request; silently truncating
  would corrupt the datagram. The peer learns the real limit from the ICMP
  Fragmentation Needed message below.

### Reassembly

The table is keyed on source address, destination address, protocol and
identification. Including the source is what stops a fragment from another sender
carrying the same id from splicing bytes into someone else's datagram.

Bounded on three axes:

| Bound | Value | Without it |
| --- | --- | --- |
| `MAX_REASSEMBLY` | 8 sets | A peer opening fragment sets and never completing them grows the table for the life of the boot |
| `MAX_REASSEMBLY_BYTES` | 65535 | A fragment claiming to extend past the protocol maximum is refused rather than allocated |
| `REASSEMBLY_TIMEOUT_MS` | 5 s | A datagram missing one piece holds its buffer until the sweep |

Overlapping fragments are legal in RFC 791 and a retransmission is the common
case, so bytes that agree are accepted. A **disagreeing** overlap discards the
entire set: that is the teardrop-style attack, and resolving it by picking one
side lets a peer put a second payload through the same reassembler.

Completion requires the final fragment *and* no gap below its end, tracked by a
per-byte `received` map rather than a single length, so an out-of-order arrival
completes correctly and a missing middle piece never does.

Note the field layout, since it is easy to get wrong: RFC 791 draws the 16-bit
word as three flag bits (15 reserved, 14 DF, 13 MF) followed by thirteen offset
bits (12..0). The offset is in the **low** bits and is multiplied by 8 to get
bytes; it is not shifted.

## ICMP errors

Destination Unreachable, Time Exceeded and Parameter Problem were previously
discarded, so every ICMP error was invisible: a send that failed because a router
dropped the datagram looked exactly like one that failed because nothing
answered. `netstat` now lists the last few with the destination they were about.

The quoted datagram carried inside an error is decoded for its protocol and
destination — that is what tells the user what was actually being attempted — but
it is treated as a hint, not verified: the quoted header has not had its checksum
checked by us.

### Echo amplification

An echo request is only reflected when its payload is at most
`MAX_ECHO_PAYLOAD` (64 bytes). Echoing whatever arrived is what turns a host into
a traffic amplifier at line rate, pointed at a third party by whoever chose the
source address. Larger requests are counted and dropped rather than answered.

## Performance Considerations

### Packet Processing Latency

Current implementation:
1. Interrupt occurs → Handler queues packet
2. Shell calls process_packets() periodically
3. Packet is processed

**Latency:** ~10-50ms depending on command processing

### Memory Efficiency

```rust
// Each RX buffer: 2KB
// Count: 32
// Total: 64KB

// Each TX buffer: 2KB
// Count: 8
// Total: 16KB

// ARP cache: bounded at 64 entries
// Each: {mac: 6, confirmed_ms: 8, stale: 1} + map overhead
// Worst case: a few KiB, independent of what the network sends

// UDP receive queue: bounded at 32 datagrams AND 256 KiB of payload.
// The byte bound is the one that matters: 32 datagrams of up to 64 KiB is
// 2 MiB of heap chosen entirely by whoever sends them.

// Per TCP connection:
//   receive buffer   up to 1 MiB   (advertised window is derived from this)
//   send buffer     up to 256 KiB  (writes waiting for window space)
//   in-flight queue up to 4 segments x 1400 bytes, payload retained for
//                    retransmission
//   table entry     1 of at most 32 connections
// TCP receive buffers are allocated per connection only as data arrives.

// Fixed per-interrupt state: MMIO base and counters are atomics, not locked
// structs, because the interrupt handler cannot block on a lock the pump holds.
```

Every bound above is chosen so a peer decides how long a transfer takes, never
how much memory the kernel uses. `netstat` reports the queue and cache counters
so a network under pressure is distinguishable from a quiet one.

### Throughput

Limited by:
- Polling-based packet processing (not interrupt-driven)
- Shell command loop latency
- No packet pipelining

**Typical:** ~100-500 packets/second (much lower than hardware capable of)

## Debugging Network Issues

### Check Configuration

```bash
mfk> ifconfig
# Should show:
# - MAC address from E1000
# - IP address 10.0.2.15
# - Netmask 255.255.255.0
```

### Check Status

```bash
mfk> netstat
# Should show:
# - Ethernet: Active
# - IP: Active
```

### Test Connectivity

```bash
mfk> ping 10.0.2.2 1
# Should show received reply
```

### Debug Output

Enable serial output to see network operations:
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | grep -i "network\|arp\|icmp"
```

## TCP

`kernel/src/net/tcp.rs` implements a client-side TCP. It is a transport layer: it
is identified by a local port, and callers reach it through `kernel/src/net/socket.rs`
(see [Socket layer](#socket-layer)) or through `http`, `wget`, `speedtest` and
`tls` as before.

### Connection identity

The initial sequence number and the ephemeral port both come from
`kernel/src/entropy.rs` (RDRAND, then RDSEED, then a seeded mixer). Both used to
be fixed: ISN `1000`, and ports counting up from 49152. Every boot therefore
produced byte-identical connection tuples, which is a starting point for
injecting traffic into someone else's connection. The entropy module sits outside
the `net_tls` feature because TCP needs it whether or not TLS is compiled.

### Buffers and windows

| Bound | Value | Why |
| --- | --- | --- |
| `MAX_RECV_BUFFER` | 1 MiB | A remote peer chooses how much kernel memory to occupy without this |
| `MAX_SEND_BUFFER` | 256 KiB | Queued writes waiting for window space |
| `MAX_CONNECTIONS` | 32 | The table was previously unbounded |
| `MAX_SEGMENT_PAYLOAD` | 1400 | Under the E1000's 2048-byte TX buffer |
| `MAX_UNACKED_SEGMENTS` | 4 | Bounds in-flight data per connection |
| `IDLE_TIMEOUT_MS` | 120 s | Reclaims connections nobody closed |

The peer's advertised window is read and respected; it used to be decoded
nowhere, so a peer that narrowed its window had no effect on what was sent. Our
own advertised window is derived from real receive-buffer occupancy, so a full
buffer is advertised as zero rather than pretending to have room.

### Retransmission

Unacknowledged segments are retained with their payload and re-sent with
exponential backoff (200 ms doubling to 8 s), giving up after
`MAX_TX_ATTEMPTS` (6). `send_data` used to hand bytes to the NIC and advance the
sequence number unconditionally: one lost packet ended the transfer with no
recovery, and a caller got no indication. `tcp::tick()` runs from
`net::process_packets` and drives retransmission, window reopening, TimeWait
expiry and idle reclamation.

Retaining the payload per segment is what makes retransmission possible at all.
A design that kept only the length could not resend anything.

### Sequence validation

Segments outside the receive window are discarded and re-acknowledged rather
than appended. A duplicate would otherwise appear twice in the body of every
download that lost one segment. A segment past the gap is dropped so the peer's
retransmission fills it; there is no reassembly queue. An ACK beyond everything
ever sent is treated as a protocol error and reset, since trusting it would
silently discard queued data.

Sequence comparisons use wraparound-safe arithmetic (`seq_lt` and friends);
a plain `<` reports `u32::MAX < 0` as false, which breaks every ACK check once a
connection passes the wrap point.

### Teardown

Every reachable state is driven: FinWait1, FinWait2, Closing (simultaneous
close), LastAck, and TimeWait. LastAck was previously a dead end — nothing ever
completed it — and TimeWait holds the port for 60 s so a late segment from the
old connection cannot be read as a new one.

### Deliberately not implemented

Congestion control, a listen/server role, urgent pointers, SACK, and out-of-order
reassembly. Anything relying on congestion control will overrun a peer that is
already backed up.

### Observability

`netstat` reports, per connection: active and total connections, segments sent,
retransmitted, and received, out-of-window drops, resets, timeouts, bytes
buffered and in flight, plus a per-connection table with state names. That is
what distinguishes "the link dropped a segment" from "the peer never
acknowledged" from "we dropped it".

## Socket layer

`kernel/src/net/socket.rs` sits above TCP and UDP and gives them one addressing
model and one lifetime model. Before it there were three incompatible ways to
name an open connection:

- **TCP** was a bare `u16` local port passed to a dozen free functions. Nothing
  distinguished a stale port from a live one, so a caller that reused a port
  number after closing addressed whatever connection inherited it — or nothing,
  which looked identical to a silent peer.
- **UDP** had no identity at all: the caller picked a port at receive time and
  every reader scanned one shared queue.
- `tls::TcpSocket` was the only owned-connection type, and it compiles
  only when the `net_tls` feature is enabled (on by default; `--no-tls`
  selects the stub instead).

### Handles are index + generation

A `Socket` is a copyable pair — a table index and a generation:

```text
socket#0g1   open
socket#0g2   a different socket, in the same slot
```

The generation is the whole point. Without it, reusing slot 0 after a close hands
the next socket a handle value an old one still carries, and a double-close or a
use-after-close turns into traffic sent to the wrong peer — the class of bug that
appears once a day and gets blamed on the network. `resolve` refuses a handle
whose generation no longer matches, or whose slot has been closed.

`Socket` is `Copy` on purpose. This kernel has no threads and no blocking
scheduler to enforce scoping against, so `Drop` cannot promise a socket is
closed; `Socket::close` does the work and releases the slot, and a copyable
value can be printed by the user and typed back without a borrow checker in the
way. Copies are safe because a handle carries no buffers.

`reap` **marks** dead entries closed rather than removing them. Removing one
would restart the generation counter for that slot and let a retired handle
become live again — the same bug in a subtler place. Keeping closed entries costs
nothing: `insert` prefers a closed slot and the table is bounded at
`MAX_SOCKETS` (32).

### Surface

| Call | Behaviour |
| --- | --- |
| `connect_tcp(peer, timeout)` | Connects and returns a handle once established, or no handle at all |
| `bind_udp(local_port, peer)` | A UDP socket; usable immediately, as there is no handshake |
| `read` / `read_timeout` | Reads buffered bytes; `read_timeout` waits for data |
| `write` / `write_all` | Sends; `write_all` loops on short writes |
| `state` / `peer_addr` / `local_port` | Status |
| `close` / `abort` / `detach` | Graceful FIN, reset, or release-only |
| `sockets()` | Open sockets, for `tcpsockets` |

A failed connect returns **no handle**. Returning one that names a half-open
connection is how a caller ends up writing into a socket that can never complete.

`state` consults the transport as well as its cached state, because the cache can
lag a peer that closed since the last pump, and a stale "connected" is how a
caller writes into a socket nobody is reading.

UDP reads are peer-filtered when a socket was bound to a peer.
`udp::receive` matches on the local port alone, which would otherwise let an
unrelated host inject a "reply" into a socket the caller believes is talking to
one specific server.

### Read semantics

A short TCP read drops the remainder of what `tcp::read_data` drained — that API
has no pushback — and logs it, rather than reporting a length the caller did not
receive. A caller needing the whole buffer should size it accordingly. UDP reads
preserve the datagram boundary.

### The pump moved out of `http`

`net::pump()` used to be `http::pump()`, and TCP called back into it while its
send buffer was full — so the dependency ran `tcp -> http -> tcp`. The scheduler
is a property of the stack, not of HTTP, so it now lives in `net/mod.rs` and
`http` re-exports it for its existing callers.

## DHCP

`kernel/src/net/dhcp.rs` implements enough of RFC 2131 to obtain a lease:
Discover, Offer, Request, Ack. Previously the kernel had no DHCP at all — every
boot required typing `ifconfig` — so address acquisition had never been
exercised.

### Sending before there is an address

The client cannot use `ip::send_packet`. That resolves the next hop over ARP and
consults the configured address to decide what the next hop is, and at this point
there is no address to configure and nothing cached to resolve with. The one
frame that must not depend on the ARP cache is the first frame.

So `send_broadcast` builds the IPv4 and UDP headers itself — source `0.0.0.0`,
destination `255.255.255.255`, ports 68 to 67, no UDP checksum, which RFC 768
permits — and hands the result to `ethernet::send_frame` with a broadcast
destination MAC. The IPv4 header checksum uses `IpHeader::checksum_bytes`, which
is shared with the rest of the stack so there is one implementation of it.

### What is refused

A lease is input from the network, so `lease_from_offer` checks it before
anything is configured:

| Refusal | Why it matters |
| --- | --- |
| `0.0.0.0`, `255.255.255.255`, multicast, loopback | None of these is a usable host address; configuring one leaves the host with no route |
| Network or broadcast address of the offered subnet | The mask the server sent defines these, not an assumed /24 |
| Missing or zero subnet mask | Guessing /24 either hides reachable hosts or sends everything to the gateway |
| Non-contiguous subnet mask | Never legitimate |
| Missing server identifier (option 54 *and* `siaddr`) | There is nobody to send the request to, and no lease could be renewed |
| Router or DNS of `0.0.0.0` | Treated as absent: stored as a gateway it black-holes every packet |
| Neither an Offer nor an Ack | Anything else on port 67 is not a server response |

The accepted address need not be one we already hold: a server re-offering the
current address is a renewal, and refusing it would break renewal.

An Ack is re-validated rather than assumed to confirm the offer. The server has
the final word on the lease, so an Ack naming a different address is checked
against the same rules instead of trusted.

Retransmission is bounded at four attempts per phase with exponential backoff
from 250 ms to 2 s, so a silent network cannot hang the caller. A NAK drops the
address: the server is reclaiming it, and continuing to use it would be wrong.

### Renewal

Renewal exposed a bug in the IP layer, not in DHCP. A server answers a client
that already holds a lease with the reply addressed to `255.255.255.255`, and
`ip::process_packet` discarded anything not addressed to our own address — so
every renewal after the first lease was silently dropped. `destination_is_accepted`
now accepts our address, the limited broadcast, and `0.0.0.0`, which is what RFC
1122 requires of a host and what the pre-lease exchange needs anyway.

### DHCP and DNS

The resolver is configurable rather than hardcoded. A host that obtained its
address over DHCP must use the resolver that server supplied, so `dhcp` calls
`dns::set_server` with option 6, falling back to the built-in default when the
offer has none.

## DNS

`kernel/src/net/dns.rs` is a minimal A-record resolver: it builds a query, sends
it over UDP, and walks the answer section.

### Bailiwick

Every security property here comes from the answer section being matched against
the name that was asked for. The previous parser returned the **first A record
found anywhere in the answer section**, regardless of which name owned it: a
response to a query for `example.com` carrying an A record for `bank.example`
was accepted as the answer. Combined with a predictable transaction ID, that is
a complete cache-poisoning answer.

So the parser takes the queried name and:

- requires the question section to echo exactly one question, for the name that
  was asked, of type A class IN;
- accepts an A record only when its **owner name** matches the name currently
  being resolved;
- compares names case-insensitively and tolerates a trailing dot, because a
  server is free to vary both;
- records the answer section before resolving, so a CNAME is matched against
  records that appear after it, which is legal.

Bailiwick is about *reachability*, not about staying on the queried string: a
CNAME to `cdn.other.net` is followed and that name's address is returned, since
refusing it would break every CDN alias. What is refused is an address for a name
no chain of CNAMEs from the queried name leads to. `MAX_CNAME_HOPS` (8) bounds
the walk, so a cycle terminates.

### Parsing bounds

Names may use compression pointers, which are attacker-controlled, so the walk is
bounded in three ways: `MAX_POINTER_JUMPS` (8) hops per name, a pointer must
resolve inside the message, and a pointer must point strictly backwards — a
forward pointer would make a record's own name part of its name. Reserved label
types are rejected, as are labels over 63 bytes.

`MAX_DNS_RESPONSE` (4 KiB) caps the buffer before parsing, so a hostile length
field cannot make the kernel copy an arbitrary amount. Every offset arithmetic
step is `checked_add`. A response whose rcode is NXDOMAIN reports that rather
than a transport failure, which is a different answer for the caller.

## Future Enhancements

### Congestion control

TCP currently sends as fast as the peer's window allows.

### UDP

Datagram transport for faster, unreliable communication.

### Routing Table

Support multiple routes and gateways.

### DHCP lease renewal timers

A lease is obtained but never renewed on a timer, so a long-lived host loses its
address when the lease expires. The retransmission and validation machinery is
already here; what is missing is scheduling the renewal.

### HTTPS

TLS 1.3 (`net_tls`) is enabled by default: `wget https://...` verifies
certificates through `embedded-tls`, which implements RFC 6125 SAN
matching (ISRG Root X1 baked in). Build with `./build.sh --no-tls`
(or `--no-default-features --features usb` for direct cargo invocations)
to omit it and fall back to the stub that reports "TLS is not enabled".

Each connection retries every embedded trust anchor in order (a failed
handshake consumes its socket, so each anchor gets a fresh connection;
verification happens before any byte is requested, hence retrying never
duplicates a download). Bundled roots (`kernel/src/net/tls.rs`,
`TRUSTED_ROOTS`):

- ISRG Root X1 — Let's Encrypt hosts (e.g. `raw.githubusercontent.com`).
- Sectigo Public Server Authentication Root E46 — e.g. `github.com`.

`tlsinfo` lists the anchors and the wall-clock state; `wget`/`speedtest`
refuse HTTPS early with `wall clock not set` when the RTC time is missing
instead of failing opaquely mid-handshake.

Known soft-float notes for the `x86_64-mfk` target: the kernel target
disables SSE2 and selects the `x86-softfloat` ABI, so every crypto crate
with an x86 SIMD backend is forced to its portable implementation —
`sha2` v0.11 via `--cfg sha2_backend="soft"`, the transitive `sha2` v0.10
via the `force-soft` cargo feature, `aes` 0.8 via `--cfg aes_force_soft`,
`polyval` 0.6 via `--cfg polyval_force_soft` (see `.cargo/config.toml`
and `kernel/Cargo.toml`). ghash delegates to polyval and ppv-lite86
already picks its generic backend on `-sse2` targets, so both are covered.
If a build still fails in LLVM with `Do not know how to split the result
of this operator`, rebuild with `--no-tls` and report which crate failed
so it can be addressed (bump or patch in a soft backend).

## Next Steps

- **[Drivers Reference](drivers.md)** — Hardware interface
- **[Interrupts Reference](interrupts.md)** — Interrupt handling
- **[Architecture Overview](architecture.md)** — System organization
