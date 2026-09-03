# App Runtime Reference

Kernel can now run apps via `run` command.

## Overview

```
Host (cargo build)
   ↓ .app (script) or .mfke (bytecode)
Disk (SimplFS on AtaBlockDevice)
   ↓ run <path> in shell
App Runtime (kernel/src/app/*)
   ├─ interpreter.rs (script batch)
   └─ loader.rs VM (MFKE bytecode)
   ↓ syscalls / shell cmds
Kernel services (VGA, FS, net)
```

Phase 1 is cooperative DOS-style: `run` blocks shell until app exits. Ctrl+C aborts. Future Phase 2 adds preemption + isolation.

## File Detection

1. ELF magic `7F 45 4C 46` -> error with hint (native Phase 2 not yet)
2. MFKE magic `4D 46 4B 45` LE `0x454B4D46` -> VM `loader::execute_mfke` `kernel/src/app/loader.rs:99`
3. Otherwise if no NUL byte and valid UTF-8 -> script `interpreter::run_script` `kernel/src/app/interpreter.rs:31`
4. Else -> unknown binary error, suggest `writehex`

## Script Apps

* File: UTF-8 text, `#` comment, empty lines skipped.
* Each line `trim()` then `shell::execute_command` `kernel/src/shell/mod.rs:140`.
* Expansions: `$0` path, `$1..$9`, `$@` all args, `$#` argc. Example `apps/examples/hello.app:1`.
* Control: `exit` or `exit 42` terminates script early with code.
* Guard: `halt`/`reboot` inside app are ignored (printed, not executed).

Create via `write` or `mkapp` `kernel/src/app/mod.rs:105`:
```sh
mkapp hello /apps/hello.app
```

## MFKE Bytecode

### Header `kernel/src/app/loader.rs:9` (32 bytes, packed LE)

| Off | Field | Type | Value |
|---|---|---|---|
| 0 | magic | u32 | 0x454B4D46 "MFKE" |
| 4 | version | u32 | 1 |
| 8 | entry_offset | u32 | offset to bytecode (normally 32) |
|12 | bytecode_len | u32 | len |
|16 | mem_extra | u32 | reserved 0 |
|20 | flags | u32 | 0 |
|24 | reserved | [u32;2] | 0 |

Build helper `loader::build_mfke` `kernel/src/app/loader.rs:175`.

### Opcodes `kernel/src/app/loader.rs:46`

| Code | Mnemonic | Encoding | Stack | Desc |
|---|---|---|---|---|
| 0x00 | HALT | - | - | exit 0 |
| 0x01 | PUSH_IMM | <i32 LE> | -> v | push |
| 0x02 | ADD | - | a b -> a+b | |
| 0x03 | SUB | - | a b -> a-b | |
| 0x04 | MUL | - | -> a*b | |
| 0x05 | DIV | - | -> a/b | panic if 0 |
| 0x06 | MOD | - | -> a%b | |
| 0x07 | PRINT_INT | - | v -> | pop print |
| 0x08 | PRINT_STR | <u16 len><bytes> | - | print utf8 |
| 0x09 | PRINT_NL | - | - | newline |
| 0x0A | DUP | - | v -> v v | |
| 0x0B | POP | - | v -> | |
| 0x0C | JMP | <i16 rel> | - | pc = next+off |
| 0x0D | JZ | <i16 rel> | v -> | if v==0 jump |
| 0x0E | JNZ | - | v -> | if v!=0 jump |
| 0x0F | EQ | - | a b -> (a==b) | |
| 0x10 | LT | - | -> (a<b) | |
| 0x11 | GT | - | -> (a>b) | |
| 0x12 | SLEEP | <u16 ms> | - | busy+net |
| 0x13 | YIELD | - | - | cooperative |
| 0x14 | EXIT | - | v -> | pop code or 0 |
| 0x15 | CALL | <u8 nr><u8 argc> | varies | syscall |

Limits: MAX_STEPS 2M, MAX_STACK 1024, fuel check every 1024 steps `loader.rs:125` plus Ctrl+C `shell::is_interrupted` check.

### Example Generator

`kernel/src/app/mod.rs:176` `create_hello_mfke` / `create_counter_mfke` show assembly emission with fixups for `JMP` offsets.

Counter pseudo:
```asm
PUSH 0
loop:
  DUP
  PRINT_INT
  PRINT_NL
  PUSH 1
  ADD
  DUP
  PUSH 5
  LT
  JZ done
  JMP loop
done:
  POP
  HALT
```

## Shell Commands `kernel/src/shell/mod.rs:182`

* `run|exec <path> [args...]` `cmd_run` -> `app::run` `kernel/src/app/mod.rs:27`
* `mkapp <kind> <path>` `cmd_mkapp` -> `app::create_example_app`
  * kinds: `hello, hello-mfke, counter, calc, filedemo, loop` `kernel/src/app/mod.rs:110`
  * shorthand `mkapp /apps/foo.app` defaults hello
* `writehex <path> <hex>` `cmd_writehex` -> `app::write_hex_file` hex whitespace ignored
* `appinfo [path]` `cmd_appinfo` shows type, header, preview
* `ps` placeholder for Phase 2 scheduler

## VM Syscalls `kernel/src/app/abi.rs:15`

Stable numbers: `Exit 0, PrintStr 1, PrintInt 2, Yield 3, Sleep 4, GetTick 5, Fs* 10-15 (planned)`. `CALL` opcode dispatches directly.

Native ABI future: `SyscallTable` struct `abi.rs:49` with `extern C fn` pointers for apps compiled against same target (will be used when paging/GDT added).

## FS Helpers

Apps use shell commands for FS (script: `write`, `cat`, `ls`) or future syscalls. Direct helpers `shell::write_file_contents` `kernel/src/shell/mod.rs:1357` / `read_file_contents` require mounted FS.

## Runner Bundling (Phase 1.5)

`tools/src/main.rs` future: `--bundle-app host/path app/path` writes file into `target/disk.img` before boot (host-side SimplFS writer). Until implemented, use in-kernel `mkapp`/`write`/`writehex`.

## Phase 2 Plan (Not Yet)

* GDT/TSS `kernel/src/gdt.rs`, paging `kernel/src/memory.rs`, ELF loader `PT_LOAD`, syscall `int 0x80` handler in IDT, fault isolation (kill app not panic), PIT preemptive scheduler `kernel/src/scheduler.rs`.

## Examples

* `apps/examples/hello.app` script
* `apps/examples/filedemo.app` FS demo
* Generated MFKE: `mkapp counter /bin/counter.mfke; run /bin/counter.mfke` outputs 0 1 2 3 4.
