// MFK freestanding sys/stat.h (MIT).
#ifndef MFK_SYS_STAT_H
#define MFK_SYS_STAT_H

#include <sys/types.h>
#define S_IFDIR 0040000
#define S_IFREG 0100000
struct stat {
    unsigned long st_size;
    unsigned int st_mode;
};
int mkdir(const char *path, mode_t mode);
int stat(const char *path, struct stat *buf);

#endif
