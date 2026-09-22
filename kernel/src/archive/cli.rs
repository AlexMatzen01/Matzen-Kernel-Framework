//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Pure command-line parsing for `tar` / `zip` / `unzip` (`no_std`).
//!
//! Kept free of filesystem access so the host test harness
//! (`archtest`) can exercise every flag combination: the kernel shell
//! (`crate::shell`) only executes what these parsers accept.

use alloc::string::String;
use alloc::vec::Vec;

/// `tar` operation mode (`-c` / `-t` / `-x`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarMode {
    Create,
    List,
    Extract,
}

/// Forced compression on `tar -c` (default: by archive extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarCompress {
    Gzip,
    Xz,
}

/// Parsed `tar` invocation.
#[derive(Debug, Clone)]
pub struct TarArgs {
    pub mode: TarMode,
    pub verbose: bool,
    pub force: Option<TarCompress>,
    pub archive: String,
    pub operands: Vec<String>,
    /// `-C DIR` (extract only).
    pub dest: Option<String>,
}

fn is_flag(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// Parse `tar` arguments (everything after the `tar ` verb).
///
/// Accepts combined clusters (`-czf`, `-tvf`, `-xvf`) with `-f`/`-C`
/// values attached (`-cfout.tar`) or separate (`-cf out.tar`), plus a
/// trailing `-C DIR` for `-x`. Exactly one of `-c`/`-t`/`-x` is required.
pub fn parse_tar(args: &str) -> Result<TarArgs, &'static str> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() {
        return Err("need one of -c/-t/-x");
    }
    let cluster = tokens[0];
    if !is_flag(cluster) {
        return Err("expected flags like -cf, -tf, -xf");
    }
    let mut mode: Option<TarMode> = None;
    let mut verbose = false;
    let mut force: Option<TarCompress> = None;
    let mut archive: Option<String> = None;
    let mut dest: Option<String> = None;
    let mut i = 1usize;
    // `f`/`C` consume the rest of the cluster as an attached value.
    let mut pending: Option<u8> = None; // b'f' | b'C'
    let mut attached = String::new();
    for ch in cluster[1..].chars() {
        if pending.is_some() {
            attached.push(ch);
            continue;
        }
        match ch {
            'c' | 't' | 'x' => {
                if mode.is_some() {
                    return Err("only one of -c/-t/-x");
                }
                mode = Some(match ch {
                    'c' => TarMode::Create,
                    't' => TarMode::List,
                    _ => TarMode::Extract,
                });
            }
            'v' => verbose = true,
            'z' | 'j' | 'J' => {
                if force.is_some() {
                    return Err("-z and -j are exclusive");
                }
                force = Some(if ch == 'z' { TarCompress::Gzip } else { TarCompress::Xz });
            }
            'f' => pending = Some(b'f'),
            'C' => pending = Some(b'C'),
            _ => return Err("unknown flag"),
        }
    }
    if let Some(p) = pending {
        if !attached.is_empty() {
            if p == b'f' {
                archive = Some(attached);
            } else {
                dest = Some(attached);
            }
        } else {
            if i >= tokens.len() {
                return Err(if p == b'f' { "-f needs an archive file" } else { "-C needs a directory" });
            }
            if p == b'f' {
                archive = Some(String::from(tokens[i]));
            } else {
                dest = Some(String::from(tokens[i]));
            }
            i += 1;
        }
    }
    let mode = mode.ok_or("need one of -c/-t/-x")?;
    let mut archive = archive;
    let mut operands: Vec<String> = Vec::new();
    while i < tokens.len() {
        if tokens[i] == "-C" || tokens[i].starts_with("-C") && tokens[i].len() > 2 {
            if mode != TarMode::Extract {
                return Err("-C is only valid with -x");
            }
            if dest.is_some() {
                return Err("duplicate -C");
            }
            if tokens[i] == "-C" {
                i += 1;
                if i >= tokens.len() {
                    return Err("-C needs a directory");
                }
                dest = Some(String::from(tokens[i]));
                i += 1;
            } else {
                dest = Some(String::from(&tokens[i][2..]));
                i += 1;
            }
        } else if tokens[i] == "-f" || tokens[i].starts_with("-f") && tokens[i].len() > 2 {
            // Split form: `tar -c -f out.tar ...` (the value may also be
            // attached: `-fout.tar`). Mode letters stay in the cluster.
            if archive.is_some() {
                return Err("duplicate -f");
            }
            if tokens[i].len() > 2 {
                archive = Some(String::from(&tokens[i][2..]));
                i += 1;
            } else {
                i += 1;
                if i >= tokens.len() {
                    return Err("-f needs an archive file");
                }
                archive = Some(String::from(tokens[i]));
                i += 1;
            }
        } else if is_flag(tokens[i]) {
            // No other flags are valid after the archive (file names
            // starting with '-' are unsupported; rename them first).
            return Err("unexpected flag");
        } else {
            operands.push(String::from(tokens[i]));
            i += 1;
        }
    }
    let archive = archive.ok_or("-f needs an archive file")?;
    if mode == TarMode::Create && operands.is_empty() {
        return Err("nothing to archive");
    }
    if mode != TarMode::Extract && dest.is_some() {
        return Err("-C is only valid with -x");
    }
    Ok(TarArgs { mode, verbose, force, archive, operands, dest })
}

/// Parsed `zip` invocation.
#[derive(Debug, Clone)]
pub struct ZipArgs {
    /// True with `-0` (stored); default is deflated.
    pub stored: bool,
    pub archive: String,
    pub operands: Vec<String>,
}

/// Parse `zip` arguments: `zip [-0|-6|-9] <archive.zip> <FILE...>`.
pub fn parse_zip(args: &str) -> Result<ZipArgs, &'static str> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0usize;
    let mut stored = false;
    while i < tokens.len() && is_flag(tokens[i]) {
        match tokens[i] {
            "-0" => stored = true,
            "-6" | "-9" => stored = false,
            _ => return Err("unknown flag"),
        }
        i += 1;
    }
    if i >= tokens.len() {
        return Err("missing archive file");
    }
    let archive = String::from(tokens[i]);
    i += 1;
    if i >= tokens.len() {
        return Err("nothing to archive");
    }
    Ok(ZipArgs { stored, archive, operands: tokens[i..].iter().map(|s| String::from(*s)).collect() })
}

/// Parsed `unzip` invocation.
#[derive(Debug, Clone)]
pub struct UnzipArgs {
    pub list_only: bool,
    pub archive: String,
    /// `-d DIR` (extract only).
    pub dest: Option<String>,
}

/// Parse `unzip` arguments: `unzip [-l] <archive.zip> [-d DIR]`.
pub fn parse_unzip(args: &str) -> Result<UnzipArgs, &'static str> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0usize;
    let mut list_only = false;
    while i < tokens.len() && is_flag(tokens[i]) {
        match tokens[i] {
            "-l" => list_only = true,
            _ => return Err("unknown flag"),
        }
        i += 1;
    }
    if i >= tokens.len() {
        return Err("missing archive file");
    }
    let archive = String::from(tokens[i]);
    i += 1;
    let mut dest: Option<String> = None;
    while i < tokens.len() {
        if tokens[i] == "-d" {
            if list_only {
                return Err("-d is only valid when extracting");
            }
            if dest.is_some() {
                return Err("duplicate -d");
            }
            i += 1;
            if i >= tokens.len() {
                return Err("-d needs a directory");
            }
            dest = Some(String::from(tokens[i]));
            i += 1;
        } else {
            return Err("unexpected argument");
        }
    }
    Ok(UnzipArgs { list_only, archive, dest })
}
