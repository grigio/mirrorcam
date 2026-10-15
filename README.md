# MirrorCam

[![CI](https://github.com/grigio/mirrorcam/actions/workflows/ci.yml/badge.svg)](https://github.com/grigio/mirrorcam/actions/workflows/ci.yml)

A minimal, borderless camera overlay for Wayland, written in Rust with
**GTK4 + libadwaita** and **GStreamer**. Inspired by
[grigio/borderless-camera](https://github.com/grigio/borderless-camera),
with auto-hiding panels and a camera/resolution selector.

## Features

* **Borderless overlay** — no title bar, black letterboxed canvas with
  **rounded corners** (the toplevel is transparent, the canvas paints the
  12 px radius, so the backdrop shows through the four corners).
* **A normal window** — no layer shell, just a regular undecorated
  `xdg_toplevel`: clicking it raises it above everything else (no pinning
  needed), it takes keyboard focus, and it shows up in the compositor's
  window switcher (`Alt+Tab`) like any other application.
* **Adwaita header, auto-hiding** — one `AdwHeaderBar` sits flush with the top
  edge (rounded bottom corners) and carries every control: the selectors on
  the left, the app title in the middle, mirror, the hamburger and the
  **close ✕ at the far right**. Only the ✕ is shown — no minimise / maximise,
  they make no sense for an overlay — and it sits on the end side, never on
  the left; quitting also works from the hamburger menu, on `q` and on
  `Ctrl+Q`. Move the pointer
  into the window to reveal it, it fades out after ~1.6 s of idleness;
  hovering the header or an open menu keeps it up, leaving the window hides it
  immediately.
* **Resize from the edges** — invisible 14 px grips on the right, bottom and
  bottom-right corner; dragging one resizes the window 1:1 with the pointer
  (minimum 240×160), without fighting the drag-to-move gesture.
* **Hamburger menu** — `☰` (instead of an info button) opens the primary menu
  with *About MirrorCam* and *Quit*, as ordinary application actions; the
  About dialog is a normal transient window the compositor stacks **centred
  over the app**, not in a corner of the desktop.
* **Toasts at the bottom** — `AdwToast` at the bottom of the window is the
  only message channel: errors appear there, and so does the camera info
  (`XiaoMi WebCam (V4L2) · 1920x1080 · 30 fps`) — *only* when the camera or
  resolution changes, then it fades away. The header has no permanent status
  line, so it always fits.
* **Drag to move** — dragging the video (or the header bar) sends one
  `xdg_toplevel.move`, so the compositor drags the window 1:1, keeps it on
  screen and raises it. A 4 px threshold tells a real drag apart from a
  click, so clicking the video still just focuses the window.
* **Camera selector** — appears only when the machine has more than one
  camera; cameras are hot-plug aware (`GstDeviceMonitor`), and unplugging the
  active camera is handled without crashing. The header gives the title up
  when the selector needs the room, and button labels ellipsize, so the bar
  always fits the window.
* **Resolution selector** — lists every size the selected camera offers, with
  the best frame rate for each (`1920x1080 · 30 fps`, `640x480 · 30 fps`, …);
  the button shows just the size, the menu keeps the fps.
* **Live camera info** — the toast text is measured from the frames that
  actually reach the sink (pad probe on the sink), so the fps is real.
* **Mirror** (selfie flip) toggle in the header.

### The 30 fps fix

The camera (XiaoMi WebCam, `uvcvideo`) advertises **raw YUYV 1920x1080 at only
5 fps**, while **MJPEG 1920x1080 runs at 30 fps**. The first pipeline forced
`video/x-raw`, so GStreamer negotiated a 5 fps mode — that was the "slow video".
The pipeline now prefers `image/jpeg` and decodes it with `jpegdec`
(libjpeg-turbo): **30 fps measured, ~15 % of one core at 1080p**. No hardware
JPEG decoder plugin is installed on this machine (`vaapi`/`v4l2codecs` are
present but expose no JPEG decoder), so software decode is the fast path here.

## Install

Every tagged release ships a **standalone binary** and a **Flatpak bundle**
on the [Releases page](https://github.com/grigio/mirrorcam/releases):

```sh
# flatpak bundle (needs the GNOME runtime: flatpak install flathub org.gnome.Platform//50)
flatpak install --user mirrorcam-*.flatpak
flatpak run io.github.grigio.mirrorcam
```

## Build

```sh
# deps: gtk4, libadwaita, gstreamer (base/good/gtk4 plugins)
cargo build --release
```

Flatpak, from a checkout (same manifest CI uses):

```sh
python3 build-aux/gen-cargo-sources.py   # vendored crates from Cargo.lock
flatpak-builder --user --install --force-clean build-dir io.github.grigio.mirrorcam.yml
```

## Run

```sh
./target/release/mirrorcam
```

| input | action |
|---|---|
| move pointer into window | reveal the header |
| click on the window | raise it to the foreground and focus it |
| `Alt+Tab` | MirrorCam is a regular window, it is in the switcher |
| `m` | toggle mirror |
| `q` / `Esc` / `Ctrl+Q` | quit |
| `☰` → *About MirrorCam* | about dialog, centred over the window |
| `☰` → *Quit* | quit |
| ✕ (right end of the header) | close the window and quit |
| drag the header bar or the video | move the window |
| drag the right / bottom edge or the corner | resize the window |

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
| `src/main.rs` | UI, panels, autohide, shortcuts, selectors, move/resize |
| `src/camera.rs` | device discovery, hotplug, format list, GStreamer pipeline |
| `data/` | `.desktop`, AppStream metainfo, scalable icon |
| `io.github.grigio.mirrorcam.yml` | Flatpak manifest (GNOME 50 runtime) |
| `build-aux/gen-cargo-sources.py` | `Cargo.lock` → `cargo-sources.json` (Flatpak vendoring) |
| `build-aux/bump-version.sh` | bump `Cargo.toml` + metainfo `<release>` in one shot |
| `.github/workflows/ci.yml` | fmt/clippy/build/test, release binaries + Flatpak on tags |

MIT licensed.
