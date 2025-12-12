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
pub fn get_mac_for_ip(ip: IpAddress) -> Result<[u8; 6], &'static str> {
    // Check cache first
    if let Some(mac) = ARP_CACHE.lookup(ip) {
        return Ok(mac);
    }
    
    // Cache miss, send ARP request
    send_arp_request(ip)?;
    
    // Wait for reply by processing packets
    for _ in 0..100000 {
        crate::net::process_packets();
        
        // Check cache again
        if let Some(mac) = ARP_CACHE.lookup(ip) {
            return Ok(mac);
        }
    }
    
    Err("No ARP reply received")
}

fn send_arp_request(target_ip: IpAddress) -> Result<(), &'static str> {
    let arp_request = ArpPacket {
        hw_type: 1,                          // Ethernet
        proto_type: 0x0800,                  // IPv4
        hw_addr_len: 6,
        proto_addr_len: 4,
        operation: 1,                        // Request
        sender_mac: OUR_MAC,
        sender_ip: OUR_IP.as_u32(),
        target_mac: [0; 6],                  // Unknown
        target_ip: target_ip.as_u32(),
    };
    
    send_ethernet_frame(BROADCAST_MAC, 0x0806, &arp_request)?;
    Ok(())
}
```

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

```
1. Ethernet frame arrives
   ↓
2. E1000 DMA transfers to RX buffer
   ↓
3. E1000 generates interrupt
   ↓
4. IRQ handler processes RX
   ↓
5. Packet added to rx_packets queue
   ↓
6. Shell main loop calls process_packets()
   ↓
7. Shell processes each queued packet
   ↓
8. Calls appropriate handler (ARP, IP, etc.)
```

### E1000 Reception

```rust
pub extern "x86-interrupt" fn e1000_rx_handler(_frame: InterruptStackFrame) {
    // Read RX tail pointer (next buffer device filled)
    let tail = read_register(REG_RDT);
    
    // Process all filled descriptors
    while HEAD != tail {
        let desc = &RX_DESCRIPTORS[HEAD];
        
        if (desc.status & 0x01) != 0 {  // Descriptor done?
            // Copy packet to buffer
            let packet = &RX_BUFFERS[HEAD][0..desc.length as usize];
            RX_PACKETS.push(packet.to_vec());
            
            // Mark descriptor as used
            RX_DESCRIPTORS[HEAD].status = 0;
            
            // Advance head
            HEAD = (HEAD + 1) % RX_RING_SIZE;
        }
    }
    
    // Acknowledge interrupt
    write_register(REG_ICR, 0xFFFFFFFF);
}
```

## Checksum Calculation

### IP Header Checksum

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
// Count: 32
// Total: 64KB

// ARP cache: ~100 entries × 10 bytes = 1KB

// Total network memory: ~130KB
```

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

## Future Enhancements

### DHCP

Automatic IP configuration instead of hardcoded.

### TCP

Full reliable transport protocol for connections.

### UDP

Datagram transport for faster, unreliable communication.

### DNS

Domain name resolution.

### Routing Table

Support multiple routes and gateways.

## Next Steps

- **[Drivers Reference](drivers.md)** — Hardware interface
- **[Interrupts Reference](interrupts.md)** — Interrupt handling
- **[Architecture Overview](architecture.md)** — System organization
