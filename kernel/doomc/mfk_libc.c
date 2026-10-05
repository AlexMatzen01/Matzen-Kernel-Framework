// MFK freestanding libc core for vendored doomgeneric (MIT).
// Memory/string/ctype/stdlib basics. printf-family lives in mfk_stdio.c
// but shares fmt_signed/fmt_unsigned helpers declared here.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <sys/stat.h>
#include <sys/types.h>
#include "mfk_defs.h"

int errno = 0;

// ---- memory ---------------------------------------------------------------
void *memcpy(void *dst, const void *src, size_t n)
{
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    while (n--)
        *d++ = *s++;
    return dst;
}

void *memmove(void *dst, const void *src, size_t n)
{
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    if (d < s) {
        while (n--)
            *d++ = *s++;
    } else if (d > s) {
        d += n;
        s += n;
        while (n--)
            *--d = *--s;
    }
    return dst;
}

void *memset(void *s, int c, size_t n)
{
    uint8_t *p = (uint8_t *)s;
    while (n--)
        *p++ = (uint8_t)c;
    return s;
}

int memcmp(const void *a, const void *b, size_t n)
{
    const uint8_t *p = (const uint8_t *)a, *q = (const uint8_t *)b;
    while (n--) {
        if (*p != *q)
            return *p - *q;
        ++p;
        ++q;
    }
    return 0;
}

void *memchr(const void *s, int c, size_t n)
{
    const uint8_t *p = (const uint8_t *)s;
    while (n--) {
        if (*p == (uint8_t)c)
            return (void *)p;
        ++p;
    }
    return 0;
}

// ---- strings --------------------------------------------------------------
size_t strlen(const char *s)
{
    size_t n = 0;
    while (s[n])
        ++n;
    return n;
}

char *strcpy(char *dst, const char *src)
{
    char *d = dst;
    while ((*d++ = *src++))
        ;
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i = 0;
    while (i < n && src[i]) {
        dst[i] = src[i];
        ++i;
    }
    while (i < n)
        dst[i++] = '\0';
    return dst;
}

char *strcat(char *dst, const char *src)
{
    strcpy(dst + strlen(dst), src);
    return dst;
}

char *strncat(char *dst, const char *src, size_t n)
{
    size_t dl = strlen(dst), i = 0;
    while (i < n && src[i]) {
        dst[dl + i] = src[i];
        ++i;
    }
    dst[dl + i] = '\0';
    return dst;
}

int strcmp(const char *a, const char *b)
{
    while (*a && *a == *b) {
        ++a;
        ++b;
    }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n)
{
    while (n--) {
        if (*a != *b)
            return (unsigned char)*a - (unsigned char)*b;
        if (*a == '\0')
            return 0;
        ++a;
        ++b;
    }
    return 0;
}

char *strchr(const char *s, int c)
{
    while (*s) {
        if (*s == (char)c)
            return (char *)s;
        ++s;
    }
    return c == 0 ? (char *)s : 0;
}

char *strrchr(const char *s, int c)
{
    const char *last = 0;
    while (*s) {
        if (*s == (char)c)
            last = s;
        ++s;
    }
    if (c == 0)
        return (char *)s;
    return (char *)last;
}

char *strstr(const char *h, const char *n)
{
    size_t nl = strlen(n);
    if (!nl)
        return (char *)h;
    while (*h) {
        if (*h == *n && memcmp(h, n, nl) == 0)
            return (char *)h;
        ++h;
    }
    return 0;
}

static char *strtok_next = 0;
char *strtok(char *s, const char *delim)
{
    char *start;
    if (s)
        strtok_next = s;
    if (!strtok_next)
        return 0;
    start = strtok_next;
    while (*start) {
        const char *d = delim;
        int is_delim = 0;
        while (*d) {
            if (*start == *d++) {
                is_delim = 1;
                break;
            }
        }
        if (!is_delim)
            break;
        ++start;
    }
    if (!*start) {
        strtok_next = 0;
        return 0;
    }
    strtok_next = start;
    while (*strtok_next) {
        const char *d = delim;
        while (*d) {
            if (*strtok_next == *d) {
                *strtok_next++ = '\0';
                return start;
            }
            ++d;
        }
        ++strtok_next;
    }
    return start;
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *d = (char *)mfk_alloc((unsigned long)n);
    if (d)
        memcpy(d, s, n);
    return d;
}

int strcasecmp(const char *a, const char *b)
{
    while (*a && *b) {
        int ca = *a, cb = *b;
        if (ca >= 'A' && ca <= 'Z')
            ca += 32;
        if (cb >= 'A' && cb <= 'Z')
            cb += 32;
        if (ca != cb)
            return ca - cb;
        ++a;
        ++b;
    }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncasecmp(const char *a, const char *b, size_t n)
{
    while (n--) {
        int ca = (unsigned char)*a, cb = (unsigned char)*b;
        if (ca >= 'A' && ca <= 'Z')
            ca += 32;
        if (cb >= 'A' && cb <= 'Z')
            cb += 32;
        if (ca != cb)
            return ca - cb;
        if (ca == '\0')
            return 0;
        ++a;
        ++b;
    }
    return 0;
}

size_t strspn(const char *s, const char *accept)
{
    size_t n = 0;
    while (s[n] && strchr(accept, s[n]))
        ++n;
    return n;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t n = 0;
    while (s[n] && !strchr(reject, s[n]))
        ++n;
    return n;
}

char *strerror(int e)
{
    (void)e;
    return "error";
}

// ---- ctype ----------------------------------------------------------------
int isalpha(int c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); }
int isdigit(int c) { return c >= '0' && c <= '9'; }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isspace(int c)
{
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v';
}
int islower(int c) { return c >= 'a' && c <= 'z'; }
int isupper(int c) { return c >= 'A' && c <= 'Z'; }
int isxdigit(int c)
{
    return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}
int ispunct(int c)
{
    return c > 32 && c < 127 && !isalnum(c);
}
int isprint(int c) { return c >= 32 && c < 127; }
int iscntrl(int c) { return (c >= 0 && c < 32) || c == 127; }
int tolower(int c) { return (c >= 'A' && c <= 'Z') ? c + 32 : c; }
int toupper(int c) { return (c >= 'a' && c <= 'z') ? c - 32 : c; }

// ---- stdlib ---------------------------------------------------------------
void *malloc(size_t size) { return mfk_alloc((unsigned long)(size ? size : 1)); }

void *calloc(size_t n, size_t size)
{
    size_t total = n * size;
    void *p = mfk_alloc((unsigned long)(total ? total : 1));
    if (p)
        memset(p, 0, total);
    return p;
}

void *realloc(void *ptr, size_t size)
{
    return mfk_realloc(ptr, (unsigned long)size);
}

void free(void *ptr) { mfk_free(ptr); }

void abort(void) { mfk_abort(1, "abort"); }

char *getenv(const char *name)
{
    (void)name;
    return 0;
}

int atoi(const char *s) { return (int)strtol(s, 0, 10); }
long atol(const char *s) { return strtol(s, 0, 10); }

double atof(const char *s)
{
    const char *p = s;
    int neg = 0;
    double acc = 0.0, frac = 0.0, div = 1.0;
    int exp_neg = 0, exp = 0;
    while (*p == ' ' || *p == '\t')
        ++p;
    if (*p == '-') {
        neg = 1;
        ++p;
    } else if (*p == '+') {
        ++p;
    }
    while (*p >= '0' && *p <= '9')
        acc = acc * 10.0 + (double)(*p++ - '0');
    if (*p == '.') {
        ++p;
        while (*p >= '0' && *p <= '9') {
            frac = frac * 10.0 + (double)(*p++ - '0');
            div *= 10.0;
        }
        acc += frac / div;
    }
    if (*p == 'e' || *p == 'E') {
        ++p;
        if (*p == '-') {
            exp_neg = 1;
            ++p;
        } else if (*p == '+') {
            ++p;
        }
        while (*p >= '0' && *p <= '9')
            exp = exp * 10 + (*p++ - '0');
        while (exp-- > 0)
            acc = exp_neg ? acc / 10.0 : acc * 10.0;
    }
    return neg ? -acc : acc;
}

static int digit_val(int c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'Z')
        return c - 'A' + 10;
    return -1;
}

long strtol(const char *s, char **end, int base)
{
    const char *p = s;
    int neg = 0;
    unsigned long acc = 0;
    while (isspace((unsigned char)*p))
        ++p;
    if (*p == '-') {
        neg = 1;
        ++p;
    } else if (*p == '+') {
        ++p;
    }
    if (base == 0) {
        if (*p == '0' && (p[1] == 'x' || p[1] == 'X')) {
            base = 16;
            p += 2;
        } else if (*p == '0') {
            base = 8;
        } else {
            base = 10;
        }
    } else if (base == 16 && *p == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }
    while (*p) {
        int d = digit_val(*p);
        if (d < 0 || d >= base)
            break;
        acc = acc * (unsigned long)base + (unsigned long)d;
        ++p;
    }
    if (end)
        *end = (char *)p;
    return neg ? -(long)acc : (long)acc;
}

unsigned long strtoul(const char *s, char **end, int base)
{
    const char *p = s;
    unsigned long acc = 0;
    while (isspace((unsigned char)*p))
        ++p;
    if (*p == '+')
        ++p;
    if (base == 0) {
        if (*p == '0' && (p[1] == 'x' || p[1] == 'X')) {
            base = 16;
            p += 2;
        } else if (*p == '0') {
            base = 8;
        } else {
            base = 10;
        }
    } else if (base == 16 && *p == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }
    while (*p) {
        int d = digit_val(*p);
        if (d < 0 || d >= base)
            break;
        acc = acc * (unsigned long)base + (unsigned long)d;
        ++p;
    }
    if (end)
        *end = (char *)p;
    return acc;
}

static unsigned long rand_state = 1;
int rand(void)
{
    rand_state = rand_state * 1103515245 + 12345;
    return (int)((rand_state >> 16) & RAND_MAX);
}

void srand(unsigned seed) { rand_state = seed ? seed : 1; }

void qsort(void *base, size_t n, size_t size,
           int (*cmp)(const void *, const void *))
{
    // Insertion sort: engine sorts tiny arrays only.
    uint8_t *b = (uint8_t *)base, *tmp;
    size_t i, j;
    if (n < 2 || size == 0)
        return;
    tmp = (uint8_t *)mfk_alloc((unsigned long)size);
    if (!tmp)
        return;
    for (i = 1; i < n; ++i) {
        memcpy(tmp, b + i * size, size);
        j = i;
        while (j > 0 && cmp(tmp, b + (j - 1) * size) < 0) {
            memcpy(b + j * size, b + (j - 1) * size, size);
            --j;
        }
        memcpy(b + j * size, tmp, size);
    }
    mfk_free(tmp);
}

int abs(int x) { return x < 0 ? -x : x; }
long labs(long x) { return x < 0 ? -x : x; }
int mkstemp(char *t)
{
    (void)t;
    return -1;
}

int remove(const char *path)
{
    (void)path;
    // SimplFS files are truncated on write; pretend removal succeeded.
    return 0;
}

int rename(const char *oldpath, const char *newpath)
{
    (void)oldpath;
    (void)newpath;
    return -1;
}

int mkdir(const char *path, mode_t mode)
{
    (void)path;
    (void)mode;
    return 0; // SimplFS dirs are created on demand; pretend success.
}

int stat(const char *path, struct stat *buf);

ssize_t read(int fd, void *buf, size_t n)
{
    (void)fd;
    (void)buf;
    (void)n;
    return -1;
}

ssize_t write(int fd, const void *buf, size_t n)
{
    if (fd == 1 || fd == 2) {
        mfk_debug_write((const char *)buf, (unsigned long)n);
        return (ssize_t)n;
    }
    return -1;
}
