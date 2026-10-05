// MFK freestanding unistd.h (MIT).
#ifndef MFK_UNISTD_H
#define MFK_UNISTD_H

#include <sys/types.h>
ssize_t read(int fd, void *buf, size_t n);
ssize_t write(int fd, const void *buf, size_t n);

#endif
