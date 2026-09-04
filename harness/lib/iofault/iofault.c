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
#include <pthread.h>
#include <fcntl.h>
#include <stdarg.h>
#include <limits.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/param.h>
#include <sys/stat.h>
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
#define OP_DIRSYNC 8

static int journal_fd = -1;
static char root[PATH_MAX];
static size_t root_len = 0;
static atomic_ullong seq = 0;
static int initialised = 0;

static pthread_once_t init_control = PTHREAD_ONCE_INIT;

static void init_impl(void) {
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

/* pthread_once: two threads entering concurrently could both pass a plain
 * `if (initialised)` check and open the journal twice, interleaving records. */
static void init_once(void) { pthread_once(&init_control, init_impl); }

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

/* Resolve a possibly-relative path against the process cwd, then compare on
 * PATH COMPONENTS. The harness runs `ltx` with cwd=repo while IOFAULT_ROOT is
 * absolute, so raw relative paths from rename()/unlink() failed the prefix
 * test and were never journalled at all. Plain strncmp also accepted siblings:
 * "/tmp/root-other" starts with "/tmp/root". */
static int canonical_under_root(const char *path, char *out, size_t cap) {
  char joined[PATH_MAX];
  if (path[0] == '/') {
    snprintf(joined, sizeof(joined), "%s", path);
  } else {
    char cwd[PATH_MAX];
    if (!getcwd(cwd, sizeof(cwd))) return 0;
    snprintf(joined, sizeof(joined), "%s/%s", cwd, path);
  }
  /* realpath() fails for a path being created or just removed, so fall back to
   * the lexically joined form rather than dropping the record. */
  char resolved[PATH_MAX];
  if (realpath(joined, resolved) == NULL)
    snprintf(resolved, sizeof(resolved), "%s", joined);

  if (root_len == 0) return 0;
  if (strncmp(resolved, root, root_len) != 0) return 0;
  /* Component boundary: the next character must end the root or be a slash. */
  if (resolved[root_len] != '\0' && resolved[root_len] != '/') return 0;
  snprintf(out, cap, "%s", resolved);
  return 1;
}

static int under_root(const char *path) {
  char scratch[PATH_MAX];
  return canonical_under_root(path, scratch, sizeof(scratch));
}

static void journal(uint32_t op, const char *raw_path, uint64_t offset,
                    const void *payload, uint64_t length) {
  char path[PATH_MAX];
  if (journal_fd < 0 || !canonical_under_root(raw_path, path, sizeof(path)))
    return;
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
  /* Only a SUCCESSFUL fsync is a durability barrier. Journalling a failed one
   * would mark preceding writes durable that never reached the platter, so the
   * replayer would refuse to drop them and the engine would be credited for a
   * guarantee it did not get.
   *
   * File and directory syncs are recorded as DIFFERENT operations, because they
   * confer different guarantees: fsync on a file makes that file's data
   * durable, while only fsync on the containing DIRECTORY makes a rename,
   * create or unlink durable. Conflating them lets the replayer treat a plain
   * file sync as metadata durability, crediting the engine for a barrier it
   * never issued. */
  if (rc == 0 && fd_path(fd, path, sizeof(path))) {
    struct stat st;
    int is_dir = (fstat(fd, &st) == 0) && S_ISDIR(st.st_mode);
    journal(is_dir ? OP_DIRSYNC : OP_FSYNC, path, 0, NULL, 0);
  }
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

/* macOS issues its real durability barrier through fcntl, not fsync.
 *
 * `fsync(2)` on macOS returns once the data reaches the drive's volatile write
 * cache; ADR-4 measured the difference (63 us versus 4,688 us) and the platform
 * documents that it "reorders and tears un-fsynced writes". The barrier that
 * actually flushes the media is `fcntl(fd, F_FULLFSYNC)`, which is what Rust's
 * `File::sync_all` and `File::sync_data` compile to on this platform -- so an
 * engine doing exactly the right thing issued NO fsync syscall at all.
 *
 * Interposing only fsync therefore recorded no barrier for any Rust program on
 * macOS. Every write became volatile, the replayer dropped, reordered and tore
 * all of them, and the resulting corruption was attributed to the engine. That
 * is precisely the failure replay.py's own contract forbids: "a replayer that
 * violated it would fail the engine for a promise the platform never made --
 * producing 'bugs' nobody could fix."
 *
 * Declared variadic, and it must be: fcntl's third argument is passed as a
 * variadic argument, which on arm64 arrives on the STACK rather than in a
 * register. A fixed three-parameter signature would read a register the caller
 * never set and forward garbage for commands like F_SETFD. */
int IOFAULT_NAME(fcntl)(int fd, int cmd, ...) {
  init_once();
  va_list ap;
  va_start(ap, cmd);
  void *arg = va_arg(ap, void *);
  va_end(ap);

#ifdef __APPLE__
  int is_barrier = (cmd == F_FULLFSYNC)
#ifdef F_BARRIERFSYNC
                   || (cmd == F_BARRIERFSYNC)
#endif
      ;
  if (is_barrier) {
    /* Two arguments: these commands ignore the third, and passing one we
     * invented would be a lie to the kernel. */
    int rc = fcntl(fd, cmd);
    char path[PATH_MAX];
    /* Same rule as fsync: only a SUCCESSFUL barrier counts, and a directory
     * sync is a different guarantee from a file sync. */
    if (rc != -1 && fd_path(fd, path, sizeof(path))) {
      struct stat st;
      int is_dir = (fstat(fd, &st) == 0) && S_ISDIR(st.st_mode);
      journal(is_dir ? OP_DIRSYNC : OP_FSYNC, path, 0, NULL, 0);
    }
    return rc;
  }
  return fcntl(fd, cmd, arg);
#else
  /* Linux has no F_FULLFSYNC; sync_all() is a real fsync there and the fsync
   * interposer already sees it. Forward untouched. */
  static int (*real)(int, int, ...) = NULL;
  if (!real) real = (int (*)(int, int, ...))dlsym(RTLD_NEXT, "fcntl");
  return real(fd, cmd, arg);
#endif
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
  if (rc == 0) {
    /* The source is recorded canonically too, so replay can find it. */
    char from_abs[PATH_MAX];
    if (!canonical_under_root(from, from_abs, sizeof(from_abs)))
      snprintf(from_abs, sizeof(from_abs), "%s", from);
    journal(OP_RENAME, to, 0, from_abs, strlen(from_abs));
  }
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
IOFAULT_INTERPOSE(iofault_fcntl, fcntl);
IOFAULT_INTERPOSE(iofault_rename, rename);
IOFAULT_INTERPOSE(iofault_ftruncate, ftruncate);
IOFAULT_INTERPOSE(iofault_unlink, unlink);
