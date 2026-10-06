//! Detects `gtk4-layer-shell` at build time.
//!
//! When the library is present the crate is compiled with `--cfg has_layer_shell`,
//! which turns the window into an overlay layer surface (always on top) and makes
//! it draggable through layer-shell margins. Without the library the app still
//! builds and runs as a plain undecorated window.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(has_layer_shell)");

    if pkg_config(&["--exists", "gtk4-layer-shell-0"]).is_none() {
        println!("cargo:warning=gtk4-layer-shell not found: no always-on-top/drag support");
        return;
    }
    println!("cargo:rustc-cfg=has_layer_shell");

    if let Some(flags) = pkg_config(&["--libs", "gtk4-layer-shell-0"]) {
        for flag in flags.split_whitespace() {
            if let Some(path) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={path}");
            } else if let Some(name) = flag.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={name}");
            }
        }
    }
}

/// Runs `pkg-config <args>` and returns its trimmed stdout, or `None` on failure.
fn pkg_config(args: &[&str]) -> Option<String> {
    let output = Command::new("pkg-config").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
