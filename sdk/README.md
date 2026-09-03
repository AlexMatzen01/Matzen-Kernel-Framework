# MFK App SDK

Build userspace apps for Matzen Kernel Framework.

## App Types (Phase 1 - now)

1. **Script app (`.app` / `.sh`)** - batch of shell commands
   - Simplest, uses existing `write` command to create file
   - Example `apps/examples/hello.app:1`
   ```sh
   echo Hello
   ls
   calc 2 + 3
   ```
   - Run: `run /apps/hello.app arg1 arg2` -> `$0` path, `$1`..`$n`, `$@`, `$#`

2. **MFKE bytecode (`.mfke` / `.bin`)** - safe VM
   - Header: magic `MFKE` (0x454B4D46 LE), version 1, entry, bytecode_len
   - VM opcodes: `kernel/src/app/loader.rs:66` (`PUSH_IMM, ADD, SUB, MUL, DIV, MOD, DUP, POP, PRINT_STR, PRINT_INT, JMP, JZ, JNZ, SLEEP, YIELD, HALT`)
   - Build: `cargo run -p mfke-asm -- app.asm app.mfke` or use in-kernel generator `mkapp hello-mfke /apps/hello.mfke`

Future Phase 2: **Native ELF** (`ET_EXEC`, `PT_LOAD`) with user mode, paging, `int 0x80` syscalls.

## Quick Start

Inside kernel shell:

```sh
mkfs
mount
# Script app
mkapp hello /apps/hello.app
run /apps/hello.app
cat /apps/hello.app
appinfo /apps/hello.app

# Bytecode app (prebuilt examples)
mkapp hello-mfke /bin/hello.mfke
mkapp counter /bin/counter.mfke
run /bin/hello.mfke
run /bin/counter.mfke
ls /bin

# Writehex for host-injected binaries
writehex /tmp/data.bin 4D464B45...
run /tmp/data.bin
```

## Host-side workflow

Pre-create files before running kernel: `tools/mfke-asm.py` assembles text assembly to MFKE, then `cargo run -p mfk-runner -- --bundle-app apps/examples/hello.app`.

See `docs/reference/apps.md` for full ABI, header, opcode tables, and Rust template.

## SDK Template

Copy `sdk/template/`:

```bash
cp -r sdk/template myapp
cd myapp
# edit src/main.mfke.asm or app.app
cargo run -p mfke-asm -- src/app.asm target/app.mfke
# then inject via writehex or runner bundling
```

## Syscalls (bytecode `CALL` and native `int 0x80`)

| Nr | Name | Args | Desc |
|---|---|---|---|
| 0 | exit | code | Terminate |
| 1 | print_str | (handled via opcode) | |
| 2 | print_int | value | |
| 3 | yield | - | Cooperative |
| 4 | sleep | ms | |
| 5 | get_tick | -> tick | |

FS/net syscalls planned Phase 2 (use script `write`/`cat` for now).
