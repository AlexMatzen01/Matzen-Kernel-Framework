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

### `ifconfig [ip-address] [netmask] [gateway]`
Configure or display network interface settings. The netmask and gateway are
optional; the default netmask is `255.255.255.0`.

**Examples:**
```
ifconfig                  # Display current configuration
ifconfig 10.0.2.15 255.255.255.0 10.0.2.2
```

### `ping <ip-address|hostname> [count]`
Send ICMP echo requests. IP destinations are resolved with ARP; hostnames use
DNS. The command has a bounded five-second reply wait.

**Example:**
```
ping 10.0.2.2 4          # Ping the QEMU gateway
```

### `dns <hostname>`
Resolve a hostname to IPv4.

### `arp [-a|list|<ip-address>]`
Show the ARP cache or resolve an address.

### `netstat`
Display interface, route, ARP, and protocol state.

### `udp-send` / `udp-recv`
Send and receive diagnostic UDP datagrams.

### `wget` / `speedtest`
Use the existing HTTP client and LibreSpeed client. Add `-d` or `--debug` for
network traces.

### `netdebug` / `tlsinfo`
Toggle packet traces or show the optional TLS 1.3 build status.

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

### `tcpstatus <local-port>` / `tcprecv <local-port> [seconds]`
Inspect a TCP connection or wait for its received data.

## Usage Example

```
mfk> ifconfig
Network Interface:
  MAC Address: 52:54:00:12:34:56
  IP Address:  Not configured

mfk> ifconfig 10.0.2.15 255.255.255.0 10.0.2.2
Network configured: 10.0.2.15 / 255.255.255.0 gateway 10.0.2.2

mfk> ping 10.0.2.2 4
Pinging 10.0.2.2 with 4 packets...

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

For QEMU user-mode networking, configure the gateway and netmask explicitly:

```
ifconfig 10.0.2.15 255.255.255.0 10.0.2.2
```

ARP resolves the gateway before IP packets are sent. TAP mode remains useful
when testing hosts outside the QEMU user-mode NAT.

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
