# Networking in MFK

## Overview
The Matzen Kernel Framework now includes basic networking support with an Intel E1000 driver and a simple network stack.

## Features

### Network Driver
- **Intel E1000 NIC Driver**: Supports the E1000 network card commonly used in QEMU
- Transmit and receive packet queuing
- MAC address configuration

### Network Stack
- **Ethernet (Layer 2)**: Frame handling with ethertype recognition
- **ARP**: Address Resolution Protocol for IP-to-MAC mapping
- **IPv4 (Layer 3)**: Basic IP packet handling
- **ICMP**: Internet Control Message Protocol (ping support)
- **UDP**: User Datagram Protocol (connectionless communication)
- **TCP**: Transmission Control Protocol (connection-oriented communication)

## Shell Commands

### `ifconfig [ip-address]`
Configure or display network interface settings.

**Examples:**
```
ifconfig                  # Display current configuration
ifconfig 10.0.2.15       # Set IP address
```

### `ping <ip-address>`
Send ICMP echo request to test connectivity.

**Example:**
```
ping 10.0.2.2            # Ping the QEMU gateway
```

### `netstat`
Display network status and protocol information.

### `tcpconnect <ip-address> <port>`
Establish a TCP connection to a remote server.

**Examples:**
```
tcpconnect 10.0.2.2 80           # Connect to QEMU host on port 80
tcpconnect 93.184.216.34 80      # Connect to example.com (requires TAP)
```

### `tcpsend <local-port> <data>`
Send data over an established TCP connection.

**Example:**
```
tcpsend 49152 GET / HTTP/1.0     # Send HTTP request
```

### `tcpclose <local-port>`
Close a TCP connection.

**Example:**
```
tcpclose 49152                    # Close connection on port 49152
```

## Usage Example

```
mfk> ifconfig
Network Interface:
  MAC Address: 52:54:00:12:34:56
  IP Address:  Not configured

mfk> ifconfig 10.0.2.15
IP address set to 10.0.2.15

mfk> ping 10.0.2.2
Pinging 10.0.2.2...
Ping sent successfully
Note: Check serial output for replies

mfk> netstat
Network Status:

Interface: E1000
  MAC: 52:54:00:12:34:56
  IP:  10.0.2.15

Protocol Stack:
  Ethernet - Active
  ARP      - Active
  IPv4     - Active
  ICMP     - Active
  UDP      - Active
  TCP      - Active

mfk> tcpconnect 10.0.2.2 80
Connecting to 10.0.2.2:80...
Connection initiated from local port 49152
Waiting for connection to establish...
Connection established! Local port: 49152

mfk> tcpsend 49152 GET / HTTP/1.0
Sent 17 bytes on port 49152
Checking for response...
Received 256 bytes:
HTTP/1.0 200 OK
...

mfk> tcpclose 49152
Closing connection on port 49152
```

## QEMU Network Configuration

The kernel runs with QEMU's user-mode networking:
- Default gateway: 10.0.2.2
- DNS server: 10.0.2.3
- Suggested kernel IP: 10.0.2.15

Port forwarding is configured by default for:
- UDP `5555` -> guest `5555`
- TCP `49152` -> guest `49152`

`run.sh` uses unrestricted user-mode networking (`restrict=off`) so the guest can make outbound internet and LAN connections without additional setup.

Note: ICMP echo replies (ping) are still limited by QEMU user-mode networking. For reliable ICMP testing, use TAP mode (`run_with_tap.sh`).

## Implementation Details

### E1000 Driver (`kernel/src/drivers/e1000.rs`)
- Memory-mapped I/O access
- 32 RX descriptors with 2048-byte buffers
- 8 TX descriptors with 2048-byte buffers
- Automatic descriptor ring management

### Network Stack (`kernel/src/net/`)
- **ethernet.rs**: Frame construction and parsing
- **arp.rs**: ARP cache and request/reply handling
- **ip.rs**: IPv4 header construction and checksum
- **icmp.rs**: Echo request/reply (ping)
- **udp.rs**: UDP packet handling
- **tcp.rs**: TCP connection management, 3-way handshake, data transfer

### Packet Flow
1. E1000 driver receives raw ethernet frames
2. Ethernet layer parses frame and dispatches by ethertype
3. ARP handles address resolution
4. IP layer validates and routes packets
5. ICMP/UDP handlers process protocol-specific data

## Future Enhancements
- TCP retransmission and congestion control
- DHCP client for automatic IP configuration
- DNS resolver
- Socket API with file descriptors
- Multiple network interfaces
- IPv6 support
- TLS/SSL support

## Limitations
- Basic TCP implementation (no retransmission, window scaling, or advanced features)
- Single network interface only
- No fragmentation/reassembly
- Simplified routing (assumes single subnet)
- ARP cache doesn't expire entries
- Limited error handling
- QEMU user-mode networking restrictions (see ICMP_STATUS.md)
