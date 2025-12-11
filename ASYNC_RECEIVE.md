# Async Receive Implementation

## Overview

The kernel now has a fully functional asynchronous packet receive system for handling ICMP ping responses. This implementation allows the ping command to send multiple packets and wait for replies with timeout handling.

## Implementation Details

### Components

1. **Pending Request Tracking** (`kernel/src/net/icmp.rs`):
   - `PENDING_PINGS`: BTreeMap storing (identifier, sequence) -> PingRequest
   - `PING_REPLIES`: Vector storing received replies
   - Each ping request is tracked with its target IP, identifier, sequence, and timestamp

2. **Reply Processing**:
   - When an ICMP echo reply arrives, it's matched against pending requests
   - RTT (Round Trip Time) is calculated using tick counts
   - Matched replies are stored in the replies queue for retrieval

3. **Timeout Management**:
   - `check_timeouts()`: Removes requests that have exceeded the 5-second timeout
   - Prevents memory leaks from lost packets

4. **Shell Integration** (`kernel/src/shell/mod.rs`):
   - `get_tick_count()`: Public function to access the tick counter
   - Enhanced `cmd_ping()`: Sends multiple packets, waits for replies, displays statistics

## Usage

### Basic Ping (4 packets)
```
ping 10.0.2.2
```

### Ping with Custom Count
```
ping 10.0.2.2 10
```

### Expected Output
```
mfk> ping 10.0.2.2 4
Pinging 10.0.2.2 with 4 packets...
Sent 4 ping(s), waiting for replies...
Reply from 10.0.2.2: seq=0 time=12ms
Reply from 10.0.2.2: seq=1 time=8ms
Reply from 10.0.2.2: seq=2 time=9ms
Reply from 10.0.2.2: seq=3 time=11ms
--- ping statistics ---
4 packets transmitted, 4 received, 0% packet loss
```

## Technical Features

- **Asynchronous Processing**: Main loop polls for incoming packets while waiting
- **Request Matching**: Uses (identifier, sequence) tuple to match replies to requests
- **RTT Calculation**: Measures round-trip time in milliseconds using tick counter
- **Timeout Handling**: 5-second timeout prevents infinite waiting
- **Statistics Display**: Shows packets sent, received, and packet loss percentage

## Data Structures

### PingRequest
```rust
pub struct PingRequest {
    pub target_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub sent_time: u64,
}
```

### PingReply
```rust
pub struct PingReply {
    pub source_ip: [u8; 4],
    pub identifier: u16,
    pub sequence: u16,
    pub rtt_ms: u64,
}
```

## Testing

1. Boot the kernel: `sh run.sh`
2. Configure network interface: `ifconfig 10.0.2.15`
3. Test ping to QEMU gateway: `ping 10.0.2.2`
4. Test with multiple packets: `ping 10.0.2.2 10`
5. Test timeout with invalid IP: `ping 192.168.1.1`

## Performance Notes

- Tick counter provides rough millisecond timing (not calibrated to real time)
- RTT measurements are approximate based on main loop iterations
- Maximum timeout is 5000 ticks (~5 seconds)
- Small delays between packets prevent network flooding
