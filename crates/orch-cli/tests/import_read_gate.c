// Linux-only test interposer: delay a selected template read until the parent
// releases it. No importer branch or production environment hook is added.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <sys/stat.h>

static ssize_t (*next_read)(int, void *, size_t);
static unsigned int seen;

__attribute__((constructor)) static void initialize(void) {
    next_read = dlsym(RTLD_NEXT, "read");
}

ssize_t read(int fd, void *buf, size_t count) {
    const char *target = getenv("IMPORT_GATE_FILE");
    struct stat actual, expected;
    if (target && fstat(fd, &actual) == 0 && stat(target, &expected) == 0 &&
        actual.st_dev == expected.st_dev && actual.st_ino == expected.st_ino &&
        lseek(fd, 0, SEEK_CUR) == 0 && ++seen == strtoul(getenv("IMPORT_GATE_READ"), NULL, 10)) {
        int ready = open(getenv("IMPORT_GATE_READY"), O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (ready >= 0) close(ready);
        struct timespec tick = {.tv_sec = 0, .tv_nsec = 1000000};
        while (access(getenv("IMPORT_GATE_RELEASE"), F_OK) != 0) nanosleep(&tick, NULL);
    }
    return next_read(fd, buf, count);
}
