// SPDX-License-Identifier: MIT
// Test probe for the cornice live test (scripts/live/run.sh). Not part of cornice.
//
//   probe win        map an xdg_toplevel filled with PROBE_COLOR (default 00ff00), flashing
//                    PROBE_FLASH (default f0f0f0) for 250 ms on every button press
//                    and log pointer/configure events to stdout
//   probe vp         zwlr_virtual_pointer_v1; commands on stdin, one per line:
//                      at X Y           move to screen X Y (warps to 0,0 first, then a relative move)
//                      abs X Y          absolute move (extents from PROBE_W x PROBE_H)
//                      move DX DY       relative move
//                      press [btn]      button press   (btn: left|right|middle, default left)
//                      release [btn]
//                      click [btn]      press + release at the current position
//                      quit
//
// Build: scripts/live/run.sh does this; needs wayland-client + wayland-scanner.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/mman.h>

#include <wayland-client.h>
#include <linux/input-event-codes.h>

#include "xdg-shell-client-protocol.h"
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct wl_seat *seat;
static struct xdg_wm_base *wm_base;
static struct zwlr_virtual_pointer_manager_v1 *vp_manager;
static struct zwlr_virtual_pointer_v1 *vp;

static struct wl_display *display;
static struct wl_surface *surface;
static struct xdg_surface *xdg_surface;
static struct xdg_toplevel *toplevel;

static uint32_t color = 0x00ff00; /* RGB */
static uint32_t flash = 0xf0f0f0; /* RGB, shown for a moment on every button press */
static int pressed;
static int32_t extent_w = 1280, extent_h = 720;
static int32_t win_w, win_h;

static uint32_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)(ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}

static void die(const char *what) {
    fprintf(stderr, "probe: %s: %s\n", what, strerror(errno));
    exit(1);
}

/* ---- shm ---- */

static void draw(void) {
    int stride = win_w * 4, size = stride * win_h;
    int fd = memfd_create("probe", MFD_CLOEXEC);
    if (fd < 0) die("memfd_create");
    if (ftruncate(fd, size) < 0) die("ftruncate");
    uint32_t *px = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (px == MAP_FAILED) die("mmap");
    /* wl_shm ARGB8888: the 32-bit value is 0xAARRGGBB, which lands in memory as B,G,R,A */
    uint32_t c = pressed ? flash : color;
    for (int i = 0; i < win_w * win_h; i++)
        px[i] = 0xff000000u | c;
    /* ponytail: pool and mapping are never freed — the probe is a short-lived test process */
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, size);
    struct wl_buffer *buf = wl_shm_pool_create_buffer(pool, 0, win_w, win_h, stride, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    wl_surface_attach(surface, buf, 0, 0);
    wl_surface_damage_buffer(surface, 0, 0, win_w, win_h);
    wl_surface_commit(surface);
}

/* ---- xdg toplevel ---- */

static void xdg_surface_configure(void *_, struct xdg_surface *s, uint32_t serial) {
    xdg_surface_ack_configure(s, serial);
    draw();
    printf("win: configured %dx%d\n", win_w, win_h);
    fflush(stdout);
}

static const struct xdg_surface_listener xdg_surface_listener = { .configure = xdg_surface_configure };

static void toplevel_configure(void *_, struct xdg_toplevel *t, int32_t w, int32_t h, struct wl_array *states) {
    (void)t; (void)states;
    /* a 0 dimension means "you choose"; river forwards the WM's dimensions, but be safe */
    win_w = w > 0 ? w : 400;
    win_h = h > 0 ? h : 300;
}

static void toplevel_close(void *_, struct xdg_toplevel *t) {
    (void)t;
    printf("win: close\n");
    fflush(stdout);
    exit(0);
}

static const struct xdg_toplevel_listener toplevel_listener = {
    .configure = toplevel_configure, .close = toplevel_close,
};

static void wm_ping(void *_, struct xdg_wm_base *b, uint32_t serial) { xdg_wm_base_pong(b, serial); }
static const struct xdg_wm_base_listener wm_base_listener = { .ping = wm_ping };

/* ---- pointer (win mode) ---- */

static void ptr_enter(void *_, struct wl_pointer *p, uint32_t serial, struct wl_surface *s,
                      wl_fixed_t x, wl_fixed_t y) {
    (void)p; (void)s;
    printf("win: pointer enter %.1f %.1f\n", wl_fixed_to_double(x), wl_fixed_to_double(y));
    fflush(stdout);
}
static void ptr_leave(void *_, struct wl_pointer *p, uint32_t serial, struct wl_surface *s) {
    (void)p; (void)s;
    printf("win: pointer leave\n"); fflush(stdout);
}
static void ptr_motion(void *_, struct wl_pointer *p, uint32_t t, wl_fixed_t x, wl_fixed_t y) {
    (void)p; (void)t;
    printf("win: pointer motion %.1f %.1f\n", wl_fixed_to_double(x), wl_fixed_to_double(y));
    fflush(stdout);
}
static void ptr_button(void *_, struct wl_pointer *p, uint32_t serial, uint32_t t,
                       uint32_t button, uint32_t state) {
    (void)p; (void)serial; (void)t;
    printf("win: pointer button %u %s\n", button, state == WL_POINTER_BUTTON_STATE_PRESSED ? "press" : "release");
    fflush(stdout);
    if (state == WL_POINTER_BUTTON_STATE_PRESSED) {
        /* a visible reaction, so a screenshot proves the click arrived */
        pressed = 1;
        draw();
    }
}
static void ptr_axis(void *_, struct wl_pointer *p, uint32_t t, uint32_t axis, wl_fixed_t v) {
    (void)p; (void)t; (void)axis; (void)v;
}
static void ptr_frame(void *_, struct wl_pointer *p) { (void)p; }
static void ptr_axis_source(void *_, struct wl_pointer *p, uint32_t s) { (void)p; (void)s; }
static void ptr_axis_stop(void *_, struct wl_pointer *p, uint32_t t, uint32_t a) { (void)p; (void)t; (void)a; }
static void ptr_axis_discrete(void *_, struct wl_pointer *p, uint32_t a, int32_t d) { (void)p; (void)a; (void)d; }

static const struct wl_pointer_listener pointer_listener = {
    .enter = ptr_enter, .leave = ptr_leave, .motion = ptr_motion, .button = ptr_button,
    .axis = ptr_axis, .frame = ptr_frame, .axis_source = ptr_axis_source,
    .axis_stop = ptr_axis_stop, .axis_discrete = ptr_axis_discrete,
};

/* ---- seat ---- */

static void seat_caps(void *_, struct wl_seat *s, uint32_t caps) {
    if ((caps & WL_SEAT_CAPABILITY_POINTER) && surface) {
        static struct wl_pointer *pointer;
        if (!pointer) {
            pointer = wl_seat_get_pointer(s);
            wl_pointer_add_listener(pointer, &pointer_listener, NULL);
        }
    }
}
static void seat_name(void *_, struct wl_seat *s, const char *name) {
    printf("seat: %s\n", name); fflush(stdout);
}
static const struct wl_seat_listener seat_listener = { .capabilities = seat_caps, .name = seat_name };

/* ---- registry ---- */

static void reg_global(void *_, struct wl_registry *reg, uint32_t name, const char *iface, uint32_t version) {
    if (!strcmp(iface, wl_compositor_interface.name))
        compositor = wl_registry_bind(reg, name, &wl_compositor_interface, version < 4 ? version : 4);
    else if (!strcmp(iface, wl_shm_interface.name))
        shm = wl_registry_bind(reg, name, &wl_shm_interface, 1);
    else if (!strcmp(iface, wl_seat_interface.name))
        seat = wl_registry_bind(reg, name, &wl_seat_interface, version < 7 ? version : 7);
    else if (!strcmp(iface, xdg_wm_base_interface.name))
        wm_base = wl_registry_bind(reg, name, &xdg_wm_base_interface, 1);
    else if (!strcmp(iface, zwlr_virtual_pointer_manager_v1_interface.name))
        vp_manager = wl_registry_bind(reg, name, &zwlr_virtual_pointer_manager_v1_interface, version);
}
static void reg_remove(void *_, struct wl_registry *r, uint32_t name) { (void)r; (void)name; }
static const struct wl_registry_listener registry_listener = { .global = reg_global, .global_remove = reg_remove };

/* ---- virtual pointer ---- */

static uint32_t button_code(const char *name) {
    if (!name || !strcmp(name, "left")) return BTN_LEFT;
    if (!strcmp(name, "right")) return BTN_RIGHT;
    if (!strcmp(name, "middle")) return BTN_MIDDLE;
    return (uint32_t)atoi(name);
}

static void vp_command(char *line) {
    char *cmd = strtok(line, " \t\n");
    if (!cmd) return;
    if (!strcmp(cmd, "abs")) {
        double x = atof(strtok(NULL, " \t\n")), y = atof(strtok(NULL, " \t\n"));
        zwlr_virtual_pointer_v1_motion_absolute(vp, now_ms(), wl_fixed_from_double(x), wl_fixed_from_double(y),
                                                extent_w, extent_h);
        zwlr_virtual_pointer_v1_frame(vp);
    } else if (!strcmp(cmd, "move")) {
        double x = atof(strtok(NULL, " \t\n")), y = atof(strtok(NULL, " \t\n"));
        zwlr_virtual_pointer_v1_motion(vp, now_ms(), wl_fixed_from_double(x), wl_fixed_from_double(y));
        zwlr_virtual_pointer_v1_frame(vp);
    } else if (!strcmp(cmd, "at")) {
        double x = atof(strtok(NULL, " \t\n")), y = atof(strtok(NULL, " \t\n"));
        /* relative motion is 1:1 and unaccelerated; a huge negative delta clamps to the layout's (0,0) */
        zwlr_virtual_pointer_v1_motion(vp, now_ms(), wl_fixed_from_double(-100000), wl_fixed_from_double(-100000));
        zwlr_virtual_pointer_v1_frame(vp);
        zwlr_virtual_pointer_v1_motion(vp, now_ms(), wl_fixed_from_double(x), wl_fixed_from_double(y));
        zwlr_virtual_pointer_v1_frame(vp);
    } else if (!strcmp(cmd, "press") || !strcmp(cmd, "release")) {
        uint32_t btn = button_code(strtok(NULL, " \t\n"));
        uint32_t state = !strcmp(cmd, "press") ? WL_POINTER_BUTTON_STATE_PRESSED : WL_POINTER_BUTTON_STATE_RELEASED;
        zwlr_virtual_pointer_v1_button(vp, now_ms(), btn, state);
        zwlr_virtual_pointer_v1_frame(vp);
    } else if (!strcmp(cmd, "click")) {
        uint32_t btn = button_code(strtok(NULL, " \t\n"));
        zwlr_virtual_pointer_v1_button(vp, now_ms(), btn, WL_POINTER_BUTTON_STATE_PRESSED);
        zwlr_virtual_pointer_v1_frame(vp);
        usleep(30000);
        zwlr_virtual_pointer_v1_button(vp, now_ms(), btn, WL_POINTER_BUTTON_STATE_RELEASED);
        zwlr_virtual_pointer_v1_frame(vp);
    } else if (!strcmp(cmd, "quit")) {
        exit(0);
    }
}

int main(int argc, char **argv) {
    if (argc < 2 || (strcmp(argv[1], "win") && strcmp(argv[1], "vp"))) {
        fprintf(stderr, "usage: probe win|vp\n");
        return 2;
    }
    int is_win = !strcmp(argv[1], "win");

    const char *c = getenv("PROBE_COLOR");
    if (c) color = (uint32_t)strtoul(c, NULL, 16);
    c = getenv("PROBE_FLASH");
    if (c) flash = (uint32_t)strtoul(c, NULL, 16);
    if (getenv("PROBE_W")) extent_w = atoi(getenv("PROBE_W"));
    if (getenv("PROBE_H")) extent_h = atoi(getenv("PROBE_H"));

    display = wl_display_connect(NULL);
    if (!display) { fprintf(stderr, "probe: cannot connect to Wayland\n"); return 1; }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!compositor || !shm || !seat) { fprintf(stderr, "probe: missing compositor/shm/seat\n"); return 1; }
    wl_seat_add_listener(seat, &seat_listener, NULL);

    if (is_win) {
        if (!wm_base) { fprintf(stderr, "probe: no xdg_wm_base\n"); return 1; }
        xdg_wm_base_add_listener(wm_base, &wm_base_listener, NULL);
        surface = wl_compositor_create_surface(compositor);
        xdg_surface = xdg_wm_base_get_xdg_surface(wm_base, surface);
        xdg_surface_add_listener(xdg_surface, &xdg_surface_listener, NULL);
        toplevel = xdg_surface_get_toplevel(xdg_surface);
        xdg_toplevel_add_listener(toplevel, &toplevel_listener, NULL);
        xdg_toplevel_set_title(toplevel, "cornice-probe");
        xdg_toplevel_set_app_id(toplevel, "cornice-probe");
        wl_surface_commit(surface);
        wl_display_roundtrip(display);
        printf("win: mapped\n");
        fflush(stdout);
        uint32_t flash_until = 0;
        while (1) {
            struct pollfd pfd = { .fd = wl_display_get_fd(display), .events = POLLIN };
            if (wl_display_dispatch_pending(display) == -1) break;
            wl_display_flush(display);
            if (poll(&pfd, 1, 100) > 0 && (pfd.revents & POLLIN) && wl_display_dispatch(display) == -1) break;
            if (pressed) {
                if (!flash_until) flash_until = now_ms() + 250;
                else if ((int32_t)(now_ms() - flash_until) >= 0) { pressed = 0; flash_until = 0; draw(); }
            }
        }
        return 0;
    }

    if (!vp_manager) { fprintf(stderr, "probe: no zwlr_virtual_pointer_manager_v1\n"); return 1; }
    vp = zwlr_virtual_pointer_manager_v1_create_virtual_pointer(vp_manager, seat);
    wl_display_roundtrip(display);
    printf("vp: ready\n");
    fflush(stdout);

    while (1) {
        struct pollfd fds[2] = {
            { .fd = wl_display_get_fd(display), .events = POLLIN },
            { .fd = STDIN_FILENO, .events = POLLIN },
        };
        if (poll(fds, 2, -1) < 0) { if (errno == EINTR) continue; die("poll"); }
        if (fds[0].revents & POLLIN) {
            if (wl_display_dispatch(display) == -1) return 1;
        }
        if (fds[1].revents & POLLIN) {
            char line[256];
            if (!fgets(line, sizeof line, stdin)) {
                /* keep running: stdin may just be closed early */
                fds[1].fd = -1;
                continue;
            }
            vp_command(line);
            wl_display_flush(display);
        }
    }
}
