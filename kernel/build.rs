//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Build script: compiles vendored doomgeneric + the MFK platform layer
//! (kernel/doomc) with clang for the freestanding x86-64 MFK target.
//!
//! - Only runs when TARGET contains "mfk" (host `cargo test` skips it).
//! - Needs `clang` on PATH (scoop/llvm). Without it, the kernel still
//!   builds; `engine.rs` falls back to `Err("engine not compiled")`
//!   via the missing `doomc_built` cfg.
//! - Upstream platform files replaced by kernel/doomc/mfk_*.c are listed
//!   in EXCLUDED (see vendor/doomgeneric/VENDOR.md).

use std::env;
use std::path::PathBuf;
use std::process::Command;

// Canonical doomgeneric file list (upstream Makefile.sdl) minus the
// platform/sound backends MFK replaces (see EXCLUDED).
const EXCLUDED: &[&str] = &[
    "doomgeneric_sdl.c",
    "i_sdlsound.c",
    "i_sdlmusic.c",
    "mus2mid.c",
    "i_cdmus.c",
    "i_endoom.c",
    "i_joystick.c",
    "i_system.c",
    "i_sound.c",
    "w_file_stdc.c",
    "d_iwad.c",
];

const UPSTREAM: &[&str] = &[
    "dummy.c",
    "am_map.c",
    "doomdef.c",
    "doomstat.c",
    "dstrings.c",
    "d_event.c",
    "d_items.c",
    "d_loop.c",
    "d_main.c",
    "d_mode.c",
    "d_net.c",
    "f_finale.c",
    "f_wipe.c",
    "g_game.c",
    "hu_lib.c",
    "hu_stuff.c",
    "info.c",
    "i_scale.c",
    "i_timer.c",
    "memio.c",
    "m_argv.c",
    "m_bbox.c",
    "m_cheat.c",
    "m_config.c",
    "m_controls.c",
    "m_fixed.c",
    "m_menu.c",
    "m_misc.c",
    "m_random.c",
    "p_ceilng.c",
    "p_doors.c",
    "p_enemy.c",
    "p_floor.c",
    "p_inter.c",
    "p_lights.c",
    "p_map.c",
    "p_maputl.c",
    "p_mobj.c",
    "p_plats.c",
    "p_pspr.c",
    "p_saveg.c",
    "p_setup.c",
    "p_sight.c",
    "p_spec.c",
    "p_switch.c",
    "p_telept.c",
    "p_tick.c",
    "p_user.c",
    "r_bsp.c",
    "r_data.c",
    "r_draw.c",
    "r_main.c",
    "r_plane.c",
    "r_segs.c",
    "r_sky.c",
    "r_things.c",
    "sha1.c",
    "sounds.c",
    "statdump.c",
    "st_lib.c",
    "st_stuff.c",
    "s_sound.c",
    "tables.c",
    "v_video.c",
    "wi_stuff.c",
    "w_checksum.c",
    "w_file.c",
    "w_main.c",
    "w_wad.c",
    "z_zone.c",
    "i_input.c",
    "i_video.c",
    "doomgeneric.c",
];

const MFK_PLATFORM: &[&str] = &[
    "mfk_platform.c",
    "mfk_wadfile.c",
    "mfk_iwad.c",
    "mfk_libc.c",
    "mfk_stdio.c",
];

/// Pick a C compiler: clang (with an explicit freestanding target) when
/// available, else cc/gcc (native x86_64 + -ffreestanding).
fn pick_compiler() -> Option<(&'static str, bool)> {
    for (cc, is_clang) in [("clang", true), ("cc", false), ("gcc", false)] {
        if Command::new(cc)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some((cc, is_clang));
        }
    }
    None
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(doomc_built)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=doomc/");
    println!("cargo:rerun-if-changed=../vendor/doomgeneric/");
    // Mouse cursors: every image in cursors/ is bundled into
    // src/desktop/cursor_data.rs via tools/convert_cursor.py. Rebuild when
    // the set changes (a missing re-run only warns; see below).
    println!("cargo:rerun-if-changed=../cursors/");
    println!("cargo:rerun-if-changed=src/desktop/cursor_data.rs");
    println!("cargo:rerun-if-changed=../tools/convert_cursor.py");

    // Warn when cursors/ changed without re-running the bundler script, so
    // a newly added cursor image is not silently missing from the kernel.
    {
        let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let generated = manifest.join("src/desktop/cursor_data.rs");
        let dirs = manifest.join("../cursors");
        let stale = match (
            std::fs::metadata(&generated).and_then(|m| m.modified()),
            std::fs::read_dir(&dirs),
        ) {
            (Ok(gen_time), Ok(rd)) => rd.filter_map(|e| e.ok()).any(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| {
                        matches!(
                            x.to_ascii_lowercase().as_str(),
                            "png"
                                | "jpg"
                                | "jpeg"
                                | "bmp"
                                | "gif"
                                | "webp"
                                | "ico"
                                | "cur"
                                | "hotspot"
                        )
                    })
                    .unwrap_or(false)
                    && std::fs::metadata(e.path())
                        .and_then(|m| m.modified())
                        .map(|t| t > gen_time)
                        .unwrap_or(false)
            }),
            _ => false,
        };
        if stale {
            println!(
                "cargo:warning=cursors: cursors/ is newer than cursor_data.rs; run `python3 tools/convert_cursor.py` to bundle new cursor images"
            );
        }
    }

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("mfk") {
        return; // host build (cargo test): no C engine
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("../vendor/doomgeneric");
    let doomc = manifest.join("doomc");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut sources: Vec<PathBuf> = Vec::new();
    for name in UPSTREAM {
        if EXCLUDED.iter().any(|e| e == name) {
            continue;
        }
        let p = vendor.join(name);
        if p.is_file() {
            sources.push(p);
        } else {
            println!("cargo:warning=doomc: upstream file missing: {}", name);
        }
    }
    for name in MFK_PLATFORM {
        sources.push(doomc.join(name));
    }

    let Some((cc, is_clang)) = pick_compiler() else {
        println!("cargo:warning=doomc: no C compiler (clang/cc/gcc); engine disabled");
        return;
    };

    let mut objects: Vec<PathBuf> = Vec::new();
    for src in &sources {
        let stem = src.file_stem().unwrap().to_string_lossy();
        let obj = out.join(format!("{}.o", stem));
        let mut cmd = Command::new(cc);
        if is_clang {
            cmd.arg("--target=x86_64-unknown-none-elf");
        }
        let status = cmd
            .arg("-ffreestanding")
            .arg("-fno-builtin")
            .arg("-fno-stack-protector")
            .arg("-mno-red-zone")
            .arg("-fPIC")
            .arg("-O2")
            .arg("-w")
            .arg("-std=gnu11")
            .arg("-DDOOMGENERIC_RESX=320")
            .arg("-DDOOMGENERIC_RESY=200")
            .arg("-I")
            .arg(vendor.as_os_str())
            .arg("-I")
            .arg(doomc.join("include").as_os_str())
            .arg("-I")
            .arg(doomc.as_os_str())
            .arg("-c")
            .arg(src.as_os_str())
            .arg("-o")
            .arg(obj.as_os_str())
            .status();
        match status {
            Ok(s) if s.success() => objects.push(obj),
            _ => {
                println!("cargo:warning=doomc: {} failed on {}; engine disabled", cc, stem);
                return; // kernel still builds; engine.rs stub takes over
            }
        }
    }

    for obj in &objects {
        println!("cargo:rustc-link-arg={}", obj.display());
    }
    println!("cargo:rustc-cfg=doomc_built");
    println!(
        "cargo:warning=doomc: engine compiled ({} objects, {})",
        objects.len(),
        cc
    );
}
