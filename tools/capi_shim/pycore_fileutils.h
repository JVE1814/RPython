/* Project-written stand-in. */
#include <fcntl.h>
#include <errno.h>
static inline int _Py_IsValidFD(int fd) { return fd >= 0 && (fcntl(fd, F_GETFD) != -1 || errno != EBADF); }
