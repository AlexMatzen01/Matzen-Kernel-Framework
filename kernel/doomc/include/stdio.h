// MFK freestanding stdio.h for vendored doomgeneric (MIT).
#ifndef MFK_STDIO_H
#define MFK_STDIO_H

#include <stddef.h>

typedef struct MFK_FILE FILE;

extern FILE *mfk_stdin_obj;
extern FILE *mfk_stdout_obj;
extern FILE *mfk_stderr_obj;
#define stdin (mfk_stdin_obj)
#define stdout (mfk_stdout_obj)
#define stderr (mfk_stderr_obj)

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define BUFSIZ 512
#define FILENAME_MAX 128

FILE *fopen(const char *path, const char *mode);
int fclose(FILE *f);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *f);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *f);
int fseek(FILE *f, long offset, int whence);
long ftell(FILE *f);
void rewind(FILE *f);
int feof(FILE *f);
int ferror(FILE *f);
int fflush(FILE *f);
char *fgets(char *s, int size, FILE *f);
int fputs(const char *s, FILE *f);
int fputc(int c, FILE *f);
int putchar(int c);
int puts(const char *s);
int fgetc(FILE *f);
int ungetc(int c, FILE *f);
int printf(const char *fmt, ...);
int fprintf(FILE *f, const char *fmt, ...);
int sprintf(char *s, const char *fmt, ...);
int snprintf(char *s, size_t n, const char *fmt, ...);
int vfprintf(FILE *f, const char *fmt, __builtin_va_list ap);
int vsnprintf(char *s, size_t n, const char *fmt, __builtin_va_list ap);
int sscanf(const char *s, const char *fmt, ...);
int fscanf(FILE *f, const char *fmt, ...);
void perror(const char *s);
int remove(const char *path);
int rename(const char *oldpath, const char *newpath);

#endif
