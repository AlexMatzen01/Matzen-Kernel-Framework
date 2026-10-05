# Vendored doomgeneric (GPL)

Upstream: https://github.com/ozkl/doomgeneric (linuxdoom-derived Doom port
with a `DG_*` platform abstraction).

## License notice

These sources are **GNU General Public License** (see `LICENSE` in the
upstream repo / file headers: GPL-2.0-or-later for the Chocolate-Doom
derived files). They are intentionally kept verbatim under `vendor/` and
are **not** covered by MFK's MIT license. MFK-specific platform code
lives in `kernel/doomc/` (MIT, same as the kernel).

## What was vendored

All `doomgeneric/*.c` + `*.h` except platform backends MFK replaces:

- excluded: `doomgeneric_sdl.c`, `doomgeneric_win.c`,
  `doomgeneric_xlib.c`, `doomgeneric_emscripten.c`,
  `doomgeneric_allegro.c`, `doomgeneric_linuxvt.c`,
  `doomgeneric_soso.c`, `doomgeneric_sosox.c` (other platforms),
  `i_sdlsound.c`, `i_sdlmusic.c`, `i_allegrosound.c`,
  `i_allegromusic.c`, `i_cdmus.c`, `mus2mid.c` (sound backends),
  `net_sdl.c` (SDL_net; other net_*.c kept, single-player only),
  `i_endoom.c` (text-mode ENDOOM screen; stubbed),
  `i_joystick.c` (Linux joystick; stubbed),
  `i_system.c`, `i_sound.c`, `w_file_stdc.c`, `d_iwad.c`
  (replaced by `kernel/doomc/mfk_*.c`).

Kept: `Makefile`, `Makefile.sdl` (reference for the canonical file list),
`config.h` (as upstream ships it), and `net_sdl.h` (declarations only;
its multiplayer users are compiled out via `#undef FEATURE_MULTIPLAYER`
in `doomfeatures.h`, so no SDL_net code links in).

Full vendored file list: `SOURCES.txt`.
MFK build: `kernel/build.rs` compiles the kept files with
`-DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200` (matches
`kernel/src/doom` framebuffer constants) plus the `kernel/doomc/`
platform layer, via clang targeting freestanding x86-64 with soft-float
flags matching `targets/x86_64-mfk.json`.
