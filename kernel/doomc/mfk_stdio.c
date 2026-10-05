// MFK freestanding stdio for vendored doomgeneric (MIT).
// Guest-filesystem-backed FILE shim + printf/scanf subsets.
// Engine C code runs with SSE enabled (kernel turns it on at engine
// start), so double math here is fine.

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include "mfk_defs.h"

// errno lives in mfk_libc.c; declared for callers via <errno.h>.
extern int errno;

struct MFK_FILE {
    int in_use;
    int is_write;   // 0 = read handle, 1 = write handle
    int eof;
    int err;
    char path[128];
    long size;      // read: file size; write/append: buffered length
    long pos;       // read position (write: == size always except seeks)
    uint8_t *wbuf;  // write buffer
    long wcap;
    int ungetc_ch;  // -1 = empty
};

#define MAX_OPEN 8
static struct MFK_FILE file_pool[MAX_OPEN];
static struct MFK_FILE stdout_file = {1, 1, 0, 0, "", 0, 0, 0, 0, -1};
static struct MFK_FILE stderr_file = {1, 1, 0, 0, "", 0, 0, 0, 0, -1};
static struct MFK_FILE stdin_file = {1, 0, 1, 0, "", 0, 0, 0, 0, -1};

FILE *mfk_stdin_obj = (FILE *)&stdin_file;
FILE *mfk_stdout_obj = (FILE *)&stdout_file;
FILE *mfk_stderr_obj = (FILE *)&stderr_file;

static void path_copy(char *dst, const char *src)
{
    unsigned i = 0;
    while (i + 1 < 128 && src[i]) {
        dst[i] = src[i];
        ++i;
    }
    dst[i] = '\0';
}

FILE *fopen(const char *path, const char *mode)
{
    int i, want_write = 0, want_append = 0;
    struct MFK_FILE *f = 0;
    if (!path || !mode)
        return 0;
    if (mode[0] == 'w')
        want_write = 1;
    else if (mode[0] == 'a')
        want_append = 1;
    else if (mode[0] != 'r')
        return 0;
    for (i = 0; i < MAX_OPEN; ++i) {
        if (!file_pool[i].in_use) {
            f = &file_pool[i];
            break;
        }
    }
    if (!f)
        return 0;
    f->in_use = 1;
    f->eof = 0;
    f->err = 0;
    f->wbuf = 0;
    f->wcap = 0;
    f->ungetc_ch = -1;
    path_copy(f->path, path);
    if (want_write) {
        f->is_write = 1;
        f->size = 0;
        f->pos = 0;
        return (FILE *)f;
    }
    long sz = mfk_fs_size(path);
    if (sz < 0) {
        f->in_use = 0;
        errno = 2; // ENOENT
        return 0;
    }
    f->is_write = 0;
    f->size = sz;
    f->pos = 0;
    if (want_append) {
        // Preload existing content so appends preserve it.
        uint8_t *buf = 0;
        if (sz > 0) {
            if (sz > 4 * 1024 * 1024) {
                f->in_use = 0;
                return 0;
            }
            buf = (uint8_t *)mfk_alloc((unsigned long)sz);
            if (!buf) {
                f->in_use = 0;
                return 0;
            }
            long got = mfk_fs_read(path, 0, buf, (unsigned long)sz);
            if (got < 0) {
                mfk_free(buf);
                f->in_use = 0;
                return 0;
            }
            sz = got;
        }
        f->is_write = 1;
        f->wbuf = buf;
        f->wcap = sz;
        f->size = sz;
        f->pos = sz;
    }
    return (FILE *)f;
}

static int flush_write(struct MFK_FILE *f)
{
    long w;
    if (!f->is_write)
        return 0;
    w = mfk_fs_write(f->path, f->wbuf ? f->wbuf : (uint8_t *)"", (unsigned long)f->size);
    if (w < 0 || w != f->size)
        return -1;
    return 0;
}

int fclose(FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    int rc = 0;
    if (f == &stdout_file || f == &stderr_file || f == &stdin_file)
        return 0;
    if (!f->in_use)
        return -1;
    if (f->is_write)
        rc = flush_write(f);
    if (f->wbuf)
        mfk_free(f->wbuf);
    f->in_use = 0;
    f->wbuf = 0;
    return rc;
}

static int write_grow(struct MFK_FILE *f, const uint8_t *data, long len)
{
    long need = f->size + len;
    if (need > f->wcap) {
        long ncap = f->wcap ? f->wcap * 2 : 1024;
        uint8_t *nb;
        while (ncap < need)
            ncap *= 2;
        nb = (uint8_t *)mfk_realloc(f->wbuf, (unsigned long)ncap);
        if (!nb)
            return -1;
        f->wbuf = nb;
        f->wcap = ncap;
    }
    if (len) {
        unsigned long i;
        for (i = 0; i < (unsigned long)len; ++i)
            f->wbuf[f->size + i] = data[i];
    }
    f->size += len;
    f->pos = f->size;
    return 0;
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    size_t total = size * nmemb, i;
    if (f == &stdout_file || f == &stderr_file) {
        mfk_debug_write((const char *)ptr, (unsigned long)total);
        return nmemb;
    }
    if (!f->in_use || !f->is_write) {
        return 0;
    }
    // Support mid-buffer seeks: pad with zeros when pos < size.
    if (f->pos < f->size) {
        long off = f->pos;
        for (i = 0; i < total && off + (long)i < f->size; ++i)
            f->wbuf[off + i] = ((const uint8_t *)ptr)[i];
        f->pos += (long)i;
        if (i < total) {
            // Reached end: append the rest.
            if (write_grow(f, (const uint8_t *)ptr + i, (long)(total - i)) != 0)
                return i / (size ? size : 1);
            return nmemb;
        }
        return nmemb;
    }
    if (write_grow(f, (const uint8_t *)ptr, (long)total) != 0)
        return 0;
    return nmemb;
}

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    size_t want = size * nmemb, got = 0;
    long r;
    if (!size || !nmemb)
        return 0;
    if (!f->in_use || f->is_write)
        return 0;
    if (f->ungetc_ch >= 0 && want > 0) {
        ((uint8_t *)ptr)[0] = (uint8_t)f->ungetc_ch;
        f->ungetc_ch = -1;
        f->pos += 1;
        got = 1;
    }
    if (got < want && f->pos < f->size) {
        long chunk = (long)(want - got);
        if (chunk > f->size - f->pos)
            chunk = f->size - f->pos;
        r = mfk_fs_read(f->path, (unsigned long)f->pos,
                        (uint8_t *)ptr + got, (unsigned long)chunk);
        if (r < 0) {
            f->err = 1;
            return got / size;
        }
        got += (size_t)r;
        f->pos += r;
    }
    if (got < want)
        f->eof = 1;
    return got / size;
}

int fseek(FILE *fp, long offset, int whence)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    long base;
    if (!f->in_use)
        return -1;
    if (whence == SEEK_SET)
        base = offset;
    else if (whence == SEEK_CUR)
        base = f->pos + offset;
    else if (whence == SEEK_END)
        base = f->size + offset;
    else
        return -1;
    if (base < 0)
        return -1;
    if (!f->is_write && base > f->size)
        return -1;
    if (f->is_write && base > f->size) {
        // Extend with zeros.
        long pad = base - f->size, i;
        uint8_t z = 0;
        for (i = 0; i < pad; ++i)
            if (write_grow(f, &z, 1) != 0)
                return -1;
    }
    f->pos = base;
    f->eof = 0;
    f->ungetc_ch = -1;
    return 0;
}

long ftell(FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    if (!f->in_use)
        return -1;
    return f->pos;
}

void rewind(FILE *fp)
{
    fseek(fp, 0, SEEK_SET);
    ((struct MFK_FILE *)fp)->eof = 0;
}

int feof(FILE *fp) { return ((struct MFK_FILE *)fp)->eof; }
int ferror(FILE *fp) { return ((struct MFK_FILE *)fp)->err; }
int fflush(FILE *fp)
{
    (void)fp;
    return 0;
}

char *fgets(char *s, int size, FILE *fp)
{
    int i = 0;
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    uint8_t ch;
    if (size <= 1 || !f->in_use || f->is_write)
        return 0;
    while (i < size - 1) {
        if (f->ungetc_ch >= 0) {
            ch = (uint8_t)f->ungetc_ch;
            f->ungetc_ch = -1;
            f->pos += 1;
        } else {
            if (f->pos >= f->size) {
                f->eof = 1;
                break;
            }
            if (mfk_fs_read(f->path, (unsigned long)f->pos, &ch, 1) != 1) {
                f->err = 1;
                break;
            }
            f->pos += 1;
        }
        s[i++] = (char)ch;
        if (ch == '\n')
            break;
    }
    if (i == 0)
        return 0;
    s[i] = '\0';
    return s;
}

int fputs(const char *s, FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    size_t n = 0;
    while (s[n])
        ++n;
    if (f == &stdout_file || f == &stderr_file) {
        mfk_debug_write(s, (unsigned long)n);
        return 0;
    }
    return fwrite(s, 1, n, fp) == n ? 0 : -1;
}

int fputc(int c, FILE *fp)
{
    uint8_t ch = (uint8_t)c;
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    if (f == &stdout_file || f == &stderr_file) {
        mfk_debug_write((const char *)&ch, 1);
        return c;
    }
    return fwrite(&ch, 1, 1, fp) == 1 ? c : EOF;
}

int fgetc(FILE *fp)
{
    uint8_t ch;
    if (fread(&ch, 1, 1, fp) == 1)
        return ch;
    return EOF;
}

int ungetc(int c, FILE *fp)
{
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    if (c == EOF || f->is_write || f->ungetc_ch >= 0)
        return EOF;
    f->ungetc_ch = c & 0xFF;
    f->pos -= 1;
    f->eof = 0;
    return c;
}

int putchar(int c)
{
    uint8_t ch = (uint8_t)c;
    mfk_debug_write((const char *)&ch, 1);
    return c;
}

int puts(const char *s)
{
    mfk_debug_write(s, __builtin_strlen(s));
    mfk_debug_write("\n", 1);
    return 0;
}

void perror(const char *s)
{
    if (s && *s) {
        mfk_debug_write(s, __builtin_strlen(s));
        mfk_debug_write(": I/O error\n", 12);
    } else {
        mfk_debug_write("I/O error\n", 10);
    }
}

// ---- printf core ----------------------------------------------------------
typedef void (*emit_fn)(void *ctx, const char *data, unsigned len);

struct buf_emit {
    char *buf;
    size_t cap;
    size_t len;
};

static void buf_emit_fn(void *ctx, const char *data, unsigned len)
{
    struct buf_emit *b = (struct buf_emit *)ctx;
    unsigned i;
    for (i = 0; i < len && b->len + 1 < b->cap; ++i)
        b->buf[b->len++] = data[i];
}

static void dbg_emit_fn(void *ctx, const char *data, unsigned len)
{
    (void)ctx;
    mfk_debug_write(data, len);
}

struct file_emit {
    FILE *fp;
    int count;
};

static void file_emit_fn(void *ctx, const char *data, unsigned len)
{
    struct file_emit *e = (struct file_emit *)ctx;
    struct MFK_FILE *f = (struct MFK_FILE *)e->fp;
    if (f == &stdout_file || f == &stderr_file) {
        mfk_debug_write(data, len);
        e->count += (int)len;
        return;
    }
    if (fwrite(data, 1, len, e->fp) == len)
        e->count += (int)len;
}

static long long mult_pow10(int e)
{
    long long r = 1;
    while (e-- > 0)
        r *= 10;
    return r;
}

static void emit_pad(emit_fn em, void *ctx, char c, int n, int *total)
{
    while (n-- > 0) {
        em(ctx, &c, 1);
        ++*total;
    }
}

static int fmt_core(emit_fn em, void *ctx, const char *fmt, va_list ap)
{
    int total = 0;
    while (*fmt) {
        if (*fmt != '%') {
            em(ctx, fmt, 1);
            ++total;
            ++fmt;
            continue;
        }
        ++fmt;
        int left = 0, zero = 0, width = 0, prec = -1, lenmod = 0; // 0=,1=l,2=ll,3=h,4=hh,5=z
        while (*fmt == '-' || *fmt == '+' || *fmt == ' ' || *fmt == '#' || *fmt == '0') {
            if (*fmt == '-')
                left = 1;
            if (*fmt == '0')
                zero = 1;
            ++fmt;
        }
        if (*fmt == '*') {
            width = va_arg(ap, int);
            ++fmt;
        } else {
            while (*fmt >= '0' && *fmt <= '9')
                width = width * 10 + (*fmt++ - '0');
        }
        if (*fmt == '.') {
            ++fmt;
            if (*fmt == '*') {
                prec = va_arg(ap, int);
                ++fmt;
            } else {
                prec = 0;
                while (*fmt >= '0' && *fmt <= '9')
                    prec = prec * 10 + (*fmt++ - '0');
            }
        }
        if (*fmt == 'l') {
            ++fmt;
            lenmod = 1;
            if (*fmt == 'l') {
                ++fmt;
                lenmod = 2;
            }
        } else if (*fmt == 'h') {
            ++fmt;
            lenmod = 3;
            if (*fmt == 'h') {
                ++fmt;
                lenmod = 4;
            }
        } else if (*fmt == 'z') {
            ++fmt;
            lenmod = 5;
        }
        char spec = *fmt++;
        if (spec == 'd' || spec == 'i' || spec == 'u' || spec == 'o' ||
            spec == 'x' || spec == 'X' || spec == 'p') {
            unsigned long long uv;
            int neg = 0, base = 10, upper = (spec == 'X');
            char digits[24];
            int nd = 0, i;
            if (spec == 'p') {
                uv = (unsigned long long)(uintptr_t)va_arg(ap, void *);
                base = 16;
            } else if (spec == 'd' || spec == 'i') {
                long long sv;
                if (lenmod == 2)
                    sv = va_arg(ap, long long);
                else if (lenmod == 1)
                    sv = va_arg(ap, long);
                else
                    sv = va_arg(ap, int);
                if (sv < 0) {
                    neg = 1;
                    uv = (unsigned long long)(-sv);
                } else {
                    uv = (unsigned long long)sv;
                }
            } else {
                if (spec == 'o')
                    base = 8;
                else if (spec == 'x' || spec == 'X')
                    base = 16;
                if (lenmod == 2)
                    uv = va_arg(ap, unsigned long long);
                else if (lenmod == 1)
                    uv = va_arg(ap, unsigned long);
                else
                    uv = va_arg(ap, unsigned int);
            }
            if (uv == 0) {
                digits[nd++] = '0';
            } else {
                while (uv) {
                    int d = (int)(uv % (unsigned)base);
                    digits[nd++] = (char)(d < 10 ? '0' + d
                                                 : (upper ? 'A' : 'a') + d - 10);
                    uv /= (unsigned)base;
                }
            }
            // Precision: minimum number of digits (zero-padded on the
            // most-significant side, i.e. appended to LSD-first digits).
            // Precision overrides the '0' flag padding.
            while (prec > nd && nd < (int)sizeof(digits))
                digits[nd++] = '0';
            if (prec >= 0)
                zero = 0;
            int numlen = nd + (neg ? 1 : 0);
            int pad = width > numlen ? width - numlen : 0;
            if (!left && !zero)
                emit_pad(em, ctx, ' ', pad, &total);
            if (neg) {
                em(ctx, "-", 1);
                ++total;
            }
            if (!left && zero)
                emit_pad(em, ctx, '0', pad, &total);
            for (i = nd - 1; i >= 0; --i) {
                em(ctx, &digits[i], 1);
                ++total;
            }
            if (left)
                emit_pad(em, ctx, ' ', pad, &total);
        } else if (spec == 'c') {
            char c = (char)va_arg(ap, int);
            int pad = width > 1 ? width - 1 : 0;
            if (!left)
                emit_pad(em, ctx, ' ', pad, &total);
            em(ctx, &c, 1);
            ++total;
            if (left)
                emit_pad(em, ctx, ' ', pad, &total);
        } else if (spec == 's') {
            const char *s = va_arg(ap, const char *);
            int sl = 0, pad, i;
            if (!s)
                s = "(null)";
            while (s[sl] && (prec < 0 || sl < prec))
                ++sl;
            pad = width > sl ? width - sl : 0;
            if (!left)
                emit_pad(em, ctx, ' ', pad, &total);
            for (i = 0; i < sl; ++i) {
                em(ctx, &s[i], 1);
                ++total;
            }
            if (left)
                emit_pad(em, ctx, ' ', pad, &total);
        } else if (spec == 'f' || spec == 'F' || spec == 'g' || spec == 'G' ||
                   spec == 'e' || spec == 'E') {
            double dv = va_arg(ap, double);
            char num[48];
            int ni = 0, i, frac = 6;
            int neg = 0;
            long long ip;
            if (dv < 0) {
                neg = 1;
                dv = -dv;
            }
            if (prec >= 0)
                frac = prec > 9 ? 9 : prec;
            ip = (long long)dv;
            {
                double fpart = dv - (double)ip;
                long long fscaled = 0, mult = 1;
                for (i = 0; i < frac; ++i)
                    mult *= 10;
                fscaled = (long long)(fpart * (double)mult + 0.5);
                if (fscaled >= mult) {
                    ip += 1;
                    fscaled -= mult;
                }
                {
                    char rev[24];
                    int rn = 0;
                    long long t = ip;
                    if (t == 0)
                        rev[rn++] = '0';
                    while (t) {
                        rev[rn++] = (char)('0' + t % 10);
                        t /= 10;
                    }
                    if (neg)
                        num[ni++] = '-';
                    while (rn)
                        num[ni++] = rev[--rn];
                    if (frac > 0) {
                        num[ni++] = '.';
                        for (i = frac - 1; i >= 0; --i)
                            num[ni++] = (char)('0' + (fscaled / mult_pow10(i)) % 10);
                    }
                }
            }
            {
                int pad = width > ni ? width - ni : 0;
                if (!left)
                    emit_pad(em, ctx, zero ? '0' : ' ', pad, &total);
                for (i = 0; i < ni; ++i) {
                    em(ctx, &num[i], 1);
                    ++total;
                }
                if (left)
                    emit_pad(em, ctx, ' ', pad, &total);
            }
        } else if (spec == 'n') {
            int *p = va_arg(ap, int *);
            *p = total;
        } else if (spec == '%') {
            em(ctx, "%", 1);
            ++total;
        } else {
            em(ctx, "%", 1);
            em(ctx, &spec, 1);
            total += 2;
        }
    }
    return total;
}

int vfprintf(FILE *f, const char *fmt, va_list ap)
{
    struct file_emit e;
    e.fp = f;
    e.count = 0;
    fmt_core(file_emit_fn, &e, fmt, ap);
    return e.count;
}

int vsnprintf(char *s, size_t n, const char *fmt, va_list ap)
{
    struct buf_emit b;
    b.buf = s;
    b.cap = n;
    b.len = 0;
    int r = fmt_core(buf_emit_fn, &b, fmt, ap);
    if (n)
        s[b.len < n ? b.len : n - 1] = '\0';
    return r;
}

int printf(const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = fmt_core(dbg_emit_fn, 0, fmt, ap);
    va_end(ap);
    return r;
}

int fprintf(FILE *f, const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vfprintf(f, fmt, ap);
    va_end(ap);
    return r;
}

int sprintf(char *s, const char *fmt, ...)
{
    va_list ap;
    struct buf_emit b;
    int r;
    b.buf = s;
    b.cap = (size_t)-1;
    b.len = 0;
    va_start(ap, fmt);
    r = fmt_core(buf_emit_fn, &b, fmt, ap);
    va_end(ap);
    s[b.len] = '\0';
    return r;
}

int snprintf(char *s, size_t n, const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(s, n, fmt, ap);
    va_end(ap);
    return r;
}

// ---- scanf subset ---------------------------------------------------------
static int scan_val(const char **pp, unsigned long long *out, int base, int width,
                    int *neg)
{
    const char *p = *pp;
    unsigned long long acc = 0;
    int n = 0;
    *neg = 0;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
        ++p;
    if (*p == '+' || *p == '-') {
        if (*p == '-')
            *neg = 1;
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
    while (*p && (width < 0 || n < width)) {
        int d;
        if (*p >= '0' && *p <= '9')
            d = *p - '0';
        else if (*p >= 'a' && *p <= 'z')
            d = *p - 'a' + 10;
        else if (*p >= 'A' && *p <= 'Z')
            d = *p - 'A' + 10;
        else
            break;
        if (d >= base)
            break;
        acc = acc * (unsigned)base + (unsigned)d;
        ++p;
        ++n;
    }
    if (!n)
        return 0;
    *pp = p;
    *out = acc;
    return 1;
}

static int vsscanf_impl(const char *s, const char *fmt, va_list ap, int *consumed_out)
{
    int assigned = 0, consumed = 0;
    while (*fmt) {
        if (*fmt == ' ' || *fmt == '\t' || *fmt == '\n' || *fmt == '\r') {
            while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r') {
                ++s;
                ++consumed;
            }
            ++fmt;
            continue;
        }
        if (*fmt != '%') {
            if (*s != *fmt)
                break;
            ++s;
            ++fmt;
            ++consumed;
            continue;
        }
        ++fmt;
        int suppress = 0, width = -1, lenmod = 0;
        if (*fmt == '*') {
            suppress = 1;
            ++fmt;
        }
        if (*fmt >= '0' && *fmt <= '9') {
            width = 0;
            while (*fmt >= '0' && *fmt <= '9')
                width = width * 10 + (*fmt++ - '0');
        }
        if (*fmt == 'l') {
            ++fmt;
            lenmod = 1;
            if (*fmt == 'l') {
                ++fmt;
                lenmod = 2;
            }
        } else if (*fmt == 'h') {
            ++fmt;
            lenmod = 3;
            if (*fmt == 'h') {
                ++fmt;
                lenmod = 4;
            }
        }
        char spec = *fmt++;
        if (spec == 'd' || spec == 'i' || spec == 'u' || spec == 'x' ||
            spec == 'X' || spec == 'o') {
            int base = 10, neg = 0;
            unsigned long long v;
            const char *before = s;
            if (spec == 'x' || spec == 'X')
                base = 16;
            else if (spec == 'o')
                base = 8;
            else if (spec == 'i')
                base = 0;
            if (!scan_val(&s, &v, base, width, &neg))
                break;
            consumed += (int)(s - before);
            if (!suppress) {
                long long sv = neg ? -(long long)v : (long long)v;
                if (lenmod == 2)
                    *va_arg(ap, long long *) = sv;
                else if (lenmod == 1)
                    *va_arg(ap, long *) = (long)sv;
                else if (spec == 'd' || spec == 'i')
                    *va_arg(ap, int *) = (int)sv;
                else
                    *va_arg(ap, unsigned *) = (unsigned)v;
                ++assigned;
            }
        } else if (spec == 's') {
            int n = 0;
            char *dst = suppress ? 0 : va_arg(ap, char *);
            while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r') {
                ++s;
                ++consumed;
            }
            while (*s && *s != ' ' && *s != '\t' && *s != '\n' && *s != '\r' &&
                   (width < 0 || n < width)) {
                if (dst)
                    dst[n] = *s;
                ++n;
                ++s;
                ++consumed;
            }
            if (!n)
                break;
            if (dst)
                dst[n] = '\0';
            if (!suppress)
                ++assigned;
        } else if (spec == 'c') {
            int n = width < 0 ? 1 : width, i;
            char *dst = suppress ? 0 : va_arg(ap, char *);
            if (!*s)
                break;
            for (i = 0; i < n && s[i]; ++i) {
                if (dst)
                    dst[i] = s[i];
            }
            if (!i)
                break;
            s += i;
            consumed += i;
            if (!suppress)
                ++assigned;
        } else if (spec == '[') {
            int negate = 0, n = 0, i;
            char set[128];
            int setn = 0;
            char *dst = suppress ? 0 : va_arg(ap, char *);
            if (*fmt == '^') {
                negate = 1;
                ++fmt;
            }
            if (*fmt == ']') {
                set[setn++] = ']';
                ++fmt;
            }
            while (*fmt && *fmt != ']' && setn < 120) {
                if (fmt[1] == '-' && fmt[2] && fmt[2] != ']') {
                    char lo = fmt[0], hi = fmt[2];
                    char c;
                    for (c = lo; c <= hi && setn < 120; ++c)
                        set[setn++] = c;
                    fmt += 3;
                } else {
                    set[setn++] = *fmt++;
                }
            }
            if (*fmt == ']')
                ++fmt;
            while (*s) {
                int in = 0;
                for (i = 0; i < setn; ++i) {
                    if (*s == set[i]) {
                        in = 1;
                        break;
                    }
                }
                if (in == negate)
                    break;
                if (width >= 0 && n >= width)
                    break;
                if (dst)
                    dst[n] = *s;
                ++n;
                ++s;
                ++consumed;
            }
            if (dst)
                dst[n] = '\0';
            if (!suppress)
                ++assigned;
        } else if (spec == 'n') {
            int *p = va_arg(ap, int *);
            *p = consumed;
        } else if (spec == '%') {
            if (*s != '%')
                break;
            ++s;
            ++consumed;
        } else {
            break;
        }
    }
    if (consumed_out)
        *consumed_out = consumed;
    return assigned;
}

int sscanf(const char *s, const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsscanf_impl(s, fmt, ap, 0);
    va_end(ap);
    return r;
}

int fscanf(FILE *fp, const char *fmt, ...)
{
    // Config-file scale: slurp up to 4KB from the current position,
    // scan in memory, advance past consumed input.
    char tmp[4096];
    struct MFK_FILE *f = (struct MFK_FILE *)fp;
    long avail, n;
    va_list ap;
    int r, used = 0;
    if (!f->in_use || f->is_write)
        return EOF;
    avail = f->size - f->pos;
    if (avail <= 0) {
        f->eof = 1;
        return EOF;
    }
    if (avail > (long)sizeof(tmp) - 1)
        avail = sizeof(tmp) - 1;
    n = mfk_fs_read(f->path, (unsigned long)f->pos, (uint8_t *)tmp,
                    (unsigned long)avail);
    if (n <= 0) {
        f->eof = 1;
        return EOF;
    }
    tmp[n] = '\0';
    va_start(ap, fmt);
    r = vsscanf_impl(tmp, fmt, ap, &used);
    va_end(ap);
    if (r <= 0 && used == 0) {
        if (r == 0)
            return 0;
        return EOF;
    }
    f->pos += used;
    if (f->pos >= f->size)
        f->eof = 1;
    return r;
}
