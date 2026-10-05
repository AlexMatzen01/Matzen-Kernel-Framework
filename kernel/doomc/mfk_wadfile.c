// MFK WAD file backend: implements the upstream `stdc_wad_file` class
// (w_file.c references it) on top of the SimplFS range API. No stdio,
// no mmap, no heap copy of the whole WAD. MIT, part of MFK.

#include <stddef.h>
#include "doomtype.h"
#include "m_argv.h"
#include "w_file.h"
#include "z_zone.h"
#include "mfk_defs.h"

typedef struct {
    wad_file_t head;
    char path[128];
} mfk_wad_t;

static void path_copy(char *dst, const char *src)
{
    unsigned i = 0;
    while (i + 1 < 128 && src[i]) {
        dst[i] = src[i];
        ++i;
    }
    dst[i] = '\0';
}

static wad_file_t *MfkOpen(char *path)
{
    mfk_wad_t *w;
    long sz;
    if (!path)
        return 0;
    sz = mfk_fs_size(path);
    if (sz < 0)
        return 0;
    w = (mfk_wad_t *)mfk_alloc(sizeof(mfk_wad_t));
    if (!w)
        return 0;
    extern wad_file_class_t stdc_wad_file;
    w->head.file_class = &stdc_wad_file;
    w->head.mapped = 0;
    w->head.length = (unsigned int)sz;
    path_copy(w->path, path);
    return &w->head;
}

static void MfkClose(wad_file_t *file)
{
    mfk_free(file);
}

static size_t MfkRead(wad_file_t *file, unsigned int offset, void *buffer,
                      size_t buffer_len)
{
    mfk_wad_t *w = (mfk_wad_t *)file;
    long want = (long)buffer_len, r;
    if ((long)offset >= (long)w->head.length)
        return 0;
    if ((long)offset + want > (long)w->head.length)
        want = (long)w->head.length - (long)offset;
    if (want <= 0)
        return 0;
    r = mfk_fs_read(w->path, offset, buffer, (unsigned long)want);
    return r < 0 ? 0 : (size_t)r;
}

wad_file_class_t stdc_wad_file = { MfkOpen, MfkClose, MfkRead };
