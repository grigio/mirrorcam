//! Hand-rolled FFI to `gtk4-layer-shell`.
//!
//! The bindings are written by hand (instead of pulling in the `gtk4-layer-shell`
//! crate) so the whole app keeps a single gtk4-rs version. The module is only
//! compiled when `build.rs` finds the library (`--cfg has_layer_shell`).

use gtk4::glib::translate::*;
use gtk4::prelude::*;
use gtk4::Window;

/// Layer-shell layer (see `GtkLayerShellLayer`).
#[derive(Clone, Copy)]
#[repr(i32)]
pub enum Layer {
    Top = 2,
    Overlay = 3,
}

/// Layer-shell edge (see `GtkLayerShellEdge`).
#[derive(Clone, Copy)]
#[repr(i32)]
pub enum Edge {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

mod ffi {
    use gtk4::ffi::GtkWindow;
    use std::os::raw::{c_char, c_int};

    /// `GTK_LAYER_SHELL_KEYBOARD_MODE_ON_DEMAND`
    pub const KEYBOARD_ON_DEMAND: c_int = 2;

    extern "C" {
        pub fn gtk_layer_is_supported() -> c_int;
        pub fn gtk_layer_init_for_window(window: *mut GtkWindow);
        pub fn gtk_layer_set_layer(window: *mut GtkWindow, layer: c_int);
        pub fn gtk_layer_set_anchor(window: *mut GtkWindow, edge: c_int, anchor: c_int);
        pub fn gtk_layer_set_margin(window: *mut GtkWindow, edge: c_int, margin: c_int);
        pub fn gtk_layer_get_margin(window: *mut GtkWindow, edge: c_int) -> c_int;
        pub fn gtk_layer_set_exclusive_zone(window: *mut GtkWindow, zone: c_int);
        pub fn gtk_layer_set_keyboard_mode(window: *mut GtkWindow, mode: c_int);
        pub fn gtk_layer_set_namespace(window: *mut GtkWindow, name: *const c_char);
    }
}

fn ptr(window: &impl IsA<Window>) -> *mut gtk4::ffi::GtkWindow {
    window.upcast_ref::<Window>().to_glib_none().0
}

/// `TRUE` when the compositor speaks `zwlr_layer_shell_v1` (blocks on a roundtrip
/// the first time it is called).
pub fn is_supported() -> bool {
    unsafe { ffi::gtk_layer_is_supported() != 0 }
}

/// Turns `window` into a floating overlay: always on top, no exclusive zone,
/// keyboard input on demand. Must be called before the window is realized.
pub fn init(window: &impl IsA<Window>) {
    unsafe {
        ffi::gtk_layer_init_for_window(ptr(window));
        ffi::gtk_layer_set_layer(ptr(window), Layer::Overlay as i32);
        // Anchor only the top-left corner: the client owns the size and the
        // margins position the surface, which is what makes dragging possible.
        ffi::gtk_layer_set_anchor(ptr(window), Edge::Left as i32, 1);
        ffi::gtk_layer_set_anchor(ptr(window), Edge::Top as i32, 1);
        ffi::gtk_layer_set_anchor(ptr(window), Edge::Right as i32, 0);
        ffi::gtk_layer_set_anchor(ptr(window), Edge::Bottom as i32, 0);
        ffi::gtk_layer_set_exclusive_zone(ptr(window), -1);
        ffi::gtk_layer_set_keyboard_mode(ptr(window), ffi::KEYBOARD_ON_DEMAND);
        ffi::gtk_layer_set_namespace(ptr(window), c"mirrorcam".as_ptr());
    }
}

pub fn set_layer(window: &impl IsA<Window>, layer: Layer) {
    unsafe { ffi::gtk_layer_set_layer(ptr(window), layer as i32) }
}

pub fn set_margin(window: &impl IsA<Window>, edge: Edge, margin: i32) {
    unsafe { ffi::gtk_layer_set_margin(ptr(window), edge as i32, margin.max(0)) }
}

pub fn margin(window: &impl IsA<Window>, edge: Edge) -> i32 {
    unsafe { ffi::gtk_layer_get_margin(ptr(window), edge as i32) }
}
