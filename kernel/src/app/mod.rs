//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! App runtime - loading and executing user apps
//!
//! Supports:
//! - Text script apps (.app, .sh, .txt) -> interpreter
//! - MFKE bytecode apps (.mfke, .bin)   -> VM
//! - ELF detection (stub, returns error with guidance)
//!
//! Execution is cooperative and runs on the kernel stack (Phase 1 DOS-style).
//! Future: isolated user mode + scheduler (Phase 2/3).

pub mod abi;
pub mod interpreter;
pub mod loader;

use alloc::string::String;
use alloc::vec::Vec;

/// ELF magic
const ELF_MAGIC: &[u8; 4] = &[0x7F, b'E', b'L', b'F'];

/// Run app at `path` with optional args slice (args[0] = app name conventionally)
/// Returns exit code or error string printed to console.
pub fn run(path: &str, args: &[&str]) -> Result<i32, &'static str> {
    // Read file via FS
    // Need mounted FS
    if !crate::shell::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let mut device = crate::shell::mounted_device();
    let data = crate::shell::read_file_contents(path, &mut device)
        .ok_or("Failed to read file (not found or not mounted)")?;

    if data.is_empty() {
        crate::println!("[run] warning: empty file");
        return Ok(0);
    }

    // Detect type
    if data.len() >= 4 && &data[0..4] == ELF_MAGIC {
        crate::println!("ELF binary detected: '{}' ({} bytes)", path, data.len());
        crate::println!("  Native ELF execution requires Phase 2 (user mode + paging).");
        crate::println!("  Currently only MFKE bytecode and script apps are runnable.");
        crate::println!("  Hint: use 'mkapp' to create example apps, or ship a .app script.");
        return Err("ELF not yet supported (see mkapp)");
    }

    if data.len() >= 4 {
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic == loader::MFKE_MAGIC {
            crate::println!("[run] MFKE bytecode app '{}' ({} bytes)", path, data.len());
            let start = crate::shell::get_tick_count();
            let res = loader::execute_mfke(&data, args);
            let elapsed = crate::shell::get_tick_count().wrapping_sub(start);
            match res {
                Ok(code) => {
                    crate::println!(
                        "\n[app '{}' exited code {} in {} ticks]",
                        path,
                        code,
                        elapsed
                    );
                    return Ok(code);
                }
                Err(e) => {
                    crate::println!("\n[app '{}' failed: {}] ({} ticks)", path, e, elapsed);
                    return Err(e);
                }
            }
        }
    }

    // Heuristic: if file contains NUL byte, maybe binary but not MFKE -> try script fallback with warning
    let is_text = is_probably_text(&data);
    if !is_text {
        crate::println!(
            "[run] '{}' looks binary but not MFKE ({} bytes)",
            path,
            data.len()
        );
        crate::println!(
            "  Try 'writehex' for hex-encoded binaries or 'mkapp' to generate a valid MFKE."
        );
        return Err("Unknown binary format");
    }

    // Treat as script
    let res = interpreter::run_script(&data, path, args);
    match res {
        Ok(code) => {
            crate::println!("[script '{}' exited code {}]", path, code);
            Ok(code)
        }
        Err(e) => {
            crate::println!("[script '{}' failed: {}]", path, e);
            Err(e)
        }
    }
}

fn is_probably_text(data: &[u8]) -> bool {
    // allow high-utf8 but no NUL
    for &b in data {
        if b == 0 {
            return false;
        }
        if b < 0x09 {
            return false;
        }
        if b > 0x7E && b < 0xA0 && b != b'\n' as u8 && b != b'\r' as u8 && b != b'\t' as u8 {
            return false;
        }
    }
    // must be valid utf8 or mostly ascii
    core::str::from_utf8(data).is_ok()
}

/// Helper for shell `mkapp` to create example files on FS
pub fn create_example_app(name: &str, kind: &str) -> Result<(), &'static str> {
    let mut device = crate::shell::mounted_device();
    if !crate::shell::is_mounted() {
        return Err("Filesystem not mounted");
    }
    let kind_lc = to_lowercase(kind.trim());
    match kind_lc.as_str() {
        "hello" | "" => create_hello_script(name, &mut device),
        "hello-mfke" | "mfke" | "bytecode" => create_hello_mfke(name, &mut device),
        "counter" => create_counter_mfke(name, &mut device),
        "calc" => create_calc_script(name, &mut device),
        "filedemo" => create_filedemo_script(name, &mut device),
        "loop" => create_loop_mfke(name, &mut device),
        _ => Err("Unknown app kind. Try: hello, hello-mfke, counter, calc, filedemo, loop"),
    }
}

fn to_lowercase(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn create_hello_script(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let content = r#"# MFK script app - hello
echo === Hello from MFK App: $0 ===
echo Args: $@
echo Current directory:
pwd
echo Files:
ls
echo Uptime:
uptime
echo App finished. Exit code 0.
"#;
    crate::shell::write_file_contents(name, content.as_bytes(), device)?;
    crate::println!("Created script app '{}' ({} bytes)", name, content.len());
    Ok(())
}

fn create_calc_script(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let content = r#"# Calculator demo
echo Calculator demo
calc 42 + 58
calc 100 - 7
calc 6 * 7
calc 20 / 4
echo Done
"#;
    crate::shell::write_file_contents(name, content.as_bytes(), device)?;
    Ok(())
}

fn create_filedemo_script(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let content = r#"# File demo - creates and reads a file
echo Creating demo file...
write /tmp/demo.txt Hello from app file demo!
cat /tmp/demo.txt
echo Listing /tmp:
ls /tmp
echo Done
"#;
    crate::shell::write_file_contents(name, content.as_bytes(), device)?;
    Ok(())
}

fn create_hello_mfke(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    // Bytecode: print "Hello from MFKE bytecode!\n", print int 42, halt
    let mut bc = Vec::new();
    // PRINT_STR "Hello from MFKE bytecode! "
    let msg = b"Hello from MFKE bytecode! ";
    bc.push(loader::opcode::PRINT_STR);
    bc.extend_from_slice(&(msg.len() as u16).to_le_bytes());
    bc.extend_from_slice(msg);
    bc.push(loader::opcode::PRINT_NL);
    // PUSH 42, PRINT_INT, PRINT_NL
    bc.extend_from_slice(&loader::encode_push(42));
    bc.push(loader::opcode::PRINT_INT);
    bc.push(loader::opcode::PRINT_NL);
    // Second message
    let msg2 = b"Bytecode VM works! Exit 0.";
    bc.push(loader::opcode::PRINT_STR);
    bc.extend_from_slice(&(msg2.len() as u16).to_le_bytes());
    bc.extend_from_slice(msg2);
    bc.push(loader::opcode::PRINT_NL);
    bc.push(loader::opcode::HALT);
    let file = loader::build_mfke(&bc);
    crate::shell::write_file_contents(name, &file, device)?;
    crate::println!(
        "Created MFKE bytecode app '{}' ({} bytes, bytecode {} bytes)",
        name,
        file.len(),
        bc.len()
    );
    Ok(())
}

fn create_counter_mfke(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    // Loop: counter 0..5 print
    // Pseudocode:
    // PUSH 0 (counter)
    // loop:
    //   DUP, PRINT_INT, PRINT_NL
    //   PUSH 1, ADD
    //   DUP, PUSH 5, LT, JZ done, JMP loop
    // done: POP, HALT
    let mut bc = Vec::new();
    bc.extend_from_slice(&loader::encode_push(0));
    // loop start at offset after this
    let loop_start = bc.len();
    bc.push(loader::opcode::DUP);
    bc.push(loader::opcode::PRINT_INT);
    bc.push(loader::opcode::PRINT_NL);
    bc.extend_from_slice(&loader::encode_push(1));
    bc.push(loader::opcode::ADD);
    // check
    bc.push(loader::opcode::DUP);
    bc.extend_from_slice(&loader::encode_push(5));
    bc.push(loader::opcode::LT);
    // JZ to end (need offset)
    let jz_pos = bc.len();
    bc.push(loader::opcode::JZ);
    bc.extend_from_slice(&0i16.to_le_bytes()); // placeholder
                                               // JMP back to loop
    let jmp_pos = bc.len();
    bc.push(loader::opcode::JMP);
    bc.extend_from_slice(&0i16.to_le_bytes()); // placeholder

    let end_pos = bc.len();
    bc.push(loader::opcode::POP); // pop counter
    let msg = b"Counter done.";
    bc.push(loader::opcode::PRINT_STR);
    bc.extend_from_slice(&(msg.len() as u16).to_le_bytes());
    bc.extend_from_slice(msg);
    bc.push(loader::opcode::PRINT_NL);
    bc.push(loader::opcode::HALT);

    // fixup offsets: JZ to end, JMP to loop
    // pc after JZ opcode is at jz_pos+3 (opcode + 2 byte offset)
    let jz_next = jz_pos + 3;
    let jz_offset = (end_pos as isize - jz_next as isize) as i16;
    bc[jz_pos + 1..jz_pos + 3].copy_from_slice(&jz_offset.to_le_bytes());

    let jmp_next = jmp_pos + 3;
    let jmp_offset = (loop_start as isize - jmp_next as isize) as i16;
    bc[jmp_pos + 1..jmp_pos + 3].copy_from_slice(&jmp_offset.to_le_bytes());

    let file = loader::build_mfke(&bc);
    crate::shell::write_file_contents(name, &file, device)?;
    crate::println!("Created counter MFKE app '{}' ({} bytes)", name, file.len());
    Ok(())
}

fn create_loop_mfke(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    // Infinite-ish loop that yields and checks tick
    // PUSH 0, loop: DUP PRINT_INT, SLEEP 500, PUSH 1 ADD, DUP PUSH 10 LT JZ end, JMP loop
    let mut bc = Vec::new();
    bc.extend_from_slice(&loader::encode_push(0));
    let loop_start = bc.len();
    bc.push(loader::opcode::DUP);
    bc.push(loader::opcode::PRINT_INT);
    bc.push(loader::opcode::PRINT_NL);
    bc.push(loader::opcode::SLEEP);
    bc.extend_from_slice(&500u16.to_le_bytes());
    bc.extend_from_slice(&loader::encode_push(1));
    bc.push(loader::opcode::ADD);
    bc.push(loader::opcode::DUP);
    bc.extend_from_slice(&loader::encode_push(10));
    bc.push(loader::opcode::LT);
    let jz_pos = bc.len();
    bc.push(loader::opcode::JZ);
    bc.extend_from_slice(&0i16.to_le_bytes());
    let jmp_pos = bc.len();
    bc.push(loader::opcode::JMP);
    bc.extend_from_slice(&0i16.to_le_bytes());
    let end_pos = bc.len();
    bc.push(loader::opcode::POP);
    bc.push(loader::opcode::HALT);

    let jz_next = jz_pos + 3;
    bc[jz_pos + 1..jz_pos + 3]
        .copy_from_slice(&((end_pos as isize - jz_next as isize) as i16).to_le_bytes());
    let jmp_next = jmp_pos + 3;
    bc[jmp_pos + 1..jmp_pos + 3]
        .copy_from_slice(&((loop_start as isize - jmp_next as isize) as i16).to_le_bytes());

    let file = loader::build_mfke(&bc);
    crate::shell::write_file_contents(name, &file, device)?;
    Ok(())
}

/// Write file from hex string (for host-injected binaries)
pub fn write_hex_file(path: &str, hex: &str) -> Result<usize, &'static str> {
    let hex = hex.trim();
    if hex.is_empty() {
        return Err("Empty hex");
    }
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        return Err("Hex length must be even");
    }
    let mut bytes = Vec::with_capacity(clean.len() / 2);
    let mut chars = clean.chars();
    while let Some(hi) = chars.next() {
        let lo = chars.next().ok_or("Odd hex")?;
        let hv = hi.to_digit(16).ok_or("Invalid hex digit")? as u8;
        let lv = lo.to_digit(16).ok_or("Invalid hex digit")? as u8;
        bytes.push((hv << 4) | lv);
    }
    let len = bytes.len();
    let mut device = crate::shell::mounted_device();
    crate::shell::write_file_contents(path, &bytes, &mut device)?;
    Ok(len)
}
