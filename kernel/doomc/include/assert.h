// MFK freestanding assert.h (MIT).
#ifndef MFK_ASSERT_H
#define MFK_ASSERT_H

void mfk_assert_fail(const char *file, int line, const char *expr);

#ifdef NDEBUG
#define assert(e) ((void)0)
#else
#define assert(e) ((e) ? (void)0 : mfk_assert_fail(__FILE__, __LINE__, #e))
#endif

#endif
