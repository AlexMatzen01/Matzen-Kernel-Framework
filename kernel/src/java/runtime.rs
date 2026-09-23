//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! In-kernel Java execution entry point.
//!
//! This validates the `.class` header, enforces the Java 8 baseline, and
//! executes the bounded interpreter supported by the current runtime.
//!
//! Cooperative like the MFKE VM: checks Ctrl+C and pumps the network
//! so `java` never wedges the shell loop.

use super::class;
use super::classfile;
use super::interpreter;
use super::version;
use crate::shell::{clear_interrupt, is_interrupted};

/// Validate and execute a Java class.
///
/// `data` is the raw `.class` bytes, `path` the guest path for messages,
/// `args` the app argv (`args[0]` is conventionally the class/path).
pub fn run_class(data: &[u8], path: &str, args: &[&str]) -> Result<i32, &'static str> {
    if data.is_empty() {
        return Err("Empty class file");
    }
    let info = class::parse_header(data)?;

    if is_interrupted() {
        clear_interrupt();
        crate::println!("\n[java interrupted ^C]");
        return Err("Interrupted");
    }
    // Cooperative tick + net pump, mirroring loader.rs.
    crate::shell::increment_tick();
    crate::net::process_packets();

    let release = info.release().unwrap_or(0);
    if !info.is_supported() {
        if release != 0 {
            crate::println!(
                "[java] '{}' is class {}.{} (Java {})",
                path,
                info.major,
                info.minor,
                release
            );
        } else {
            crate::println!(
                "[java] '{}' is class {}.{} (unknown release)",
                path,
                info.major,
                info.minor
            );
        }
        // Print without nested-format tricks so no_std stays simple.
        crate::println!(
            "Exception in thread \"main\" java.lang.UnsupportedClassVersionError: {}.{}",
            info.major,
            info.minor
        );
        if release != 0 {
            crate::println!(
                "  Class was compiled for Java {} but mfk-jvm supports Java {} (class {}.0).",
                release,
                version::COMPAT_RELEASE,
                version::COMPAT_MAJOR
            );
            crate::println!(
                "  Recompile on the host with: javac --release {} <sources>",
                version::COMPAT_RELEASE
            );
        } else {
            crate::println!(
                "  Unknown class version; mfk-jvm supports up to class {}.0 (Java {}).",
                version::SUPPORTED_MAX_MAJOR,
                version::COMPAT_RELEASE
            );
        }
        return Err("UnsupportedClassVersionError");
    }

    let parsed = classfile::parse(data)?;
    let class_name = parsed.class_name(parsed.this_class)?;
    crate::println!(
        "[java] '{}' class {}.{} (Java 8 baseline, {} bytes)",
        path,
        info.major,
        info.minor,
        data.len()
    );
    if args.len() > 1 {
        crate::print!("[java] args:");
        for a in &args[1..] {
            crate::print!(" {}", a);
        }
        crate::println!();
    }
    crate::println!("[java] main class: {}", class_name);
    let code = interpreter::execute_main(&parsed, &args[1..])?;
    Ok(code)
}

/// One-line `java -version` payload shared by shell and `appinfo`.
pub fn version_banner() -> &'static str {
    "mfk-jvm 0.1.0 (compat Java 8, class 52.0)"
}
