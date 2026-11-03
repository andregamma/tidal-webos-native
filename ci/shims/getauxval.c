/* The NDK sysroot's glibc (2.12) predates getauxval (2.16), but Rust's std and `ring` call it.
 * The TV's runtime libc may or may not have it; this weak fallback reads /proc/self/auxv. */
#include <fcntl.h>
#include <unistd.h>

__attribute__((weak)) unsigned long getauxval(unsigned long type)
{
    unsigned long pair[2];
    unsigned long val = 0;
    int fd = open("/proc/self/auxv", O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    while (read(fd, pair, sizeof pair) == (ssize_t)sizeof pair) {
        if (pair[0] == type) {
            val = pair[1];
            break;
        }
        if (pair[0] == 0) {
            break;
        }
    }
    close(fd);
    return val;
}
