//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! In-kernel Java support (JDK/JVM with versions).
//!
//! - `version` — release <-> class-major registry (Java 8 baseline).
//! - `class`   — `.class` header parser (CAFEBABE + major.minor).
//! - `runtime` — validation + execution entry (`run_class`).
//!
//! `no_std` + `alloc` compatible. Full interpreter/natives/GC build here.

pub mod class;
pub mod classfile;
pub mod interpreter;
pub mod runtime;
pub mod version;
