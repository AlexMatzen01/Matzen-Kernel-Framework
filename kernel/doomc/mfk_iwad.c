// MFK IWAD resolver replacing upstream d_iwad.c (MIT, part of MFK).
//
// No directory scanning on MFK: the single active WAD comes from the
// desktop/shell (mfk_set_wad_path), defaulting to the automated setup
// guest path /wad/doom1.wad.

#include <stddef.h>
#include <string.h>
#include "d_iwad.h"
#include "d_mode.h"
#include "doomtype.h"
#include "m_misc.h"
#include "z_zone.h"
#include "mfk_defs.h"

static int wad_matches(const char *have, const char *want)
{
    // Case-insensitive basename comparison so "-iwad DOOM2.WAD"
    // still resolves when the user bundled doom2.wad.
    const char *hb = have, *wb = want, *p;
    for (p = have; *p; ++p) {
        if (*p == '/' || *p == '\\')
            hb = p + 1;
    }
    for (p = want; *p; ++p) {
        if (*p == '/' || *p == '\\')
            wb = p + 1;
    }
    while (*hb && *wb) {
        int a = *hb++, b = *wb++;
        if (a >= 'A' && a <= 'Z')
            a += 32;
        if (b >= 'A' && b <= 'Z')
            b += 32;
        if (a != b)
            return 0;
    }
    return *hb == *wb;
}

char *D_FindWADByName(char *filename)
{
    const char *wad = mfk_wad_path();
    if (wad_matches(wad, filename))
        return M_StringDuplicate(wad);
    return NULL;
}

char *D_TryFindWADByName(char *filename)
{
    return D_FindWADByName(filename);
}

static GameMission_t mission_for_basename(const char *base)
{
    char lower[32];
    unsigned i = 0;
    while (i + 1 < sizeof(lower) && base[i]) {
        char c = base[i];
        lower[i++] = (char)((c >= 'A' && c <= 'Z') ? c + 32 : c);
    }
    lower[i] = '\0';
    if (!strcmp(lower, "doom2.wad") || !strcmp(lower, "doom2f.wad"))
        return doom2;
    if (!strcmp(lower, "tnt.wad"))
        return pack_tnt;
    if (!strcmp(lower, "plutonia.wad"))
        return pack_plut;
    if (!strcmp(lower, "chex.wad") || !strcmp(lower, "chex3.wad"))
        return pack_chex;
    if (!strcmp(lower, "hacx.wad"))
        return pack_hacx;
    if (!strcmp(lower, "heretic.wad") || !strcmp(lower, "hexen.wad") ||
        !strcmp(lower, "strife.wad") || !strcmp(lower, "strife1.wad"))
        return none; // different games: this engine cannot run them
    return doom;
}

char *D_FindIWAD(int mask, GameMission_t *mission)
{
    const char *wad = mfk_wad_path();
    const char *base = wad;
    const char *p;
    GameMission_t m;
    (void)mask;
    if (mfk_fs_size(wad) < 0)
        return NULL;
    for (p = wad; *p; ++p) {
        if (*p == '/' || *p == '\\')
            base = p + 1;
    }
    m = mission_for_basename(base);
    if (m == none)
        return NULL;
    if (mission)
        *mission = m;
    return M_StringDuplicate(wad);
}

static const iwad_t *no_iwads[] = { NULL };

const iwad_t **D_FindAllIWADs(int mask)
{
    (void)mask;
    return no_iwads;
}

char *D_SaveGameIWADName(GameMission_t gamemission)
{
    (void)gamemission;
    return M_StringDuplicate(mfk_wad_path());
}

char *D_SuggestIWADName(GameMission_t mission, GameMode_t mode)
{
    (void)mission;
    (void)mode;
    return M_StringDuplicate("doom1.wad");
}

char *D_SuggestGameName(GameMission_t mission, GameMode_t mode)
{
    (void)mission;
    (void)mode;
    return M_StringDuplicate("doom");
}

void D_CheckCorrectIWAD(GameMission_t mission)
{
    (void)mission;
}
