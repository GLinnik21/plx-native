/* SPDX-License-Identifier: MIT */
#define _GNU_SOURCE
#ifndef PROBE_HEADLESS_ONLY
#include <SDL2/SDL.h>
#include <SDL2/SDL_ttf.h>
#endif
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#if !defined(O_DIRECTORY) || !defined(O_NOFOLLOW) || !defined(O_NONBLOCK)
#error "storage probe requires O_DIRECTORY, O_NOFOLLOW and O_NONBLOCK"
#endif

#define APP_ID "com.beb.plxnative.storageprobe"
#define MARKER "plxnative-storage-probe-v1\n"
#define MAX_PRIOR 128
#define BACK_WEBOS ((SDL_Keycode)1073742094)

typedef struct {
  const char *name;
  char dir_meta[128];
  char prior[160];
  char create[96];
  char write[96];
  char file_sync[96];
  char close_result[96];
  char rename_result[96];
  char dir_sync[96];
} Probe;

static void result(char *dst, size_t cap, const char *op, int err) {
  if (err == 0) snprintf(dst, cap, "%s: OK", op);
  else snprintf(dst, cap, "%s: ERROR errno=%d (%s)", op, err, strerror(err));
}

static bool trusted_prior(int dirfd, Probe *p, bool *absent) {
  *absent = false;
  int fd = openat(dirfd, "probe.dat", O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
  if (fd < 0) {
    if (errno == ENOENT) {
      *absent = true;
      snprintf(p->prior, sizeof p->prior, "Prior: NEW (probe.dat absent)");
    } else {
      snprintf(p->prior, sizeof p->prior, "Prior: ERROR open errno=%d (%s)", errno, strerror(errno));
    }
    return false;
  }
  struct stat st;
  if (fstat(fd, &st) != 0) {
    snprintf(p->prior, sizeof p->prior, "Prior: ERROR fstat errno=%d (%s)", errno, strerror(errno));
    close(fd);
    return false;
  }
  if (!S_ISREG(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0777) != 0600 || st.st_size < 0 || st.st_size > MAX_PRIOR) {
    snprintf(p->prior, sizeof p->prior,
             "Prior: ERROR trust type=%s uid=%lu mode=%03o size=%lld",
             S_ISREG(st.st_mode) ? "file" : "other", (unsigned long)st.st_uid,
             (unsigned)(st.st_mode & 0777), (long long)st.st_size);
    close(fd);
    return false;
  }
  char bytes[MAX_PRIOR + 1];
  ssize_t total = 0;
  while (total < st.st_size) {
    ssize_t n = read(fd, bytes + total, (size_t)(st.st_size - total));
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) {
      snprintf(p->prior, sizeof p->prior, "Prior: ERROR read errno=%d (%s)", n < 0 ? errno : 0,
               n < 0 ? strerror(errno) : "early EOF");
      close(fd);
      return false;
    }
    total += n;
  }
  close(fd);
  bytes[total] = '\0';
  if ((size_t)total != strlen(MARKER) || memcmp(bytes, MARKER, strlen(MARKER)) != 0) {
    snprintf(p->prior, sizeof p->prior, "Prior: ERROR marker mismatch");
    return false;
  }
  snprintf(p->prior, sizeof p->prior, "Prior: PRIOR FOUND (trusted marker)");
  return true;
}

static void run_probe(int rootfd, Probe *p, unsigned serial) {
  struct stat st;
  if (fstatat(rootfd, p->name, &st, AT_SYMLINK_NOFOLLOW) != 0) {
    snprintf(p->dir_meta, sizeof p->dir_meta, "Directory: ERROR lstat errno=%d (%s)", errno, strerror(errno));
    return;
  }
  snprintf(p->dir_meta, sizeof p->dir_meta, "Directory: uid=%lu gid=%lu mode=%03o type=%s",
           (unsigned long)st.st_uid, (unsigned long)st.st_gid, (unsigned)(st.st_mode & 0777),
           S_ISDIR(st.st_mode) ? "dir" : "other");
  int dirfd = openat(rootfd, p->name, O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
  if (dirfd < 0) {
    snprintf(p->prior, sizeof p->prior, "Prior: ERROR open-directory errno=%d (%s)", errno, strerror(errno));
    return;
  }
  bool absent = false;
  bool prior_ok = trusted_prior(dirfd, p, &absent);
  if (!absent && !prior_ok) {
    snprintf(p->create, sizeof p->create, "Create temp: SKIPPED (untrusted prior)");
    close(dirfd);
    return;
  }

  char temp[80];
  snprintf(temp, sizeof temp, ".probe.tmp.%lu.%u", (unsigned long)getpid(), serial);
  int fd = openat(dirfd, temp, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
  int create_err = fd < 0 ? errno : 0;
  result(p->create, sizeof p->create, "Create temp", create_err);
  if (fd < 0) { close(dirfd); return; }

  const char *at = MARKER;
  size_t left = strlen(MARKER);
  int write_err = 0;
  while (left) {
    ssize_t n = write(fd, at, left);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) { write_err = n < 0 ? errno : EIO; break; }
    at += n; left -= (size_t)n;
  }
  result(p->write, sizeof p->write, "Write marker", write_err);
  int sync_err = 0;
  if (write_err) snprintf(p->file_sync, sizeof p->file_sync, "Fsync file: SKIPPED");
  else {
    if (fsync(fd) != 0) sync_err = errno;
    result(p->file_sync, sizeof p->file_sync, "Fsync file", sync_err);
  }
  int close_err = close(fd) == 0 ? 0 : errno;
  result(p->close_result, sizeof p->close_result, "Close temp", close_err);
  if (write_err || sync_err || close_err) {
    unlinkat(dirfd, temp, 0);
    close(dirfd);
    return;
  }
  int rename_err = renameat(dirfd, temp, dirfd, "probe.dat") == 0 ? 0 : errno;
  result(p->rename_result, sizeof p->rename_result, "Rename", rename_err);
  if (rename_err) unlinkat(dirfd, temp, 0);
  int dir_sync_err = 0;
  if (rename_err) snprintf(p->dir_sync, sizeof p->dir_sync, "Fsync directory: SKIPPED");
  else {
    if (fsync(dirfd) != 0) dir_sync_err = errno;
    result(p->dir_sync, sizeof p->dir_sync, "Fsync directory", dir_sync_err);
  }
  close(dirfd);
}

static bool exe_root(char out[PATH_MAX]) {
  ssize_t n = readlink("/proc/self/exe", out, PATH_MAX - 1);
  if (n <= 0) return false;
  out[n] = '\0';
  char *slash = strrchr(out, '/');
  if (!slash) return false;
  *slash = '\0';
  return true;
}

#ifndef PROBE_HEADLESS_ONLY
static int draw_text(SDL_Renderer *r, TTF_Font *font, int x, int y, int width,
                     SDL_Color color, const char *s) {
  SDL_Surface *surface = TTF_RenderUTF8_Blended_Wrapped(font, s, color, (Uint32)width);
  if (!surface) return 0;
  SDL_Texture *texture = SDL_CreateTextureFromSurface(r, surface);
  SDL_Rect dst = {x, y, surface->w, surface->h};
  SDL_FreeSurface(surface);
  if (texture) {
    SDL_RenderCopy(r, texture, NULL, &dst);
    SDL_DestroyTexture(texture);
  }
  return dst.h;
}

static int show_ui(const char *root, Probe probes[3]) {
  SDL_SetHint("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
  if (SDL_Init(SDL_INIT_VIDEO) != 0 || TTF_Init() != 0) return 2;
  SDL_Window *w = SDL_CreateWindow("PlxNative storage probe", SDL_WINDOWPOS_CENTERED,
                                   SDL_WINDOWPOS_CENTERED, 1920, 1080,
                                   SDL_WINDOW_SHOWN | SDL_WINDOW_FULLSCREEN_DESKTOP);
  SDL_Renderer *r = w ? SDL_CreateRenderer(w, -1, SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC) : NULL;
  char font_path[PATH_MAX + sizeof "/appfont.ttf"];
  snprintf(font_path, sizeof font_path, "%s/appfont.ttf", root);
  TTF_Font *font = TTF_OpenFont(font_path, 24);
  TTF_Font *title = TTF_OpenFont(font_path, 38);
  if (!w || !r || !font || !title) return 2;
  if (SDL_RenderSetLogicalSize(r, 1920, 1080) != 0) return 2;
  bool running = true, redraw = true;
  while (running) {
    if (!redraw) {
      SDL_Event e;
      if (!SDL_WaitEvent(&e)) continue;
      if (e.type == SDL_QUIT || (e.type == SDL_KEYDOWN &&
          (e.key.keysym.sym == SDLK_ESCAPE || e.key.keysym.sym == BACK_WEBOS))) running = false;
      redraw = e.type == SDL_WINDOWEVENT && e.window.event == SDL_WINDOWEVENT_EXPOSED;
      continue;
    }
    redraw = false;
    SDL_SetRenderDrawColor(r, 20, 22, 27, 255); SDL_RenderClear(r);
    SDL_Color white = {238, 240, 244, 255}, dim = {174, 181, 194, 255}, accent = {116, 190, 255, 255};
    draw_text(r, title, 70, 45, 1780, white, "PlxNative storage probe — no account or network access");
    char proc[180];
    snprintf(proc, sizeof proc, "App %s   uid=%lu gid=%lu pid=%lu", APP_ID,
             (unsigned long)geteuid(), (unsigned long)getegid(), (unsigned long)getpid());
    draw_text(r, font, 70, 100, 1780, dim, proc);
    int x[3] = {70, 690, 1310};
    for (int i = 0; i < 3; i++) {
      int y = 175; draw_text(r, title, x[i], y, 560, accent, probes[i].name); y += 55;
      const char *lines[] = {probes[i].dir_meta, probes[i].prior, probes[i].create, probes[i].write,
                             probes[i].file_sync, probes[i].close_result,
                             probes[i].rename_result, probes[i].dir_sync};
      for (unsigned j = 0; j < sizeof lines / sizeof lines[0]; j++)
        if (lines[j][0]) y += draw_text(r, font, x[i], y, 560, white, lines[j]) + 10;
    }
    draw_text(r, font, 70, 980, 1780, dim,
              "First launch: photograph NEW/results. Press BACK to exit fully. Reopen and photograph PRIOR FOUND/results.");
    draw_text(r, font, 70, 1020, 1780, dim, "Nothing is uploaded. Only test0755, test0775 and test0777 beside this executable are accessed.");
    SDL_RenderPresent(r);
  }
  TTF_CloseFont(title); TTF_CloseFont(font); SDL_DestroyRenderer(r); SDL_DestroyWindow(w);
  TTF_Quit(); SDL_Quit(); return 0;
}
#endif

int main(int argc, char **argv) {
  bool headless = argc >= 2 && strcmp(argv[1], "--headless") == 0;
  char root[PATH_MAX];
  if (headless) {
    if (argc != 4 || strcmp(argv[2], "--root") != 0 || argv[3][0] != '/' || strlen(argv[3]) >= sizeof root) {
      fprintf(stderr, "usage: %s --headless --root /absolute/temp/root\n", argv[0]); return 2;
    }
    snprintf(root, sizeof root, "%s", argv[3]);
  } else if (!exe_root(root)) {
    return 2;
  }
  int rootfd = open(root, O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
  if (rootfd < 0) { fprintf(stderr, "root open errno=%d (%s)\n", errno, strerror(errno)); return 2; }
  Probe probes[3] = {{.name="test0755"}, {.name="test0775"}, {.name="test0777"}};
  for (unsigned i = 0; i < 3; i++) run_probe(rootfd, &probes[i], i + 1);
  close(rootfd);
  if (!headless) {
#ifdef PROBE_HEADLESS_ONLY
    fprintf(stderr, "this build supports --headless only\n"); return 2;
#else
    return show_ui(root, probes);
#endif
  }
  printf("uid=%lu gid=%lu pid=%lu root=%s\n", (unsigned long)geteuid(),
         (unsigned long)getegid(), (unsigned long)getpid(), root);
  for (int i = 0; i < 3; i++) printf("[%s]\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n", probes[i].name,
      probes[i].dir_meta, probes[i].prior, probes[i].create, probes[i].write,
      probes[i].file_sync, probes[i].close_result, probes[i].rename_result, probes[i].dir_sync);
  return 0;
}
