//! MirrorCam — a minimal, borderless camera overlay built with GTK4 + libadwaita.
//!
//! * borderless window, always on top (layer shell, when the compositor supports it)
//! * auto-hiding panels: move the pointer to reveal them, they fade away when idle
//! * camera selector, shown only when the machine has more than one camera
//! * mirror toggle, `m` mirrors, `q` / `Esc` quits

mod camera;
#[cfg(has_layer_shell)]
mod layer_shell;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gstreamer as gst;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use libadwaita as adw;

const APP_ID: &str = "org.mirrorcam.Camera";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const WIDTH: i32 = 480;
const HEIGHT: i32 = 320;
/// Idle time after which the panels hide themselves.
const HIDE_DELAY: Duration = Duration::from_millis(1600);
/// How often the idle state is checked.
const TICK: Duration = Duration::from_millis(250);
/// Panel cross-fade, keep in sync with `transition-duration` in `CSS`.
const FADE_MS: u32 = 180;
/// Extra time after the fade before the panels are unmapped.
const FADE_SLACK_MS: u64 = 40;

const CSS: &str = r#"
.cam-root { background-color: #000000; }

.cam-panel {
    background-color: rgba(14, 15, 18, 0.60);
    border-radius: 14px;
    padding: 6px;
    box-shadow: 0 2px 14px rgba(0, 0, 0, 0.45);
}
/* the auto-hiding header bar is flush with the top edge of the window */
.cam-panel.cam-bar {
    border-radius: 0 0 14px 14px;
    padding: 6px 10px;
}
.cam-panel label        { color: #ffffff; }
.cam-panel label.title  { font-weight: 700; }
.cam-panel.status       { color: rgba(255, 255, 255, 0.70); }
.cam-panel separator    { background-color: rgba(255, 255, 255, 0.25); margin: 3px 4px; }

.cam-panel button,
.cam-panel dropdown button {
    color: #ffffff;
    background-color: rgba(255, 255, 255, 0.12);
    border: none;
    box-shadow: none;
    border-radius: 9px;
    min-width: 30px;
    min-height: 30px;
    padding: 0 8px;
}
.cam-panel button:hover { background-color: rgba(255, 255, 255, 0.24); }
.cam-panel button:checked { background-color: rgba(88, 156, 255, 0.75); }
.cam-panel dropdown { padding: 0; }

picture.mirrored { transform: scaleX(-1); }
"#;

type Shared = Rc<App>;

struct App {
    application: adw::Application,
    window: adw::ApplicationWindow,
    picture: gtk::Picture,
    top_revealer: gtk::Revealer,
    top_panel: gtk::Box,
    status: gtk::Label,
    title: gtk::Label,
    dropdown: gtk::DropDown,
    res_dropdown: gtk::DropDown,
    separator: gtk::Separator,
    mirror_btn: gtk::ToggleButton,
    #[cfg_attr(not(has_layer_shell), allow(dead_code))]
    pin_btn: gtk::ToggleButton,
    toast_overlay: adw::ToastOverlay,

    devices: RefCell<Vec<gst::Device>>,
    /// Resolutions offered by the selected camera.
    formats: RefCell<Vec<camera::Format>>,
    /// Currently chosen resolution, `(width, height)`.
    size: Cell<Option<(i32, i32)>>,
    /// Keeps watching for cameras plugged in while the app runs.
    watcher: RefCell<Option<camera::Devices>>,
    camera: RefCell<Option<camera::Camera>>,
    hide_src: RefCell<Option<glib::SourceId>>,
    unmap_src: RefCell<Option<glib::SourceId>>,
    last_activity: Cell<Instant>,
    /// Last pointer position reported by `EventControllerMotion`.
    last_pos: Cell<Option<(f64, f64)>>,
    /// Bumped on every camera switch, invalidates pending status updates.
    epoch: Cell<u32>,
    over_panel: Cell<bool>,
    panels_shown: Cell<bool>,
    /// Guards the dropdown against notifications we cause ourselves.
    updating: Cell<bool>,
    /// `true` when the window is a layer surface.
    #[cfg_attr(not(has_layer_shell), allow(dead_code))]
    layered: bool,
/// `true` while the overlay is being dragged.
    #[cfg_attr(not(has_layer_shell), allow(dead_code))]
    dragging: Cell<bool>,
    #[cfg_attr(not(has_layer_shell), allow(dead_code))]
    output: Cell<(i32, i32)>,
    #[cfg_attr(not(has_layer_shell), allow(dead_code))]
    centered: Cell<bool>,
}

fn main() {
    gst::init().expect("could not initialise GStreamer");

    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_activate(|application| {
        load_css();
        let app = build_ui(application);
        app.window.present();
        // hot-plug: rebuild the camera list when a device appears or disappears
        {
            let weak = Rc::downgrade(&app);
            *app.watcher.borrow_mut() = camera::Devices::watch(move || {
                if let Some(app) = weak.upgrade() {
                    refresh_cameras(&app);
                }
            });
        }
        refresh_cameras(&app);
        // reveal the controls once so they are discoverable, then let them hide
        touch(&app);
    });
    let _ = application.run();
}

// ---------------------------------------------------------------- UI --------

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build_ui(application: &adw::Application) -> Shared {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(WIDTH)
        .default_height(HEIGHT)
        .title("MirrorCam")
        .build();
    window.set_decorated(false);
    let layered = init_layer_shell(&window);

    // ---- video -------------------------------------------------------
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.add_css_class("cam-root");

    // ---- panels ------------------------------------------------------
    // Adwaita style: the title on the left, the controls on the right of an
    // auto-hiding header bar; "About" lives in the hamburger menu.
    let title = gtk::Label::new(Some("MirrorCam"));
    title.add_css_class("title");
    let status = gtk::Label::new(Some("starting…"));
    status.add_css_class("status");
    // the line carries name · size · fps, keep it inside the window
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_max_width_chars(50);
    // second row of the header: a floating pill right under the bar
    status.add_css_class("cam-panel");
    status.set_halign(gtk::Align::Start);
    status.set_margin_start(12);

    let dropdown = gtk::DropDown::from_strings(&[]);
    dropdown.set_tooltip_text(Some("Choose a camera"));
    // the button shows a shortened label so the header always fits the window
    dropdown.set_factory(Some(&button_factory(|text| text.to_string())));

    let res_dropdown = gtk::DropDown::from_strings(&[]);
    res_dropdown.set_tooltip_text(Some("Choose a resolution"));
    // `1920x1080 · 30 fps` → `1920x1080` (the fps stays in the list and in the status)
    res_dropdown.set_factory(Some(&button_factory(|text| {
        text.split(" · ").next().unwrap_or(text).to_string()
    })));

    let separator = gtk::Separator::new(gtk::Orientation::Vertical);

    let mirror_btn = gtk::ToggleButton::new();
    mirror_btn.set_child(Some(&gtk::Image::from_icon_name(
        "object-flip-horizontal-symbolic",
    )));
    mirror_btn.set_tooltip_text(Some("Mirror the image (m)"));

    let pin_btn = gtk::ToggleButton::new();
    pin_btn.set_child(Some(&gtk::Image::from_icon_name("view-pin-symbolic")));
    pin_btn.set_tooltip_text(Some("Always on top (p)"));
    pin_btn.set_visible(layered);
    pin_btn.set_active(true);

    // primary (hamburger) menu — Adwaita puts "About" there, not in the bar
    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Main Menu"));

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.append(&dropdown);
    controls.append(&res_dropdown);
    controls.append(&separator);
    controls.append(&mirror_btn);
    controls.append(&pin_btn);
    controls.append(&menu_btn);

    // header bar: app title on the left, controls on the right
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("cam-panel");
    bar.add_css_class("cam-bar");
    bar.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    bar.append(&controls);

    let top_panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    top_panel.append(&bar);
    top_panel.append(&status);

    // the whole header (bar + status) hides and shows as one unit
    let top_revealer = revealer(&top_panel, gtk::Align::Fill, gtk::Align::Start);

    // ---- window content ---------------------------------------------
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("cam-root");
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&top_revealer);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&overlay));
    window.set_content(Some(&toast_overlay));

    let app = Rc::new(App {
        application: application.clone(),
        window,
        picture,
        top_revealer,
        top_panel,
        status,
        title,
        dropdown,
        res_dropdown,
        separator,
        mirror_btn,
        pin_btn,
        toast_overlay,
        devices: RefCell::new(Vec::new()),
        formats: RefCell::new(Vec::new()),
        size: Cell::new(None),
        watcher: RefCell::new(None),
        camera: RefCell::new(None),
        hide_src: RefCell::new(None),
        unmap_src: RefCell::new(None),
        last_activity: Cell::new(Instant::now()),
        last_pos: Cell::new(None),
        epoch: Cell::new(0),
        over_panel: Cell::new(false),
        panels_shown: Cell::new(false),
        updating: Cell::new(false),
        layered,
        dragging: Cell::new(false),
        output: Cell::new((0, 0)),
        centered: Cell::new(false),
    });
    // panels start hidden, they only appear on pointer activity
    app.top_panel.set_visible(false);

    // ---- signals -----------------------------------------------------
    {
        let motion = gtk::EventControllerMotion::new();
        {
            let app = app.clone();
            motion.connect_motion(move |_, x, y| {
                // GTK re-delivers motion at unchanged coordinates whenever a
                // child (un)maps; such duplicates must not restart the idle
                // clock, or the panels would flicker forever
                if app.last_pos.get() != Some((x, y)) {
                    app.last_pos.set(Some((x, y)));
                    touch(&app);
                }
            });
        }
        // entering the surface delivers no motion event, reveal on enter too
        {
            let app = app.clone();
            motion.connect_enter(move |_, x, y| {
                app.last_pos.set(Some((x, y)));
                touch(&app);
            });
        }
        {
            let app = app.clone();
            motion.connect_leave(move |_| {
                app.last_pos.set(None);
                pointer_left(&app);
            });
        }
        app.window.add_controller(motion);

        let key = gtk::EventControllerKey::new();
        {
            let app = app.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                match keyval.to_unicode() {
                    Some('q' | 'Q' | '\u{1b}') => {
                        app.application.quit();
                    }
                    Some('m' | 'M') => {
                        app.mirror_btn.set_active(!app.mirror_btn.is_active());
                        touch(&app);
                    }
                    Some('p' | 'P') => {
                        app.pin_btn.set_active(!app.pin_btn.is_active());
                        touch(&app);
                    }
                    _ => {}
                }
                glib::Propagation::Proceed
            });
        }
        app.window.add_controller(key);

        let click = gtk::GestureClick::new();
        {
            let app = app.clone();
            click.connect_pressed(move |_, _, _, _| touch(&app));
        }
        app.window.add_controller(click);
    }

    watch_panel(&app, &app.top_panel);

    {
        let dropdown = app.dropdown.clone();
        let app = app.clone();
        dropdown.connect_selected_notify(move |dropdown| {
            if app.updating.get() {
                return;
            }
            let position = dropdown.selected();
            if position != gtk::INVALID_LIST_POSITION {
                // the new camera may offer other resolutions
                refresh_formats(&app);
                open_camera(&app, position);
            }
        });
    }
    {
        let res_dropdown = app.res_dropdown.clone();
        let app = app.clone();
        res_dropdown.connect_selected_notify(move |dropdown| {
            if app.updating.get() {
                return;
            }
            let position = dropdown.selected();
            if position == gtk::INVALID_LIST_POSITION {
                return;
            }
            app.size.set(
                app.formats
                    .borrow()
                    .get(position as usize)
                    .map(|format| (format.width, format.height)),
            );
            open_camera(&app, camera_index(&app));
        });
    }
    {
        let mirror = app.mirror_btn.clone();
        let app = app.clone();
        mirror.connect_toggled(move |button| {
            if button.is_active() {
                app.picture.add_css_class("mirrored");
            } else {
                app.picture.remove_css_class("mirrored");
            }
        });
    }
    {
        // hamburger menu: About MirrorCam + Quit, as actions on the application
        let menu = gtk::gio::Menu::new();
        menu.append(Some("About MirrorCam"), Some("app.about"));
        menu.append(Some("Quit"), Some("app.quit"));
        menu_btn.set_menu_model(Some(&menu));

        let about = gtk::gio::SimpleAction::new("about", None);
        let about_app = app.clone();
        about.connect_activate(move |_, _| show_about(&about_app));
        application.add_action(&about);

        let quit = gtk::gio::SimpleAction::new("quit", None);
        let quit_app = app.clone();
        quit.connect_activate(move |_, _| quit_app.application.quit());
        application.add_action(&quit);
        application.set_accels_for_action("app.quit", &["<primary>q"]);
    }

    #[cfg(has_layer_shell)]
    if app.layered {
        let map_app = app.clone();
        app.window.connect_map(move |_| center_on_output(&map_app));
        let pin_app = app.clone();
        app.pin_btn.connect_toggled(move |button| {
            let layer = if button.is_active() {
                layer_shell::Layer::Overlay
            } else {
                layer_shell::Layer::Top
            };
            layer_shell::set_layer(&pin_app.window, layer);
        });
        setup_drag(&app);
    }

    app
}

/// Builds the widget a `GtkDropDown` uses for its button: a label capped at
/// 12 characters, filled by `shorten`. The popover keeps the full strings (it
/// uses the default `list_factory`), so the header bar never grows wider than
/// the window while the menus stay readable.
fn button_factory(shorten: impl Fn(&str) -> String + 'static) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("GtkListItem");
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(12);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(text) = item.item().and_then(|object| object.downcast::<gtk::StringObject>().ok())
        else {
            return;
        };
        let Some(label) = item.child().and_then(|child| child.downcast::<gtk::Label>().ok()) else {
            return;
        };
        label.set_text(&shorten(&text.string()));
    });
    factory
}

fn revealer(child: &impl IsA<gtk::Widget>, halign: gtk::Align, valign: gtk::Align) -> gtk::Revealer {
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(FADE_MS);
    revealer.set_child(Some(child));
    revealer.set_reveal_child(false);
    revealer.set_halign(halign);
    revealer.set_valign(valign);
    revealer
}

// ---------------------------------------------------------- autohide --------

/// Any pointer/key activity: show the panels and restart the idle clock.
fn touch(app: &Shared) {
    app.last_activity.set(Instant::now());
    show_panels(app, true);
    ensure_ticker(app);
}

fn pointer_left(app: &Shared) {
    if can_hide(app) {
        show_panels(app, false);
        stop_ticker(app);
    } else {
        // a menu is open: it lives on its own surface, keep everything up
        touch(app);
    }
}

/// Keeps the panels visible while the pointer rests on one of them.
fn watch_panel(app: &Shared, panel: &gtk::Box) {
    let motion = gtk::EventControllerMotion::new();
    {
        let app = app.clone();
        motion.connect_enter(move |_, _, _| {
            app.over_panel.set(true);
            touch(&app);
        });
    }
    {
        let app = app.clone();
        motion.connect_leave(move |_| {
            app.over_panel.set(false);
            touch(&app);
        });
    }
    panel.add_controller(motion);
}

fn ensure_ticker(app: &Shared) {
    if app.hide_src.borrow().is_some() {
        return;
    }
    let app2 = app.clone();
    let source = glib::timeout_add_local(TICK, move || {
        if !can_hide(&app2) {
            return glib::ControlFlow::Continue; // pointer on a panel / menu open
        }
        if !app2.panels_shown.get() {
            *app2.hide_src.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        if app2.last_activity.get().elapsed() >= HIDE_DELAY {
            show_panels(&app2, false);
            *app2.hide_src.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    *app.hide_src.borrow_mut() = Some(source);
}

fn stop_ticker(app: &Shared) {
    if let Some(source) = app.hide_src.borrow_mut().take() {
        source.remove();
    }
}

fn can_hide(app: &Shared) -> bool {
    // any open menu — camera, resolution or the hamburger — lives on its own
    // surface; the header must stay up until it is closed
    !app.over_panel.get() && !popover_open(&app.window)
}

/// `true` while a `GtkDropDown` menu is open (the menu is a separate surface,
/// so the panels must not hide underneath it).
fn popover_open(widget: &impl IsA<gtk::Widget>) -> bool {
    let children = widget.observe_children();
    for index in 0..children.n_items() {
        let Some(child) = children
            .item(index)
            .and_then(|object| object.downcast::<gtk::Widget>().ok())
        else {
            continue;
        };
        if child.type_().is_a(gtk::Popover::static_type()) && child.is_mapped() {
            return true;
        }
        if popover_open(&child) {
            return true;
        }
    }
    false
}

/// Shows or hides both panels: cross-fade, then unmap so that a hidden panel
/// can neither be seen nor clicked.
fn show_panels(app: &Shared, visible: bool) {
    if app.panels_shown.get() == visible {
        return;
    }
    if visible {
        if let Some(source) = app.unmap_src.borrow_mut().take() {
            source.remove();
        }
        app.panels_shown.set(true);
        app.top_panel.set_visible(true);
        app.top_revealer.set_reveal_child(true);
    } else {
        app.panels_shown.set(false);
        app.top_revealer.set_reveal_child(false);
        app.over_panel.set(false);
        let app2 = app.clone();
        let source = glib::timeout_add_local(
            Duration::from_millis(u64::from(FADE_MS) + FADE_SLACK_MS),
            move || {
                // forget the id before the source destroys itself, otherwise a
                // later `SourceId::remove()` would panic on a dead source
                *app2.unmap_src.borrow_mut() = None;
                if !app2.panels_shown.get() {
                    app2.top_panel.set_visible(false);
                }
                glib::ControlFlow::Break
            },
        );
        *app.unmap_src.borrow_mut() = Some(source);
    }
}

// ----------------------------------------------------------- cameras --------

fn refresh_cameras(app: &Shared) {
    let devices = match app.watcher.borrow().as_ref() {
        Some(watcher) => watcher.devices(),
        None => camera::list_devices(),
    };
    let new_keys: Vec<String> = devices.iter().map(camera::key).collect();

    let previous_key = app
        .devices
        .borrow()
        .get(camera_index(app) as usize)
        .map(camera::key);
    let selected = previous_key
        .as_ref()
        .and_then(|key| new_keys.iter().position(|other| other == key))
        .unwrap_or(0) as u32;

    let labels = camera::labels(&devices);
    let strings: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&strings);

    app.updating.set(true);
    app.dropdown.set_model(Some(&model));
    if !strings.is_empty() {
        app.dropdown.set_selected(selected);
    }
    app.updating.set(false);
    // a selector only makes sense with something to select
    app.dropdown.set_visible(strings.len() > 1);
    // give the header room for the selector when there is one
    app.title.set_visible(strings.len() < 2);
    update_separator(app);
    println!("mirrorcam: {} camera(s)", strings.len());
    for device in &devices {
        println!("mirrorcam: found camera {} ({})", camera::label(device), camera::key(device));
    }

    *app.devices.borrow_mut() = devices;

    if app.devices.borrow().is_empty() {
        app.show_error("No camera found");
        return;
    }
    let formats_changed = refresh_formats(app);
    let camera_changed =
        previous_key != app.devices.borrow().get(selected as usize).map(camera::key);
    // only restart the pipeline when the user actually ended up on another
    // camera or resolution, a hot-plug event must not interrupt the preview
    if camera_changed || formats_changed || app.camera.borrow().is_none() {
        open_camera(app, selected);
    }
}

/// Rebuilds the resolution list for the selected camera, keeping the chosen
/// size when the new camera offers it. `true` when the size actually changed.
fn refresh_formats(app: &Shared) -> bool {
    let device = app
        .devices
        .borrow()
        .get(camera_index(app) as usize)
        .cloned();
    let formats = device.map(|device| camera::formats(&device)).unwrap_or_default();
    let labels: Vec<String> = formats.iter().map(camera::Format::label).collect();
    let strings: Vec<&str> = labels.iter().map(String::as_str).collect();

    let wanted = app.size.get();
    let selected = wanted
        .and_then(|size| {
            formats
                .iter()
                .position(|format| (format.width, format.height) == size)
        })
        .unwrap_or(0) as u32;

    app.updating.set(true);
    app.res_dropdown
        .set_model(Some(&gtk::StringList::new(&strings)));
    if !strings.is_empty() {
        app.res_dropdown.set_selected(selected);
    }
    app.updating.set(false);
    app.res_dropdown.set_visible(strings.len() > 1);
    update_separator(app);

    let chosen = formats
        .get(selected as usize)
        .map(|format| (format.width, format.height));
    let changed = chosen != wanted && (chosen.is_some() || wanted.is_some());
    app.size.set(chosen);
    *app.formats.borrow_mut() = formats;
    changed
}

/// The separator divides the selectors from the buttons; hide it when no
/// selector is visible.
fn update_separator(app: &Shared) {
    app.separator
        .set_visible(app.dropdown.is_visible() || app.res_dropdown.is_visible());
}

/// Index of the selected camera, `0` when there is nothing to select.
fn camera_index(app: &Shared) -> u32 {
    let selected = app.dropdown.selected();
    if selected == gtk::INVALID_LIST_POSITION {
        0
    } else {
        selected
    }
}

fn open_camera(app: &Shared, position: u32) {
    let device = app
        .devices
        .borrow()
        .get(position as usize)
        .cloned();
    let Some(device) = device else {
        return;
    };
    let name = camera::label(&device);
    let format = app
        .formats
        .borrow()
        .get(app.res_dropdown.selected() as usize)
        .copied();

    let epoch = app.epoch.get() + 1;
    app.epoch.set(epoch);
    app.camera.replace(None); // stops the previous pipeline
    app.picture.set_paintable(None::<&gdk::Paintable>);
    app.status.set_text(&name);

    let error_app = app.clone();
    let camera = match camera::Camera::open(
        &device,
        format,
        move |message| error_app.show_error(&message),
    ) {
        Ok(camera) => camera,
        Err(error) => {
            app.show_error(&format!("{name}: {error}"));
            return;
        }
    };
    *app.camera.borrow_mut() = Some(camera);
    attach_paintable(app, epoch);
    update_status(app, &name);

    // negotiated size and measured frame rate, refreshed while this camera runs
    let app2 = app.clone();
    glib::timeout_add_local(Duration::from_millis(1000), move || {
        if app2.epoch.get() != epoch {
            return glib::ControlFlow::Break; // another camera took over
        }
        update_status(&app2, &name);
        glib::ControlFlow::Continue
    });
}

/// `XiaoMi WebCam · 1920x1080 · 30 fps` (shorter while the pipeline starts).
fn update_status(app: &Shared, name: &str) {
    let camera = app.camera.borrow();
    let text = match camera.as_ref().and_then(camera::Camera::resolution) {
        Some(resolution) => {
            let fps = camera.as_ref().map(camera::Camera::fps).unwrap_or(0);
            if fps > 0 {
                format!("{name} · {resolution} · {fps} fps")
            } else {
                format!("{name} · {resolution}")
            }
        }
        None => name.to_string(),
    };
    drop(camera);
    app.status.set_text(&text);
}

/// The sink hands out its `GstPaintable` as soon as it has started; poll for it
/// (the property is a plain GObject property, so no signal can be used here).
fn attach_paintable(app: &Shared, epoch: u32) {
    let app2 = app.clone();
    let mut attempts = 0;
    glib::timeout_add_local(Duration::from_millis(60), move || {
        attempts += 1;
        if app2.epoch.get() != epoch {
            return glib::ControlFlow::Break; // another camera took over
        }
        let paintable = app2
            .camera
            .borrow()
            .as_ref()
            .and_then(camera::Camera::paintable);
        if let Some(paintable) = paintable {
            app2.picture.set_paintable(Some(&paintable));
            return glib::ControlFlow::Break;
        }
        if attempts >= 25 {
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

// ------------------------------------------------------------- misc ---------

impl App {
    fn show_error(&self, message: &str) {
        println!("mirrorcam: {message}");
        // toasts are the notification channel and live at the bottom of the
        // window (Adwaita); the status line only repeats it while no camera
        // is running, so the header never turns into an error banner
        if self.camera.borrow().is_none() {
            self.status.set_text(message);
        }
        let toast = adw::Toast::new(message);
        toast.set_timeout(4);
        self.toast_overlay.add_toast(toast);
    }
}

fn show_about(app: &Shared) {
    let developers: &[&str] = &["MirrorCam contributors"];
    adw::AboutWindow::builder()
        .transient_for(&app.window)
        .application_name("MirrorCam")
        .version(VERSION)
        .developers(developers)
        .comments("A minimal, borderless camera overlay with auto-hiding panels.")
        .website("https://github.com/grigio/borderless-camera")
        .license_type(gtk::License::MitX11)
        .build()
        .present();
}

// ------------------------------------------------------ layer-shell bits -----

/// Initializes the window as a layer surface, `false` when unsupported.
#[cfg(has_layer_shell)]
fn init_layer_shell(window: &adw::ApplicationWindow) -> bool {
    let supported = layer_shell::is_supported();
    println!(
        "mirrorcam: layer-shell {}",
        if supported { "available" } else { "unsupported" }
    );
    if supported {
        layer_shell::init(window);
    }
    supported
}

#[cfg(not(has_layer_shell))]
fn init_layer_shell(_window: &adw::ApplicationWindow) -> bool {
    println!("mirrorcam: built without gtk4-layer-shell");
    false
}

/// Places the window in the middle of the output it landed on.
#[cfg(has_layer_shell)]
fn center_on_output(app: &Shared) {
    if app.centered.get() {
        return;
    }
    let Some(surface) = app.window.surface() else {
        return;
    };
    let Some(monitor) = surface.display().monitor_at_surface(&surface) else {
        return;
    };
    let (output_w, output_h) = monitor_size(&monitor);
    app.output.set((output_w, output_h));
    layer_shell::set_margin(
        &app.window,
        layer_shell::Edge::Left,
        ((output_w - WIDTH) / 2).max(0),
    );
    layer_shell::set_margin(
        &app.window,
        layer_shell::Edge::Top,
        ((output_h - HEIGHT) / 2).max(0),
    );
    app.centered.set(true);
}

/// `gdk::Rectangle` exposes no accessors, read the fields through the C layout.
#[cfg(has_layer_shell)]
fn monitor_size(monitor: &gdk::Monitor) -> (i32, i32) {
    use gtk4::glib::translate::*;
    let geometry = monitor.geometry();
    let stash = geometry.to_glib_none();
    unsafe { ((*stash.0).width, (*stash.0).height) }
}

/// Dragging a layer surface is done by moving its margins.
#[cfg(has_layer_shell)]
fn setup_drag(app: &Shared) {
    let gesture = gtk::GestureDrag::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    {
        let app = app.clone();
        gesture.connect_drag_begin(move |_, _, _| {
            app.dragging.set(true);
            touch(&app);
        });
    }
    {
        let app = app.clone();
        gesture.connect_drag_update(move |_, dx, dy| {
            if !app.dragging.get() {
                return;
            }
            // `dx`/`dy` are surface-local: they are measured against the
            // position the surface had when the press happened, so the window
            // must be offset from where it is *now*, otherwise it trails the
            // pointer at half speed
            let (output_w, output_h) = app.output.get();
            let cur_left = layer_shell::margin(&app.window, layer_shell::Edge::Left);
            let cur_top = layer_shell::margin(&app.window, layer_shell::Edge::Top);
            let new_left = (cur_left + dx as i32).clamp(0, (output_w - WIDTH).max(0));
            let new_top = (cur_top + dy as i32).clamp(0, (output_h - HEIGHT).max(0));
            layer_shell::set_margin(&app.window, layer_shell::Edge::Left, new_left);
            layer_shell::set_margin(&app.window, layer_shell::Edge::Top, new_top);
        });
    }
    {
        let app = app.clone();
        gesture.connect_drag_end(move |_, _, _| {
            app.dragging.set(false);
            touch(&app);
        });
    }
    app.picture.add_controller(gesture);
}
