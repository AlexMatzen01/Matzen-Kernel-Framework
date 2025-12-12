# Shell Commands Reference

Complete list of all built-in shell commands.

## Help & Information

### `help`
Display all available commands with brief descriptions.

```bash
mfk> help
Available commands:
  help      - Display this help message
  ...
```

### `about`
Show project information and features.

```bash
mfk> about
Matzen Kernel Framework (MFK) v0.1.0
A simple terminal OS written in Rust

Features:
  - VGA text mode output
  - PS/2 keyboard input
  - Built-in command shell
  ...
```

### `version`
Display kernel version and build info.

```bash
mfk> version
Matzen Kernel Framework (MFK)
  Version:  0.1.0
  Build:    debug
  Arch:     x86_64
  Compiler: rustc (nightly)
```

### `cpuinfo`
Display CPU information (vendor, model, features).

```bash
mfk> cpuinfo
CPU Information:
  Vendor: GenuineIntel (or AuthenticAMD)
  ...
```

## Display & Terminal

### `clear` / `cls`
Clear the terminal screen.

```bash
mfk> clear
```

Clears VGA buffer and positions cursor at top-left.

### `echo <text>`
Print text to the terminal.

```bash
mfk> echo "Hello, MFK!"
Hello, MFK!

mfk> echo "Current system: $(uptime)"
```

### `color <color>`
Change terminal text color.

```bash
mfk> color green      # Green text
mfk> color white      # White text (default)
mfk> color cyan       # Cyan text
mfk> color yellow     # Yellow text
mfk> color red        # Red text
mfk> color blue       # Blue text
mfk> color pink       # Pink/Magenta text
```

## System Information

### `uptime`
Show system uptime since boot.

```bash
mfk> uptime
System uptime: 5 minutes, 23 seconds
(Loop iterations: 52340000)
```

Note: Approximation based on main loop iterations (~1000/sec).

### `memory` / `mem`
Display memory layout and availability.

```bash
mfk> memory
Memory Information:
  VGA Buffer:    0xB8000 (4 KB)
  Kernel loaded: 0x100000 (varies)

Memory Layout:
  0x00000000 - 0x0009FFFF: Conventional Memory (640 KB)
  0x000A0000 - 0x000BFFFF: VGA Memory
  0x000C0000 - 0x000FFFFF: ROM Area
  0x00100000+            : Extended Memory (Kernel)
```

### `date`
Display current date and time from RTC.

```bash
mfk> date
Friday, December 10, 2025
14:30:45 UTC
```

### `whoami`
Display current user (always "root").

```bash
mfk> whoami
root
```

### `test`
Run system diagnostics and tests.

```bash
mfk> test
Running tests...
✓ Test 1 passed
✓ Test 2 passed
...
```

## Calculation

### `calc <expression>`
Simple arithmetic calculator.

```bash
mfk> calc 5 + 3
Result: 8

mfk> calc 100 * 2
Result: 200

mfk> calc 10 / 2
Result: 5
```

Supports: `+`, `-`, `*`, `/`

## System Control

### `reboot`
Reboot the system.

```bash
mfk> reboot
Rebooting...
```

Triggers system reset via keyboard controller.

### `halt` / `shutdown`
Halt/shutdown the system.

```bash
mfk> halt
System halted. You can now turn off your computer.
```

System enters infinite halt loop. Press Ctrl+C or close QEMU.

## Network Commands

### `ifconfig [IP]`
Configure or display network interface settings.

**Display current config:**
```bash
mfk> ifconfig
Network Interface:
  MAC Address: 52:54:00:12:34:56
  IP Address:  10.0.2.15
```

**Set IP address:**
```bash
mfk> ifconfig 192.168.1.100
IP address set to 192.168.1.100
```

Format: `ifconfig <octet1>.<octet2>.<octet3>.<octet4>`

### `ping <IP> [count]`
Send ICMP echo requests (ping).

```bash
mfk> ping 10.0.2.2 4
Pinging 10.0.2.2 with 4 packets...
Reply from 10.0.2.2: seq=0 time=2ms
Reply from 10.0.2.2: seq=1 time=1ms
Reply from 10.0.2.2: seq=2 time=1ms
Reply from 10.0.2.2: seq=3 time=2ms
--- ping statistics ---
4 packets transmitted, 4 received, 0% packet loss
```

Parameters:
- `<IP>` — Target IP address (required)
- `[count]` — Number of packets (default: 4)

Timeout: 5 seconds per ping batch

### `netstat`
Display network status and statistics.

```bash
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
  TCP      - Not implemented
```

## File System Commands

### `diskinfo`
Display disk information.

```bash
mfk> diskinfo
Disk Information:
  Drive 0 (Primary Master): 2880 blocks
  Drive 1 (Primary Slave): 2880 blocks
  ...
```

### `mkfs`
Format disk with SimpleFS filesystem.

```bash
mfk> mkfs
Formatting disk...
Filesystem created and mounted
```

Creates inode table and initializes filesystem structures.

### `mount`
Mount the filesystem.

```bash
mfk> mount
Filesystem mounted
```

Must run after `mkfs` or on an existing formatted disk.

### `ls` / `dir`
List files in current directory.

```bash
mfk> ls
Files:
  file1.txt (512 bytes)
  data.bin (1024 bytes)
  readme.txt (256 bytes)
```

### `touch <filename>`
Create an empty file.

```bash
mfk> touch newfile.txt
File created: newfile.txt
```

### `write <filename> <content>`
Create or overwrite a file with content.

```bash
mfk> write test.txt "Hello, World!"
File written: test.txt (13 bytes)

mfk> write data.bin This_is_longer_content
File written: data.bin (25 bytes)
```

### `cat <filename>`
Display file contents.

```bash
mfk> cat test.txt
Hello, World!
```

For binary files, displays raw bytes.

### `rm <filename>`
Delete a file.

```bash
mfk> rm test.txt
File deleted: test.txt
```

## Command Examples

### System Exploration
```bash
mfk> about              # What is MFK?
mfk> help               # Available commands
mfk> cpuinfo            # CPU vendor and model
mfk> memory             # Memory map
mfk> uptime             # How long running?
mfk> date               # Current date/time
```

### Network Testing
```bash
mfk> ifconfig           # Check network config
mfk> netstat            # Network status
mfk> ping 10.0.2.2 4    # Ping gateway
mfk> ping 8.8.8.8       # Ping external (may not work)
```

### File Operations
```bash
mfk> mkfs               # Create filesystem
mfk> mount              # Mount filesystem
mfk> ls                 # List files
mfk> write test.txt "content"  # Create file
mfk> cat test.txt       # Read file
mfk> rm test.txt        # Delete file
```

### System Control
```bash
mfk> color green        # Change color
mfk> clear              # Clear screen
mfk> uptime             # Show uptime
mfk> calc 5 + 3         # Calculate
mfk> halt               # Shutdown
```

## Command Tips

- **Case-sensitive**: Commands are case-sensitive (`Help` ≠ `help`)
- **Whitespace**: Commands separated by spaces
- **No pipes**: No redirection or piping (not implemented)
- **Line editing**: Backspace to delete, no arrow keys
- **Interrupt**: Ctrl+C may cancel long operations
- **Short forms**: Some commands have aliases (`cls` = `clear`, `mem` = `memory`)

## Error Handling

If a command fails:

```bash
mfk> ping invalid
Invalid IP address format

mfk> cat nonexistent.txt
File not found

mfk> unknown_command
Unknown command: 'unknown_command'. Type 'help' for available commands.
```

## Future Commands

Planned but not yet implemented:
- `cat` with multiple files
- `grep` for file searching
- `mkdir` for directory creation
- `cd` for directory navigation
- `tcp` for TCP connection testing
- `dhcp` for automatic IP configuration
- `dns` for domain resolution

## Next Steps

- **[Network Stack](networking.md)** — How networking works
- **[File System](filesystem.md)** — File storage details
- **[Running the Kernel](../guide/running.md)** — Boot and use
