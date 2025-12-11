# Ctrl+C Interrupt Handling

## Overview

The kernel now supports interrupt handling with Ctrl+C. This allows users to cancel long-running operations (like ping) and return to the command prompt to enter new commands.

## Features

### Ctrl+C Detection
- When Ctrl+C (ASCII 0x03) is pressed in the shell, it sets a global interrupt flag
- Displays `^C` on the screen
- Clears the current command buffer
- Returns to the prompt for new input

### Interrupt Flag Mechanism
- Global `INTERRUPT_FLAG` (AtomicBool) tracks interrupt state
- Can be checked by long-running operations
- Automatically cleared on command execution or interrupt handling

### Integration with Ping
- Ping command checks `is_interrupted()` during the reply-waiting loop
- If interrupted, exits early with "Ping cancelled by user" message
- Clears any pending ping requests
- Returns to prompt immediately

## Usage

### Cancel a Command During Execution
```
mfk> ping 10.0.2.2 100
Pinging 10.0.2.2 with 100 packets...
Sent 100 ping(s), waiting for replies...
Reply from 10.0.2.2: seq=0 time=12ms
Reply from 10.0.2.2: seq=1 time=8ms
^C                              <- User presses Ctrl+C
Ping cancelled by user

mfk> help                        <- Immediately returns to prompt
```

### Cancel Before Command Execution
```
mfk> ping^C                      <- Ctrl+C during command entry
^C
mfk>                             <- New prompt
```

## Implementation Details

### Shell Module Changes
1. **New global flag**: `static INTERRUPT_FLAG: AtomicBool`
2. **Helper functions**:
   - `is_interrupted()` - Check if Ctrl+C was pressed
   - `clear_interrupt()` - Clear the interrupt flag
   - `set_interrupt()` - Set the interrupt flag (private)

3. **Main loop changes**:
   - Added `'\x03'` case to match Ctrl+C in keyboard input
   - Displays `^C`, clears buffer, returns to prompt
   - Sets interrupt flag so running operations can detect cancellation

### Ping Command Changes
- Added `clear_interrupt()` before waiting for replies
- Check `!is_interrupted()` in the reply-waiting loop
- Display cancellation message and return to prompt if interrupted

## Key Code Sections

### Shell Interrupt Detection
```rust
'\x03' => {
    // Ctrl+C detected
    set_interrupt();
    println!("^C");
    cmd_len = 0;
    cmd_buffer = [0; MAX_CMD_LENGTH];
    print!("\n{}", PROMPT);
}
```

### Ping Interrupt Handling
```rust
let mut received = 0;
clear_interrupt();

while get_tick_count() - start_time < timeout_ms && !is_interrupted() {
    // ... process packets and check for replies ...
}

if is_interrupted() {
    println!("Ping cancelled by user");
    clear_interrupt();
}
```

## Compatibility

- Works with any long-running operation that checks `is_interrupted()`
- Requires explicit interrupt handling in each command
- Safe for multi-threaded operations due to AtomicBool

## Future Enhancements

- Add Ctrl+C support to other long-running commands (file operations, etc.)
- Implement signal handlers for more robust interrupt handling
- Add Ctrl+Z for suspend/background operations (if job control is implemented)
- Handle Ctrl+D for EOF (end of input)
