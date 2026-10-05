// MFK platform layer: DG_* implementation, abort trampoline, I_* stubs
// replacing upstream i_system.c / i_sound.c / i_joystick.c / i_endoom.c.
// MIT, part of MFK.

#include <stdio.h>
#include <string.h>
#include "mfk_defs.h"
#include "i_video.h"
#include "doomgeneric.h"
#include "doomtype.h"
#include "i_sound.h"
#include "i_system.h"
#include "i_video.h"
#include "m_argv.h"
#include "d_mode.h"

// ---- Abort trampoline ----------------------------------------------------
static void *abort_jmp_env[5]; // __builtin_jmp_buf (opaque, 5 words on x86-64)
static int abort_code_store = 0;
static char abort_msg_store[256];

int mfk_abort_code(void) { return abort_code_store; }
const char *mfk_abort_msg(void) { return abort_msg_store; }

void mfk_abort(int code, const char *msg)
{
    abort_code_store = code;
    if (msg) {
        unsigned i = 0;
        while (i + 1 < sizeof(abort_msg_store) && msg[i]) {
            abort_msg_store[i] = msg[i];
            ++i;
        }
        abort_msg_store[i] = '\0';
    } else {
        abort_msg_store[0] = '\0';
    }
    __builtin_longjmp(abort_jmp_env, 1);
}

int mfk_protect(void (*fn)(void *), void *arg)
{
    abort_code_store = 0;
    abort_msg_store[0] = '\0';
    if (__builtin_setjmp(abort_jmp_env))
        return 1;
    fn(arg);
    return 0;
}

// ---- DG_* (doomgeneric platform API) -------------------------------------
// NOTE: the key queue (mfk_pop_key) and WAD path (mfk_wad_path) live in
// Rust (kernel/src/doom/engine.rs) and are linked in from there.
void DG_Init(void) { /* framebuffer allocated by doomgeneric_Create */ }

void DG_DrawFrame(void)
{
    // The engine rendered into DG_ScreenBuffer (320x200x32). The Rust
    // desktop loop copies it out after each tick; nothing to do here.
}

void DG_SleepMs(uint32_t ms) { mfk_sleep_ms((unsigned long)ms); }
uint32_t DG_GetTicksMs(void) { return (uint32_t)mfk_ticks_ms(); }

int DG_GetKey(int *pressed, unsigned char *key)
{
    return mfk_pop_key(pressed, key);
}

void DG_SetWindowTitle(const char *title) { (void)title; }

// ---- Entry wrappers called through mfk_protect ---------------------------
static int create_argc_store;
static char **create_argv_store;

static void create_inner(void *arg)
{
    (void)arg;
    doomgeneric_Create(create_argc_store, create_argv_store);
}

int mfk_engine_create(int argc, char **argv)
{
    create_argc_store = argc;
    create_argv_store = argv;
    return mfk_protect(create_inner, 0);
}

static void tick_inner(void *arg)
{
    (void)arg;
    doomgeneric_Tick();
}

int mfk_engine_tick(void) { return mfk_protect(tick_inner, 0); }

// ---- i_system replacements -----------------------------------------------
byte *I_ZoneBase(int *size)
{
    // Doom zone needs a few MB; 16MB from the kernel heap. Freed never
    // (engine lifetime == window lifetime).
    *size = 16 * 1024 * 1024;
    return (byte *)mfk_alloc((unsigned long)*size);
}

void I_PrintBanner(char *msg)
{
    mfk_debug_write(msg, __builtin_strlen(msg));
    mfk_debug_write("\n", 1);
}

void I_Tactile(int on, int off, int total)
{
    (void)on;
    (void)off;
    (void)total;
}

void I_AtExit(atexit_func_t func, boolean run_on_error)
{
    (void)func;
    (void)run_on_error; // no exit handlers: quits funnel via mfk_abort
}

// Sound device ids (silent v1: no devices present).
int snd_sfxdevice = 0;
int snd_musicdevice = 0;

void I_PrintDivider(void) {}
void I_PrintStartupBanner(char *gamedescription) { I_PrintBanner(gamedescription); }
boolean I_ConsoleStdout(void) { return true; }

void I_Init(void) {}
void I_BindVariables(void) {}

void I_Quit(void) { mfk_abort(0, "quit"); }

void I_Error(char *error, ...)
{
    char buf[256];
    __builtin_va_list ap;
    __builtin_va_start(ap, error);
    // Minimal vsnprintf is provided by mfk_libc.c.
    vsnprintf(buf, sizeof(buf), error, ap);
    __builtin_va_end(ap);
    mfk_debug_write("DOOM ERROR: ", 12);
    mfk_debug_write(buf, __builtin_strlen(buf));
    mfk_debug_write("\n", 1);
    mfk_abort(1, buf);
}

boolean I_GetMemoryValue(unsigned int offset, void *value, int size)
{
    (void)offset;
    (void)value;
    (void)size;
    return false;
}

void I_Endoom(byte *endoom) { (void)endoom; }
void mfk_assert_fail(const char *file, int line, const char *expr)
{
    char buf[192];
    snprintf(buf, sizeof(buf), "assert %s:%d: %s", file, line, expr);
    mfk_debug_write("DOOM ASSERT: ", 13);
    mfk_debug_write(buf, __builtin_strlen(buf));
    mfk_debug_write("\n", 1);
    mfk_abort(1, buf);
}

void I_InitJoystick(void) {}
void I_ShutdownJoystick(void) {}
void I_UpdateJoystick(void) {}
void I_BindJoystickVariables(void) {}

// exit/atexit: route through the abort protocol.
int atexit(void (*fn)(void)) { (void)fn; return 0; }
void exit(int code) { mfk_abort(code == 0 ? 0 : 1, code == 0 ? "exit" : "exit(error)"); }
void _exit(int code) { mfk_abort(code == 0 ? 0 : 1, "exit"); }

// ---- Silent sound (v1): full i_sound API, no backend ---------------------
void I_InitSound(boolean use_sfx_prefix) { (void)use_sfx_prefix; }
void I_ShutdownSound(void) {}
int I_GetSfxLumpNum(sfxinfo_t *sfxinfo) { (void)sfxinfo; return 0; }
void I_UpdateSound(void) {}
void I_UpdateSoundParams(int channel, int vol, int sep)
{
    (void)channel;
    (void)vol;
    (void)sep;
}
int I_StartSound(sfxinfo_t *sfxinfo, int channel, int vol, int sep)
{
    (void)sfxinfo;
    (void)vol;
    (void)sep;
    return channel;
}
void I_StopSound(int channel) { (void)channel; }
boolean I_SoundIsPlaying(int channel) { (void)channel; return false; }
void I_PrecacheSounds(sfxinfo_t *sounds, int num_sounds)
{
    (void)sounds;
    (void)num_sounds;
}
void I_InitMusic(void) {}
void I_ShutdownMusic(void) {}
void I_SetMusicVolume(int volume) { (void)volume; }
void I_PauseSong(void) {}
void I_ResumeSong(void) {}
void *I_RegisterSong(void *data, int len) { (void)data; (void)len; return 0; }
void I_UnRegisterSong(void *handle) { (void)handle; }
void I_PlaySong(void *handle, boolean looping)
{
    (void)handle;
    (void)looping;
}
void I_StopSong(void) {}
boolean I_MusicIsPlaying(void) { return false; }
void I_BindSoundVariables(void) {}
