//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFKE Loader and Bytecode VM
//!
//! MFKE = Matzen Framework Kernel Executable
//! Minimal safe bytecode VM (not native x86) to allow apps without ring3/paging.
//! Future native ELF execution will be added behind `native` feature.

use crate::shell::{clear_interrupt, get_tick_count, is_interrupted};
use alloc::vec::Vec;

/// MFKE magic "MFKE" LE 0x454B464D
pub const MFKE_MAGIC: u32 = 0x454B4D46; // little endian: 'M','F','K','E' -> 0x45 0x4D etc? check: 'M'=0x4D, 'F'=0x46, 'K'=0x4B, 'E'=0x45 -> LE bytes 4D 46 4B 45 -> u32 0x454B464D
pub const MFKE_VERSION: u32 = 1;

/// Header 32 bytes, packed
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct MfkeHeader {
    pub magic: u32,
    pub version: u32,
    pub entry_offset: u32,
    pub bytecode_len: u32,
    pub mem_extra: u32, // reserved extra memory pages (not used)
    pub flags: u32,
    pub reserved: [u32; 2],
}

impl MfkeHeader {
    pub fn is_valid(&self) -> bool {
        let magic = unsafe { core::ptr::addr_of!(self.magic).read_unaligned() };
        let version = unsafe { core::ptr::addr_of!(self.version).read_unaligned() };
        magic == MFKE_MAGIC && version == MFKE_VERSION
    }
    pub fn entry(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.entry_offset).read_unaligned() }
    }
    pub fn len(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.bytecode_len).read_unaligned() }
    }
}

/// Bytecode opcodes - keep stable, documented in docs/reference/apps.md
pub mod opcode {
    pub const HALT: u8 = 0x00;
    pub const PUSH_IMM: u8 = 0x01; // <i32 LE>
    pub const ADD: u8 = 0x02;
    pub const SUB: u8 = 0x03;
    pub const MUL: u8 = 0x04;
    pub const DIV: u8 = 0x05;
    pub const MOD: u8 = 0x06;
    pub const PRINT_INT: u8 = 0x07; // pop and print
    pub const PRINT_STR: u8 = 0x08; // <u16 len LE> <bytes>
    pub const PRINT_NL: u8 = 0x09;
    pub const DUP: u8 = 0x0A;
    pub const POP: u8 = 0x0B;
    pub const JMP: u8 = 0x0C; // <i16 offset LE> relative to next pc
    pub const JZ: u8 = 0x0D; // <i16 offset> pop, if zero jump
    pub const JNZ: u8 = 0x0E;
    pub const EQ: u8 = 0x0F;
    pub const LT: u8 = 0x10;
    pub const GT: u8 = 0x11;
    pub const SLEEP: u8 = 0x12; // <u16 ms> approx
    pub const YIELD: u8 = 0x13;
    pub const EXIT: u8 = 0x14; // <i32 code> or pop if not provided? we use pop variant: EXIT pops code
    pub const CALL: u8 = 0x15; // <u8 syscall_nr> <u8 arg_count> - pops args, dispatches
                               // Extended string ops
    pub const PRINT_STR_N: u8 = 0x16; // alias to PRINT_STR (alternative)
}

/// Max steps to avoid infinite loop (approx fuel)
const MAX_STEPS: usize = 2_000_000;
const MAX_STACK: usize = 1024;

/// Execute MFKE bytecode image. Returns exit code or error string.
/// `data` is whole file bytes including header.
/// `args` currently unused but reserved for future argv.
pub fn execute_mfke(data: &[u8], _args: &[&str]) -> Result<i32, &'static str> {
    if data.len() < core::mem::size_of::<MfkeHeader>() {
        return Err("File too small for MFKE header");
    }
    let header = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const MfkeHeader) };
    if !header.is_valid() {
        return Err("Invalid MFKE magic/version");
    }
    let entry = header.entry() as usize;
    let bc_len = header.len() as usize;
    if entry as usize >= data.len() {
        return Err("Bad entry offset");
    }
    if entry + bc_len > data.len() {
        return Err("Bytecode length out of bounds");
    }
    let code = &data[entry..entry + bc_len];
    // VM state
    let mut pc: usize = 0;
    let mut stack: Vec<i64> = Vec::with_capacity(64);
    let mut steps: usize = 0;

    // helper to read
    let read_i32 = |pc: &mut usize| -> Result<i32, &'static str> {
        if *pc + 4 > code.len() {
            return Err("Unexpected EOF reading i32");
        }
        let v = (code[*pc] as i32)
            | ((code[*pc + 1] as i32) << 8)
            | ((code[*pc + 2] as i32) << 16)
            | ((code[*pc + 3] as i32) << 24);
        *pc += 4;
        Ok(v)
    };
    let read_u16 = |pc: &mut usize| -> Result<u16, &'static str> {
        if *pc + 2 > code.len() {
            return Err("Unexpected EOF reading u16");
        }
        let v = (code[*pc] as u16) | ((code[*pc + 1] as u16) << 8);
        *pc += 2;
        Ok(v)
    };
    let read_i16 = |pc: &mut usize| -> Result<i16, &'static str> {
        if *pc + 2 > code.len() {
            return Err("Unexpected EOF reading i16");
        }
        let v = (code[*pc] as u16) | ((code[*pc + 1] as u16) << 8);
        *pc += 2;
        Ok(v as i16)
    };

    // ensure Ctrl+C clears previous
    // caller should have cleared, but we check loop

    while pc < code.len() {
        if steps > MAX_STEPS {
            return Err("App exceeded step limit (infinite loop?)");
        }
        steps += 1;

        // cooperative yield check each 1024 steps + interrupt
        if steps % 1024 == 0 {
            crate::net::process_packets();
            if is_interrupted() {
                clear_interrupt();
                crate::println!("\n[app interrupted ^C]");
                return Err("Interrupted");
            }
            // small cooperative hint
            core::hint::spin_loop();
        }

        let op = code[pc];
        pc += 1;
        match op {
            opcode::HALT => {
                return Ok(0);
            }
            opcode::PUSH_IMM => {
                let v = read_i32(&mut pc)?;
                if stack.len() >= MAX_STACK {
                    return Err("Stack overflow");
                }
                stack.push(v as i64);
            }
            opcode::ADD => {
                if stack.len() < 2 {
                    return Err("Stack underflow ADD");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(a.wrapping_add(b));
            }
            opcode::SUB => {
                if stack.len() < 2 {
                    return Err("Stack underflow SUB");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(a.wrapping_sub(b));
            }
            opcode::MUL => {
                if stack.len() < 2 {
                    return Err("Stack underflow MUL");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(a.wrapping_mul(b));
            }
            opcode::DIV => {
                if stack.len() < 2 {
                    return Err("Stack underflow DIV");
                }
                let b = stack.pop().unwrap();
                if b == 0 {
                    return Err("Division by zero");
                }
                let a = stack.pop().unwrap();
                stack.push(a / b);
            }
            opcode::MOD => {
                if stack.len() < 2 {
                    return Err("Stack underflow MOD");
                }
                let b = stack.pop().unwrap();
                if b == 0 {
                    return Err("Division by zero");
                }
                let a = stack.pop().unwrap();
                stack.push(a % b);
            }
            opcode::EQ => {
                if stack.len() < 2 {
                    return Err("Stack underflow EQ");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(if a == b { 1 } else { 0 });
            }
            opcode::LT => {
                if stack.len() < 2 {
                    return Err("Stack underflow LT");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(if a < b { 1 } else { 0 });
            }
            opcode::GT => {
                if stack.len() < 2 {
                    return Err("Stack underflow GT");
                }
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(if a > b { 1 } else { 0 });
            }
            opcode::DUP => {
                if stack.is_empty() {
                    return Err("Stack underflow DUP");
                }
                if stack.len() >= MAX_STACK {
                    return Err("Stack overflow");
                }
                let v = *stack.last().unwrap();
                stack.push(v);
            }
            opcode::POP => {
                if stack.is_empty() {
                    return Err("Stack underflow POP");
                }
                stack.pop();
            }
            opcode::PRINT_INT => {
                if stack.is_empty() {
                    return Err("Stack underflow PRINT_INT");
                }
                let v = stack.pop().unwrap();
                crate::print!("{}", v);
            }
            opcode::PRINT_STR => {
                let len = read_u16(&mut pc)? as usize;
                if pc + len > code.len() {
                    return Err("PRINT_STR out of bounds");
                }
                let slice = &code[pc..pc + len];
                pc += len;
                if let Ok(s) = core::str::from_utf8(slice) {
                    crate::print!("{}", s);
                } else {
                    // binary print as hex?
                    for &b in slice {
                        crate::print!("{}", b as char);
                    }
                }
            }
            opcode::PRINT_NL => {
                crate::println!();
            }
            opcode::JMP => {
                let off = read_i16(&mut pc)? as isize;
                let new_pc = (pc as isize).wrapping_add(off as isize);
                if new_pc < 0 || new_pc as usize > code.len() {
                    return Err("JMP out of bounds");
                }
                pc = new_pc as usize;
            }
            opcode::JZ => {
                let off = read_i16(&mut pc)? as isize;
                if stack.is_empty() {
                    return Err("Stack underflow JZ");
                }
                let v = stack.pop().unwrap();
                if v == 0 {
                    let new_pc = (pc as isize).wrapping_add(off as isize);
                    if new_pc < 0 || new_pc as usize > code.len() {
                        return Err("JZ out of bounds");
                    }
                    pc = new_pc as usize;
                }
            }
            opcode::JNZ => {
                let off = read_i16(&mut pc)? as isize;
                if stack.is_empty() {
                    return Err("Stack underflow JNZ");
                }
                let v = stack.pop().unwrap();
                if v != 0 {
                    let new_pc = (pc as isize).wrapping_add(off as isize);
                    if new_pc < 0 || new_pc as usize > code.len() {
                        return Err("JNZ out of bounds");
                    }
                    pc = new_pc as usize;
                }
            }
            opcode::SLEEP => {
                let ms = read_u16(&mut pc)? as u64;
                let start = get_tick_count();
                // ticks approx 1000 per sec, but our tick is loop iterations ~ not real time
                // approximate: busy wait with packet processing
                while get_tick_count().wrapping_sub(start) < ms {
                    crate::net::process_packets();
                    if is_interrupted() {
                        clear_interrupt();
                        return Err("Interrupted");
                    }
                    for _ in 0..1000 {
                        core::hint::spin_loop();
                    }
                    // increment tick for editor-compat
                    crate::shell::increment_tick();
                }
            }
            opcode::YIELD => {
                crate::shell::increment_tick();
                crate::net::process_packets();
                // voluntary
                core::hint::spin_loop();
            }
            opcode::EXIT => {
                // EXIT pops exit code if stack not empty else 0 ; if next bytes look like i32, we handled PUSH before? For convenience support immediate: if stack empty, peek i32
                // We'll pop if available else 0
                let code = if !stack.is_empty() {
                    stack.pop().unwrap() as i32
                } else {
                    0
                };
                return Ok(code);
            }
            opcode::CALL => {
                if pc + 2 > code.len() {
                    return Err("CALL truncated");
                }
                let nr = code[pc];
                pc += 1;
                let _argc = code[pc];
                pc += 1;
                // dispatch - for now support few
                match nr {
                    x if x == crate::app::abi::SyscallNr::GetTick as u8 => {
                        if stack.len() >= MAX_STACK {
                            return Err("Stack overflow");
                        }
                        stack.push(get_tick_count() as i64);
                    }
                    x if x == crate::app::abi::SyscallNr::Yield as u8 => {
                        crate::shell::increment_tick();
                        crate::net::process_packets();
                    }
                    x if x == crate::app::abi::SyscallNr::Exit as u8 => {
                        let c = if !stack.is_empty() {
                            stack.pop().unwrap() as i32
                        } else {
                            0
                        };
                        return Ok(c);
                    }
                    x if x == crate::app::abi::SyscallNr::PrintInt as u8 => {
                        if stack.is_empty() {
                            return Err("CALL PrintInt needs arg");
                        }
                        let v = stack.pop().unwrap();
                        crate::print!("{}", v);
                    }
                    x if x == crate::app::abi::SyscallNr::PrintStr as u8 => {
                        // Not used in bytecode - PRINT_STR handles
                        return Err("CALL PrintStr not supported use PRINT_STR opcode");
                    }
                    _ => return Err("Unknown syscall in CALL"),
                }
            }
            _ => {
                return Err("Invalid opcode");
            }
        }
    }
    Ok(0)
}

/// Validate header without executing
pub fn validate_header(data: &[u8]) -> Result<MfkeHeader, &'static str> {
    if data.len() < core::mem::size_of::<MfkeHeader>() {
        return Err("File too small");
    }
    let h = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const MfkeHeader) };
    if !h.is_valid() {
        return Err("Bad MFKE header");
    }
    Ok(h)
}

/// Helper to build MFKE file in memory (for mkapp generator)
pub fn build_mfke(bytecode: &[u8]) -> Vec<u8> {
    let header = MfkeHeader {
        magic: MFKE_MAGIC,
        version: MFKE_VERSION,
        entry_offset: core::mem::size_of::<MfkeHeader>() as u32,
        bytecode_len: bytecode.len() as u32,
        mem_extra: 0,
        flags: 0,
        reserved: [0; 2],
    };
    let mut out = Vec::with_capacity(core::mem::size_of::<MfkeHeader>() + bytecode.len());
    let hdr_bytes = unsafe {
        core::slice::from_raw_parts(
            &header as *const _ as *const u8,
            core::mem::size_of::<MfkeHeader>(),
        )
    };
    out.extend_from_slice(hdr_bytes);
    out.extend_from_slice(bytecode);
    out
}

/// Convenience: encode i32 LE push
pub fn encode_push(v: i32) -> Vec<u8> {
    let mut b = Vec::new();
    b.push(opcode::PUSH_IMM);
    b.extend_from_slice(&v.to_le_bytes());
    b
}
