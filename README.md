# MirrorCam

A minimal, borderless camera overlay for Wayland, written in Rust with
**GTK4 + libadwaita** and **GStreamer**. Inspired by
[grigio/borderless-camera](https://github.com/grigio/borderless-camera),
with auto-hiding panels and a camera/resolution selector.

## Features

* **Borderless overlay** — no title bar, black letterboxed canvas.
* **Always on top** — uses `gtk4-layer-shell` when the compositor supports it
  (detected at build time by `build.rs`; without the library the app still
  builds and runs as a plain undecorated window).
* **Adwaita header, auto-hiding** — one header bar at the top edge holds every
  control (app title on the left, selectors and buttons on the right), with the
  `Camera · WxH · N fps` status line as a pill just below it. Move the pointer
  into the window to reveal it, it fades out after ~1.6 s of idleness;
  hovering the header or an open menu keeps it up, leaving the window hides it
  immediately.
* **Hamburger menu** — `☰` (instead of an info button) opens the primary menu
  with *About MirrorCam* and *Quit*, as ordinary application actions.
* **Toasts at the bottom** — errors are reported with an `AdwToast` at the
  bottom of the window (the header never turns into an error banner).
* **Drag to move** — on a layer surface the window is moved through its
  margins, so it can be repositioned anywhere on the screen.
* **Camera selector** — appears only when the machine has more than one
  camera; cameras are hot-plug aware (`GstDeviceMonitor`), and unplugging the
  active camera is handled without crashing. The header gives the title up
  when the selector needs the room, and button labels ellipsize, so the bar
  always fits the window.
* **Resolution selector** — lists every size the selected camera offers, with
  the best frame rate for each (`1920x1080 · 30 fps`, `640x480 · 30 fps`, …);
  the button shows just the size, the menu keeps the fps.
* **Live status** — `Camera · WxH · N fps`, measured from the frames that
  actually reach the sink (pad probe), refreshed once a second.
* **Mirror** (selfie flip) and **pin** (always on top) toggles in the header.

### The 30 fps fix

The camera (XiaoMi WebCam, `uvcvideo`) advertises **raw YUYV 1920x1080 at only
5 fps**, while **MJPEG 1920x1080 runs at 30 fps**. The first pipeline forced
`video/x-raw`, so GStreamer negotiated a 5 fps mode — that was the "slow video".
The pipeline now prefers `image/jpeg` and decodes it with `jpegdec`
(libjpeg-turbo): **30 fps measured, ~15 % of one core at 1080p**. No hardware
JPEG decoder plugin is installed on this machine (`vaapi`/`v4l2codecs` are
present but expose no JPEG decoder), so software decode is the fast path here.

## Build

```sh
# deps: gtk4, libadwaita, gstreamer (base/good/gtk4 plugins), optionally gtk4-layer-shell
cargo build --release
```

`build.rs` probes `pkg-config --exists gtk4-layer-shell-0` and enables the
`has_layer_shell` cfg accordingly.

## Run

```sh
./target/release/mirrorcam
```

| input | action |
|---|---|
| move pointer into window | reveal the header |
| `m` | toggle mirror |
| `p` | toggle always-on-top (layer shell) |
| `q` / `Esc` / `Ctrl+Q` | quit |
| `☰` → *About MirrorCam* | about dialog |
| `☰` → *Quit* | quit |
| drag anywhere on the video | move the window |

## Testing

GUI behaviour was verified in a nested, isolated Wayland session with the
[virtual-wayland](https://github.com/) skill (screenshots + pixel diffs, virtual
pointer only — the real desktop is never touched):

```sh
VW=~/.pi/agent/skills/virtual-wayland/scripts/vw
$VW start --headless
eval $($VW env)
$VW run ./target/debug/mirrorcam
$VW shot /tmp/a.png
$VW input mousemove -a -x 390 -y 300
```

## Layout

| file | role |
|---|---|
| `src/main.rs` | UI, panels, autohide, shortcuts, selectors, status |
| `src/camera.rs` | device discovery, hotplug, format list, GStreamer pipeline |
| `src/layer_shell.rs` | thin `gtk4-layer-shell` wrapper |
| `build.rs` | detects `gtk4-layer-shell` at build time |

MIT licensed.
