// MFK freestanding stdlib.h for vendored doomgeneric (MIT).
#ifndef MFK_STDLIB_H
#define MFK_STDLIB_H

#include <stddef.h>

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 32767

void *malloc(size_t size);
void *calloc(size_t n, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
void abort(void);
void exit(int code);
void _exit(int code);
int atexit(void (*fn)(void));
char *getenv(const char *name);
int atoi(const char *s);
long atol(const char *s);
double atof(const char *s);
long strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
int rand(void);
void srand(unsigned seed);
void qsort(void *base, size_t n, size_t size, int (*cmp)(const void *, const void *));
int abs(int x);
long labs(long x);
int mkstemp(char *t);

#endif
