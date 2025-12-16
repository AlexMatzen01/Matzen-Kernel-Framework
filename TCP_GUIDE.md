# TCP Implementation and Testing Guide

## Overview

MFK now includes a basic TCP (Transmission Control Protocol) implementation supporting:
- 3-way handshake (SYN, SYN-ACK, ACK)
- Connection establishment
- Data transmission
- Connection teardown (FIN)
- Multiple simultaneous connections

## TCP Features

### Implemented
- ✅ SYN/SYN-ACK/ACK handshake for connection establishment
- ✅ ACK acknowledgments for received data
- ✅ FIN/ACK for graceful connection close
- ✅ Per-connection state tracking
- ✅ Receive and send buffers
- ✅ TCP checksum calculation (including pseudo-header)
- ✅ Multiple concurrent connections

### Not Yet Implemented
- ❌ Retransmission on packet loss
- ❌ Congestion control
- ❌ Window scaling
- ❌ Selective acknowledgments (SACK)
- ❌ Fast retransmit/recovery
- ❌ TCP options (timestamps, MSS, etc.)
- ❌ Out-of-order packet reassembly

## Shell Commands

### `tcpconnect <ip> <port>`
Initiates a TCP connection to a remote server.

**Examples:**
```bash
# Connect to QEMU host web server
tcpconnect 10.0.2.2 80

# Connect to external server (requires TAP networking)
tcpconnect 93.184.216.34 80
```

**Output:**
```
Connecting to 10.0.2.2:80...
Connection initiated from local port 49152
Waiting for connection to establish...
Connection established! Local port: 49152
Use 'tcpsend 49152 <data>' to send data
Use 'tcpclose 49152' to close connection
```

### `tcpsend <local-port> <data>`
Sends data over an established TCP connection.

**Examples:**
```bash
# Send HTTP GET request
tcpsend 49152 GET / HTTP/1.0

# Send custom data
tcpsend 49152 Hello, Server!
```

**Output:**
```
Sent 17 bytes on port 49152
Checking for response...
Received 256 bytes:
HTTP/1.0 200 OK
Content-Type: text/html
...
```

### `tcpclose <local-port>`
Closes a TCP connection gracefully.

**Example:**
```bash
tcpclose 49152
```

## Testing TCP with QEMU User-Mode Networking

QEMU's user-mode networking has **better TCP support** than ICMP, but with limitations:

### What Works
- ✅ Outbound connections to the host (10.0.2.2)
- ✅ Connections to forwarded ports
- ✅ HTTP requests to QEMU's host gateway

### What Doesn't Work
- ❌ Direct connections to external IPs (due to NAT)
- ❌ Incoming connections (no port forwarding by default)

### Example Test Session

```bash
# Build and run
./build.sh
./run.sh target/x86_64-mfk/debug/mfk-kernel

# In the kernel shell:
mfk> ifconfig 10.0.2.15
IP address set to 10.0.2.15

mfk> tcpconnect 10.0.2.2 80
Connecting to 10.0.2.2:80...
Connection established! Local port: 49152

mfk> tcpsend 49152 GET / HTTP/1.0\r\n\r\n
Sent 18 bytes on port 49152
Checking for response...
(Response from server if available)

mfk> tcpclose 49152
Closing connection on port 49152
```

## Testing TCP with TAP Networking (Recommended)

For full TCP functionality, use TAP networking:

```bash
# Run with TAP networking
./run_with_tap.sh

# In kernel:
ifconfig 192.168.100.2
tcpconnect 192.168.100.1 80

# From host (in another terminal):
# Start a simple HTTP server
python3 -m http.server 80

# Or use netcat
nc -l 80
```

## TCP State Machine

The implementation follows standard TCP states:

```
CLOSED → SYN_SENT → ESTABLISHED → FIN_WAIT_1 → FIN_WAIT_2 → TIME_WAIT → CLOSED
                  ↓
              CLOSE_WAIT → LAST_ACK → CLOSED
```

**Current States Used:**
- `Closed`: No connection
- `SynSent`: SYN sent, waiting for SYN-ACK
- `Established`: Connection active, can send/receive data
- `FinWait1`: FIN sent, waiting for ACK
- `CloseWait`: Received FIN, waiting for application close

## Debugging TCP

Enable verbose logging by watching serial output:

```
TCP: Sending packet to 10.0.2.2:80, flags=0x2, seq=1000, ack=0, data_len=0
TCP: Received packet from 10.0.2.2:80 to port 49152, flags=0x12, seq=0, ack=1001
TCP: Received SYN-ACK
TCP connection established to 10.0.2.2:80
```

**Flag Values:**
- `0x01` = FIN
- `0x02` = SYN
- `0x04` = RST
- `0x08` = PSH
- `0x10` = ACK
- `0x12` = SYN+ACK
- `0x18` = PSH+ACK

## Implementation Details

### File: `kernel/src/net/tcp.rs`

**Key Functions:**
- `connect()`: Initiates TCP connection (sends SYN)
- `send_data()`: Transmits data over established connection
- `close()`: Gracefully closes connection (sends FIN)
- `process_packet()`: Handles incoming TCP segments
- `get_state()`: Returns current connection state
- `read_data()`: Retrieves received data from buffer

**Data Structures:**
- `TcpHeader`: Standard 20-byte TCP header
- `TcpConnection`: Per-connection state (seq/ack nums, buffers, state)
- `TcpState`: Enum for TCP state machine

### Integration

TCP packets are routed through the IP layer:
1. Application calls `tcp::connect()` or `tcp::send_data()`
2. TCP constructs segment with header + data
3. Calls `ip::send_packet()` with protocol=6 (TCP)
4. IP wraps in IP header, calculates checksum
5. Ethernet layer adds frame header, gets destination MAC via ARP
6. E1000 driver transmits packet

Received packets follow reverse path:
1. E1000 receives frame
2. Ethernet extracts payload, checks ethertype
3. IP validates header, routes by protocol
4. TCP processes segment, updates connection state
5. Data stored in connection's receive buffer

## Limitations and Known Issues

1. **No Retransmission**: Lost packets are not retransmitted
2. **No Timeouts**: Connections don't time out if remote host disappears
3. **Fixed Sequence Numbers**: Uses simple counter, not cryptographically random
4. **No Window Management**: Advertises fixed window size (8192 bytes)
5. **QEMU User-Mode**: Limited to connections to host gateway (10.0.2.2)
6. **No Listening Sockets**: Cannot act as a server (yet)

## Future Improvements

- [ ] Add retransmission timer
- [ ] Implement sliding window protocol
- [ ] Support listening/server sockets
- [ ] Add timeout handling
- [ ] Implement congestion control (Reno/Cubic)
- [ ] Support TCP options (MSS, window scale, SACK)
- [ ] Better sequence number generation
- [ ] Connection pooling and limits

## See Also

- [NETWORKING.md](NETWORKING.md) - General networking documentation
- [ICMP_STATUS.md](ICMP_STATUS.md) - ICMP/ping limitations with QEMU
- [kernel/src/net/tcp.rs](kernel/src/net/tcp.rs) - TCP implementation source
