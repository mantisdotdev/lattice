/*
 * I/O fault-injection shim for G1.1's power-loss harness.
 *
 * Interposes the libc write path and journals every operation that affects
 * durability. It sits BELOW the product: `ltx` is unmodified and cannot detect
 * it, which is what §0.3's anti-gaming rule requires -- a product that could
 * see the shim could behave differently under measurement.
 *
 * It does NOT inject faults itself. Recording and replaying are separated on
 * purpose: a shim that both perturbed and observed would make a failure hard to
 * attribute, and a recorded journal can be replayed many different ways from
 * one expensive execution.
 *
 * Journal record (little-endian):
 *   u32 op        1=write 2=pwrite 3=fsync 4=fdatasync 5=rename 6=ftruncate 7=unlink
 *   u64 seq       global ordering
 *   u64 offset    file offset for writes, else 0
 *   u64 length    bytes for writes/truncate, else 0
 *   u32 path_len
 *   u8  path[path_len]
 *   u8  payload[length]   (writes only)
 *
 * Only paths under IOFAULT_ROOT are journalled, so the shim never perturbs the
 * harness's own bookkeeping or unrelated system I/O.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/param.h>
#include <string.h>
#include <unistd.h>

/*
 * macOS resolves libc symbols through a two-level namespace, so defining a
 * function named `write` does NOT shadow libc's. dyld requires an explicit
 * __interpose table. Linux's LD_PRELOAD does use plain symbol override, so the
 * two platforms need different plumbing for the same effect.
 */
#ifdef __APPLE__
#define IOFAULT_INTERPOSE(replacement, original)                              \
  __attribute__((used)) static struct {                                       \
    const void *repl;                                                         \
    const void *orig;                                                         \
  } _interpose_##original __attribute__((section("__DATA,__interpose"))) = {  \
      (const void *)(unsigned long)&replacement,                              \
      (const void *)(unsigned long)&original}
#define IOFAULT_NAME(f) iofault_##f
#else
#define IOFAULT_INTERPOSE(replacement, original)
#define IOFAULT_NAME(f) f
#endif

#define OP_WRITE 1
#define OP_PWRITE 2
#define OP_FSYNC 3
#define OP_FDATASYNC 4
#define OP_RENAME 5
#define OP_FTRUNCATE 6
#define OP_UNLINK 7

static int journal_fd = -1;
static char root[PATH_MAX];
static size_t root_len = 0;
static atomic_ullong seq = 0;
static int initialised = 0;

static void init_once(void) {
  if (initialised) return;
  initialised = 1;
  const char *j = getenv("IOFAULT_JOURNAL");
  const char *r = getenv("IOFAULT_ROOT");
  if (!j || !r) return;
  /* Resolve the root. On macOS F_GETPATH returns the fully-resolved path
   * (/private/var/...), while the caller's IOFAULT_ROOT is usually the
   * symlinked form (/var/...). Comparing them unresolved silently journalled
   * nothing at all -- the shim ran, interposed correctly, and recorded zero
   * records because every path failed the prefix test. */
  if (realpath(r, root) == NULL) snprintf(root, sizeof(root), "%s", r);
  root_len = strlen(root);
  journal_fd = open(j, O_WRONLY | O_CREAT | O_APPEND, 0600);
}

/* Resolve an fd to a path. Only fds under IOFAULT_ROOT are journalled. */
static int fd_path(int fd, char *out, size_t cap) {
#ifdef __APPLE__
  char buf[PATH_MAX];
  if (fcntl(fd, F_GETPATH, buf) == -1) return 0;
  snprintf(out, cap, "%s", buf);
  return 1;
#else
  char link[64];
  snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
  ssize_t n = readlink(link, out, cap - 1);
  if (n < 0) return 0;
  out[n] = '\0';
  return 1;
#endif
}

static int under_root(const char *path) {
  return root_len > 0 && strncmp(path, root, root_len) == 0;
}

static void journal(uint32_t op, const char *path, uint64_t offset,
                    const void *payload, uint64_t length) {
  if (journal_fd < 0 || !under_root(path)) return;
  uint64_t s = atomic_fetch_add(&seq, 1);
  uint32_t plen = (uint32_t)strlen(path);

  /* One writev-shaped burst so concurrent writers cannot interleave a record. */
  size_t header = 4 + 8 + 8 + 8 + 4;
  size_t total = header + plen + (payload ? length : 0);
  unsigned char *rec = malloc(total);
  if (!rec) return;
  size_t o = 0;
  memcpy(rec + o, &op, 4); o += 4;
  memcpy(rec + o, &s, 8); o += 8;
  memcpy(rec + o, &offset, 8); o += 8;
  memcpy(rec + o, &length, 8); o += 8;
  memcpy(rec + o, &plen, 4); o += 4;
  memcpy(rec + o, path, plen); o += plen;
  if (payload) memcpy(rec + o, payload, length);

#ifdef __APPLE__
  ssize_t (*real_write)(int, const void *, size_t) = write;
#else
  ssize_t (*real_write)(int, const void *, size_t) =
      (ssize_t (*)(int, const void *, size_t))dlsym(RTLD_NEXT, "write");
#endif
  size_t written = 0;
  while (written < total) {
    ssize_t n = real_write(journal_fd, rec + written, total - written);
    if (n <= 0) break;
    written += (size_t)n;
  }
  free(rec);
}

ssize_t IOFAULT_NAME(write)(int fd, const void *buf, size_t count) {
  init_once();
#ifdef __APPLE__
  ssize_t (*real)(int, const void *, size_t) = write;
#else
  static ssize_t (*real)(int, const void *, size_t) = NULL;
  if (!real) real = (ssize_t (*)(int, const void *, size_t))dlsym(RTLD_NEXT, "write");
#endif
  char path[PATH_MAX];
  off_t before = 0;
  int tracked = fd_path(fd, path, sizeof(path)) && under_root(path);
  if (tracked) before = lseek(fd, 0, SEEK_CUR);
  ssize_t n = real(fd, buf, count);
  if (tracked && n > 0) journal(OP_WRITE, path, (uint64_t)before, buf, (uint64_t)n);
  return n;
}

ssize_t IOFAULT_NAME(pwrite)(int fd, const void *buf, size_t count, off_t offset) {
  init_once();
#ifdef __APPLE__
  ssize_t (*real)(int, const void *, size_t, off_t) = pwrite;
#else
  static ssize_t (*real)(int, const void *, size_t, off_t) = NULL;
  if (!real) real = (ssize_t (*)(int, const void *, size_t, off_t))dlsym(RTLD_NEXT, "pwrite");
#endif
  ssize_t n = real(fd, buf, count, offset);
  char path[PATH_MAX];
  if (n > 0 && fd_path(fd, path, sizeof(path)))
    journal(OP_PWRITE, path, (uint64_t)offset, buf, (uint64_t)n);
  return n;
}

int IOFAULT_NAME(fsync)(int fd) {
  init_once();
#ifdef __APPLE__
  int (*real)(int) = fsync;
#else
  static int (*real)(int) = NULL;
  if (!real) real = (int (*)(int))dlsym(RTLD_NEXT, "fsync");
#endif
  int rc = real(fd);
  char path[PATH_MAX];
  if (fd_path(fd, path, sizeof(path))) journal(OP_FSYNC, path, 0, NULL, 0);
  return rc;
}

int IOFAULT_NAME(fdatasync)(int fd) {
  init_once();
#ifdef __APPLE__
  int rc = fsync(fd);   /* macOS has no fdatasync; fsync is the real call */
#else
  static int (*real)(int) = NULL;
  if (!real) real = (int (*)(int))dlsym(RTLD_NEXT, "fdatasync");
  int rc = real ? real(fd) : fsync(fd);
#endif
  char path[PATH_MAX];
  if (fd_path(fd, path, sizeof(path))) journal(OP_FDATASYNC, path, 0, NULL, 0);
  return rc;
}

int IOFAULT_NAME(rename)(const char *from, const char *to) {
  init_once();
#ifdef __APPLE__
  int (*real)(const char *, const char *) = rename;
#else
  static int (*real)(const char *, const char *) = NULL;
  if (!real) real = (int (*)(const char *, const char *))dlsym(RTLD_NEXT, "rename");
#endif
  int rc = real(from, to);
  if (rc == 0) journal(OP_RENAME, to, 0, from, strlen(from));
  return rc;
}

int IOFAULT_NAME(ftruncate)(int fd, off_t length) {
  init_once();
#ifdef __APPLE__
  int (*real)(int, off_t) = ftruncate;
#else
  static int (*real)(int, off_t) = NULL;
  if (!real) real = (int (*)(int, off_t))dlsym(RTLD_NEXT, "ftruncate");
#endif
  int rc = real(fd, length);
  char path[PATH_MAX];
  if (rc == 0 && fd_path(fd, path, sizeof(path)))
    journal(OP_FTRUNCATE, path, 0, NULL, (uint64_t)length);
  return rc;
}

int IOFAULT_NAME(unlink)(const char *path) {
  init_once();
#ifdef __APPLE__
  int (*real)(const char *) = unlink;
#else
  static int (*real)(const char *) = NULL;
  if (!real) real = (int (*)(const char *))dlsym(RTLD_NEXT, "unlink");
#endif
  int rc = real(path);
  if (rc == 0) journal(OP_UNLINK, path, 0, NULL, 0);
  return rc;
}


/* Register the interposers. Linux gets these for free via LD_PRELOAD. */
IOFAULT_INTERPOSE(iofault_write, write);
IOFAULT_INTERPOSE(iofault_pwrite, pwrite);
IOFAULT_INTERPOSE(iofault_fsync, fsync);
IOFAULT_INTERPOSE(iofault_rename, rename);
IOFAULT_INTERPOSE(iofault_ftruncate, ftruncate);
IOFAULT_INTERPOSE(iofault_unlink, unlink);
