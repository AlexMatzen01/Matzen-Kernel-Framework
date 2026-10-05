// MFK platform layer for vendored doomgeneric (MIT, part of MFK).
//
// Shared declarations between the C platform files and the Rust kernel
// (kernel/src/doom/engine.rs provides the mfk_* exports).

#ifndef MFK_DEFS_H
#define MFK_DEFS_H

#include <stddef.h>
#include <stdint.h>

// ---- Rust exports (engine.rs) -------------------------------------------
// All paths are guest-absolute ("/wad/doom1.wad"); the FS must be mounted.

long mfk_fs_size(const char *path);                       // <0 on error
long mfk_fs_read(const char *path, unsigned long offset,  // bytes read, <0 err
                 void *buf, unsigned long len);
long mfk_fs_write(const char *path,                       // bytes written, <0 err
                  const void *buf, unsigned long len);
void mfk_debug_write(const char *buf, unsigned long len); // serial
unsigned long mfk_ticks_ms(void);                         // monotonic ms
void mfk_sleep_ms(unsigned long ms);                      // cooperative sleep
void *mfk_alloc(unsigned long size);                      // kernel heap
void *mfk_calloc(unsigned long n, unsigned long size);
void *mfk_realloc(void *ptr, unsigned long size);
void mfk_free(void *ptr);
char *mfk_strdup(const char *s);

// ---- Abort protocol ------------------------------------------------------
// I_Error / I_Quit / exit() funnel here. Uses __builtin longjmp back to
// the mfk_protect() trampoline so a dying engine can never trap the
// desktop loop. Must not cross Rust frames (all engine calls from Rust
// go through mfk_protect).
void mfk_abort(int code, const char *msg); // code 0 = clean quit
int mfk_protect(void (*fn)(void *), void *arg); // 0 ok, 1 aborted
int mfk_abort_code(void);                  // valid after abort
const char *mfk_abort_msg(void);

// ---- Key queue + WAD path (owned by Rust, kernel/src/doom/engine.rs) ----
// The engine drains input via mfk_pop_key (called from DG_GetKey) and
// resolves the IWAD via mfk_wad_path. Declared here for the C files;
// defined on the Rust side and linked in.
int mfk_pop_key(int *pressed, unsigned char *key); // 1 = event
const char *mfk_wad_path(void);

#endif
