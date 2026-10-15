# AGENTS.md — MirrorCam

## Identity

- App ID: `io.github.grigio.mirrorcam` (reverse-DNS GitHub id, like
  Carosello). It is hardcoded **once**, as `APP_ID` in `src/main.rs`, and
  repeated in the Flatpak manifest, the `.desktop`, the metainfo and the
  icon filename — all five must stay identical or the Flatpak export
  links the desktop file to the wrong app. `org.mirrorcam.Camera` was the
  old id, renamed for Flathub/FlatPark compatibility.
- About dialog icon is `APP_ID`, so a local `cargo run` shows a generic
  icon unless the hicolor icon is installed — expected, not a bug.

## Versioning

- `Cargo.toml` is the single source of truth. The About dialog reads
  `env!("CARGO_PKG_VERSION")`; the Flatpak's user-visible version comes
  from the metainfo `<release>` entries.
- Bump with `build-aux/bump-version.sh <ver> ["notes"]` — updates
  `Cargo.toml` and prepends a metainfo `<release>`. Then tag `v<ver>`.
- CI `versions` job fails on any drift between Cargo / metainfo / git
  tag, so a hand-edited version that skips the script turns the release
  red before anything is built.

## Flatpak

- Manifest `io.github.grigio.mirrorcam.yml`, `buildsystem: simple`
  (plain Cargo — no meson): `cargo --offline --frozen build --release`
  then `install` of binary + desktop + metainfo + icon.
- **`cargo-sources.json` is gitignored** and regenerated from `Cargo.lock`
  by `build-aux/gen-cargo-sources.py` (CI runs it in the `flatpak` job;
  run it by hand before a local `flatpak-builder`). Never hand-edit it,
  never commit it — after any dependency change just re-run the script.
  Each lockfile package becomes an `archive` + an inline
  `.cargo-checksum.json` entry, plus a trailing `shell` source that
  writes `.cargo/config.toml` pointing at the vendored directory
  `/run/build/mirrorcam/cargo/vendored` (flatpak-builder builds each
  module in `/run/build/<module name>`).
- `CARGO_HOME=/run/build-mirrorcam/cargo` in the manifest keeps cargo's
  cache out of the read-only source tree (Carosello does the same).
- **Runtime bump = 2 places**: `runtime-version: 'NN'` in the manifest
  and the CI image `ghcr.io/flathub-infra/flatpak-github-actions:gnome-NN`
  (same NN). `sdk-extensions: rust-stable` resolves against the
  **freedesktop** branch underneath GNOME, not GNOME's number: GNOME 50 →
  freedesktop 25.08 → `rust-stable//25.08`. There is no `rust-stable//50`.
- **Camera access in the sandbox**: `--device=all` (v4l2 `/dev/video*`,
  the provider that actually reports the XiaoMi webcam) plus
  `--filesystem=xdg-run/pipewire-0` for machines whose camera is exposed
  through PipeWire. `GstDeviceMonitor` probes both, so both grants are
  needed for the selector to be complete.
- The GNOME Platform runtime ships `v4l2src`, `jpegdec` (gst-plugins-good)
  and `gtk4paintablesink`, so the pipeline needs no extra flatpak modules.
- Local verify (GNOME 50 + rust-stable 25.08 installed):
  `flatpak-builder --user --install --force-clean build-dir io.github.grigio.mirrorcam.yml`
  then run it in a **nested Wayland session** (see "GUI development"
  below — the Flatpak runs there fine and still sees the camera):
  `timeout 8 flatpak run io.github.grigio.mirrorcam` — must print the
  camera line and have an **empty stderr** (a panic or a missing plugin
  shows up there; exit 124 is just the timeout).

## CI (`.github/workflows/ci.yml`)

- `build` runs in `container: ubuntu:26.04` (root, no sudo, no
  toolchain — Rust comes from `dtolnay/rust-toolchain@stable` with
  `rustfmt, clippy`). Keep the container in sync with the Cargo feature
  floor (`gtk4 v4_10`, `libadwaita v1_4`): too old a GTK/libadwaita makes
  the sys crates' build scripts exit 101 *inside the Clippy step*, which
  looks like a lint failure while nothing was linted.
- GStreamer dev packages are installed in the same apt step
  (`libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`) plus the
  runtime plugins; without them `gstreamer-sys` fails at build time.
- `flatpak` and `release` are **tag-only** (`v*`): the binary artifact,
  the `.flatpak` bundle and the GitHub release are only produced for
  releases. `release` `needs: [build, flatpak]`, so a red job anywhere
  blocks the release.
- Release body = the commit titles since the previous `v*` tag, so tag
  messages don't matter but **commit titles do**.
- FlatPark notify runs last with `continue-on-error`: harmless while the
  app isn't listed there, useful once it is.

## Verify (before every commit/push)

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
python3 build-aux/gen-cargo-sources.py    # only when deps changed
appstreamcli validate --no-net data/io.github.grigio.mirrorcam.metainfo.xml
desktop-file-validate data/io.github.grigio.mirrorcam.desktop
```

CI is not the safety net: `-D warnings` turns ordinary clippy lints into
failures and a red job blocks the release.

## Panics / RefCell

- Same trap as Carosello: `if let Some(x) = state.borrow_mut()…` keeps the
  `RefMut` alive for the whole `if`, so a second borrow in the body
  panics. Split into two statements.
- `gst::Device` is not `Send` across the app's structures in any useful
  way — camera handles stay on the main context; the GStreamer bus watch
  is `add_watch_local` (main loop), never a thread.

## GUI development (nested Wayland session — preferred)

Develop and test MirrorCam **inside a nested compositor** via the
`virtual-wayland` skill, never on the real desktop. It starts a second
`labwc` whose pointer/keyboard exist only inside it
(`zwlr_virtual_pointer_v1` / `zwp_virtual_keyboard_v1`), so screenshots
and input injection never touch the user's session: `/dev/uinput` is
never opened and `ydotoold` is never used.

```sh
VW=~/.pi/agent/skills/virtual-wayland/scripts/vw
pkill -x mirrorcam                 # host side; always -x, never -f (see below)
$VW start --headless --size 960x640   # or omit --headless to watch it
eval $($VW env)                    # export WAYLAND_DISPLAY=wayland-N

cargo build && $VW run ./target/debug/mirrorcam   # native binary
# or the Flatpak — it works here too (verified):
$VW run flatpak run io.github.grigio.mirrorcam

$VW input mousemove -a -x 120 -y 30   # hover the window -> reveals the header
$VW shot /tmp/a.png                   # screenshot of the NESTED output
$VW input click 0xC0                  # left click (0x40 down / 0x80 up)
$VW input key 109:1 109:0            # raw evdev keycode (End here)
$VW stop
```

Verified on this machine:

- **The Flatpak runs inside the nested session and still sees the
  camera.** `--device=all` grants `/dev/video*` independently of which
  compositor is underneath, so the sandboxed build enumerates the XiaoMi
  webcam exactly like the native one (`mirrorcam: found camera XiaoMi
  WebCam (V4L2) (/dev/video0)`). No real desktop session is needed for
  either build.
- **Screenshot pixels == `vw input -a` coordinates, 1:1 at any scale**
  (`vw shot` pins grim to the output scale and `-a` sends mode-size
  extents). Never divide by the scale. The default session is 960×640 @
  scale 0.5, so apps render on a 1920×1280 logical desktop.
- The window maps at its default `WIDTH`×`HEIGHT` (480×320) at 0,0; the
  header is hidden until the pointer enters the window (`mousemove -a`
  into it), and the live camera frames appear immediately — a **blank
  screenshot means the pipeline failed**, check the client log.
- App stdout/stderr is a normal file: `$VW logs` for the compositor,
  `/tmp/virtual-wayland/client-N.log` for the app. `pgrep -cx mirrorcam`
  works unchanged. Exit code 124 under `timeout` is normal, not a crash.

Use `--headless` for unattended runs (screenshots return instantly); the
windowed mode only renders while its window is visible, so an occluded
nested window makes `vw shot` time out.

## GUI automation on the real session (last resort)

- Only for things the nested session cannot reach — and for MirrorCam
  that is nothing: the Flatpak needs no host session (see above). When
  you must use the real session, keep it to a `timeout 8 flatpak run …`
  smoke test followed by `pkill -x mirrorcam`, and never leave a window
  or a camera stream running unattended.
- `pkill -f <path/pattern>` also matches your own shell's command line —
  use `pkill -x mirrorcam`.
- Never reach for `ydotool`/`xdotool`/`wtype` in a flow meant for the
  nested session: those move the user's real pointer and keys. `vw input`
  is the only injector that belongs here.
