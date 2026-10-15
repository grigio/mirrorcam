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
  then, in a Wayland session,
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

## GUI automation (this machine)

- The nested-session flow in the README (`virtual-wayland` skill) works
  for the native binary. The Flatpak cannot run inside it (different
  sandbox), test that one on the real session with a `timeout` and a
  `pkill -x mirrorcam` afterwards.
- `pkill -f <path/pattern>` also matches your own shell's command line —
  use `pkill -x mirrorcam`.
