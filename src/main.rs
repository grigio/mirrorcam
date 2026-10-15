//! MirrorCam — a minimal, borderless camera overlay built with GTK4 + libadwaita.
//!
//! * a normal, borderless window: the compositor raises it on click, it shows
//!   up in the window switcher (alt+tab) and takes keyboard focus
//! * auto-hiding panels: move the pointer to reveal them, they fade away when idle
//! * camera selector, shown only when the machine has more than one camera
//! * mirror toggle, `m` mirrors, `q` / `Esc` quits

mod camera;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gstreamer as gst;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use libadwaita as adw;

const APP_ID: &str = "io.github.grigio.mirrorcam";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const WIDTH: i32 = 480;
const HEIGHT: i32 = 320;
/// Smallest size the window can be dragged down to.
const MIN_WIDTH: i32 = 240;
const MIN_HEIGHT: i32 = 160;
/// Thickness of the invisible resize strips along the window edges.
const GRIP: i32 = 14;
/// Distance a pointer drag must travel before it counts as a drag (px):
/// below it a press is just a click — focus the window, open a menu — and
/// no `begin_move` / `begin_resize` request is sent to the compositor.
const DRAG_THRESHOLD: f64 = 4.0;
/// Size asked for the About dialog before it is placed over the overlay.
const ABOUT_WIDTH: i32 = 620;
const ABOUT_HEIGHT: i32 = 480;
/// Idle time after which the panels hide themselves.
const HIDE_DELAY: Duration = Duration::from_millis(1600);
/// How often the idle state is checked.
const TICK: Duration = Duration::from_millis(250);
/// Panel cross-fade, keep in sync with `transition-duration` in `CSS`.
const FADE_MS: u32 = 180;
/// Extra time after the fade before the panels are unmapped.
const FADE_SLACK_MS: u64 = 40;

const CSS: &str = r#"
/* the toplevel itself is transparent: only the canvas paints, so the corners
   of the window can be rounded. Scoped to *our* window — the About dialog
   must keep the Adwaita background. */
window.cam-window { background-color: transparent; }

.cam-root {
    background-color: #000000;
    /* GNOME/Adwaita window radius */
    border-radius: 12px;
}

/* The auto-hiding header is a real AdwHeaderBar: the "flat" class drops the
   theme background and this strip draws the same translucent band + hairline
   edge GNOME media apps use. The buttons inside are left to Adwaita's `.osd`
   rules, no custom button CSS. */
.controls-bg-top {
    background-color: alpha(rgba(0, 0, 0, 0.65), 0.9);
    padding: 0;
    border-bottom: 1px solid #4d4d4d;
}

picture.mirrored { transform: scaleX(-1); }
"#;

type Shared = Rc<App>;

struct App {
    application: adw::Application,
    window: adw::ApplicationWindow,
    picture: gtk::Picture,
    top_revealer: gtk::Revealer,
    top_panel: adw::HeaderBar,
    dropdown: gtk::DropDown,
    res_dropdown: gtk::DropDown,
    mirror_btn: gtk::ToggleButton,
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
    /// Bumped on every camera switch, invalidates pending announcements.
    epoch: Cell<u32>,
    over_panel: Cell<bool>,
    panels_shown: Cell<bool>,
    /// Guards the dropdown against notifications we cause ourselves.
    updating: Cell<bool>,
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
    // scopes the transparent-background CSS rule above to this window only
    window.add_css_class("cam-window");
    // icon shown by the compositor in the window switcher / taskbar
    window.set_icon_name(Some("camera-video-symbolic"));
    // the compositor drives the size from now on (xdg configure events),
    // this is the smallest window the resize grips may shrink it to
    window.set_size_request(MIN_WIDTH, MIN_HEIGHT);

    // ---- video -------------------------------------------------------
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.add_css_class("cam-root");

    // ---- header ------------------------------------------------------
    // GNOME HIG: a real AdwHeaderBar with an AdwWindowTitle — selectors on
    // the start side, actions on the end side, libadwaita's window controls
    // (the ✕, right of the hamburger) after them; "About" lives in the
    // hamburger menu.
    let window_title = adw::WindowTitle::builder().title("MirrorCam").build();

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

    let mirror_btn = gtk::ToggleButton::new();
    mirror_btn.set_child(Some(&gtk::Image::from_icon_name(
        "object-flip-horizontal-symbolic",
    )));
    mirror_btn.set_tooltip_text(Some("Mirror the image (m)"));

    // primary (hamburger) menu — Adwaita puts "About" there, not in the bar
    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Main Menu"));

    let header = adw::HeaderBar::builder()
        .title_widget(&window_title)
        .css_classes(["flat", "titlebar", "controls-bg-top", "osd"])
        .build();
    // window controls: only the ✕, on the right of the panel, after the
    // app's own buttons (the start side stays empty — no ✕ on the left).
    // The layout comes from `org.gnome.desktop.wm.preferences button-layout`
    // (`appmenu:close`), so libadwaita shows just the close button; minimising
    // / maximising make no sense for this overlay and are not offered.
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);

    header.pack_start(&dropdown);
    header.pack_start(&res_dropdown);
    // `AdwHeaderBar::pack_end` prepends, so the calls run in reverse visual
    // order; libadwaita appends the window controls (the ✕) after everything,
    // i.e. hard right, at the very end of the bar.
    header.pack_end(&menu_btn);
    header.pack_end(&mirror_btn);

    // the header hides and shows as one unit
    let top_panel = header;
    let top_revealer = revealer(&top_panel, gtk::Align::Fill, gtk::Align::Start);

    // ---- window content ---------------------------------------------
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("cam-root");
    overlay.set_child(Some(&picture));
    // resize grips are added first: the header bar stays above them, so the
    // buttons at the right edge keep working
    let right_grip = resize_grip(
        &overlay,
        GRIP,
        -1,
        gtk::Align::End,
        gtk::Align::Fill,
        "e-resize",
    );
    let bottom_grip = resize_grip(
        &overlay,
        -1,
        GRIP,
        gtk::Align::Fill,
        gtk::Align::End,
        "s-resize",
    );
    let corner_grip = resize_grip(
        &overlay,
        GRIP + 12,
        GRIP + 12,
        gtk::Align::End,
        gtk::Align::End,
        "se-resize",
    );
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
        dropdown,
        res_dropdown,
        mirror_btn,
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
    });
    // panels start hidden, they only appear on pointer activity. The
    // revealer keeps its strip while the bar is hidden, so it must stop
    // taking the pointer: a drag in that strip belongs to the video.
    app.top_panel.set_visible(false);
    app.top_revealer.set_can_target(false);

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
                open_camera(&app, position, true);
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
            open_camera(&app, camera_index(&app), true);
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
    setup_resize(&app, &right_grip, gdk::SurfaceEdge::East);
    setup_resize(&app, &bottom_grip, gdk::SurfaceEdge::South);
    setup_resize(&app, &corner_grip, gdk::SurfaceEdge::SouthEast);
    // dragging the video moves the window. The header needs no gesture of
    // its own: `AdwHeaderBar` wraps its content in a `GtkWindowHandle`,
    // which issues the very same `xdg_toplevel.move` when the bar is
    // dragged (and nothing when a button inside it is pressed)
    setup_drag(&app, &app.picture);

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
        let Some(text) = item
            .item()
            .and_then(|object| object.downcast::<gtk::StringObject>().ok())
        else {
            return;
        };
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        label.set_text(&shorten(&text.string()));
    });
    factory
}

fn revealer(
    child: &impl IsA<gtk::Widget>,
    halign: gtk::Align,
    valign: gtk::Align,
) -> gtk::Revealer {
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
fn watch_panel(app: &Shared, panel: &impl IsA<gtk::Widget>) {
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
        app.top_revealer.set_can_target(true);
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
                    // the empty strip stops intercepting the pointer, so a
                    // drag over it reaches the video and moves the window
                    app2.top_revealer.set_can_target(false);
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
    app.top_panel.set_show_title(strings.len() < 2);
    println!("mirrorcam: {} camera(s)", strings.len());
    for device in &devices {
        println!(
            "mirrorcam: found camera {} ({})",
            camera::label(device),
            camera::key(device)
        );
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
        open_camera(app, selected, false);
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
    let formats = device
        .map(|device| camera::formats(&device))
        .unwrap_or_default();
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

    let chosen = formats
        .get(selected as usize)
        .map(|format| (format.width, format.height));
    let changed = chosen != wanted && (chosen.is_some() || wanted.is_some());
    app.size.set(chosen);
    *app.formats.borrow_mut() = formats;
    changed
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

fn open_camera(app: &Shared, position: u32, notify: bool) {
    let device = app.devices.borrow().get(position as usize).cloned();
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

    let error_app = app.clone();
    let camera = match camera::Camera::open(&device, format, move |message| {
        error_app.show_error(&message)
    }) {
        Ok(camera) => camera,
        Err(error) => {
            app.show_error(&format!("{name}: {error}"));
            return;
        }
    };
    *app.camera.borrow_mut() = Some(camera);
    attach_paintable(app, epoch);
    if notify {
        announce(app, epoch, &name);
    }
}

/// `XiaoMi WebCam (V4L2) · 1920x1080 · 30 fps` as a toast.
///
/// The header carries no permanent status line: the camera info shows up only
/// when the settings change and disappears with the toast. One shot per
/// camera change, fired once the pipeline settled and the rate is measurable.
fn announce(app: &Shared, epoch: u32, name: &str) {
    let name = name.to_string();
    let app2 = app.clone();
    glib::timeout_add_local(Duration::from_millis(1400), move || {
        if app2.epoch.get() != epoch {
            return glib::ControlFlow::Break; // a newer change took over
        }
        let text = {
            let camera = app2.camera.borrow();
            let Some(camera) = camera.as_ref() else {
                return glib::ControlFlow::Break;
            };
            let mut text = name.clone();
            if let Some(resolution) = camera.resolution() {
                text.push_str(&format!(" · {resolution}"));
            }
            let fps = camera.fps();
            if fps > 0 {
                text.push_str(&format!(" · {fps} fps"));
            }
            text
        };
        app2.toast(&text);
        glib::ControlFlow::Break
    });
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
    /// A toast at the bottom of the window — the only notification channel
    /// (Adwaita): it never blocks the header and goes away by itself.
    fn toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(4);
        self.toast_overlay.add_toast(toast);
    }

    fn show_error(&self, message: &str) {
        println!("mirrorcam: {message}");
        self.toast(message);
    }
}

fn show_about(app: &Shared) {
    let developers: &[&str] = &["MirrorCam contributors"];
    let about = adw::AboutWindow::builder()
        .application_name("MirrorCam")
        .version(VERSION)
        .icon_name(APP_ID)
        .developers(developers)
        .comments("A minimal, borderless camera overlay with auto-hiding panels.")
        .website("https://github.com/grigio/mirrorcam")
        .license_type(gtk::License::MitX11)
        .build();
    // a normal transient window: the compositor stacks it over MirrorCam
    // and places it like the dialog of any other application
    about.set_transient_for(Some(&app.window));
    about.set_default_size(ABOUT_WIDTH, ABOUT_HEIGHT);
    about.set_modal(true);
    about.present();
}

// ---------------------------------------------------------------- resize ---

/// Adds an invisible widget along an edge (or corner) of the canvas: the
/// cursor hints at the direction, dragging it starts the compositor's own
/// resize for the matching `gdk::SurfaceEdge`.
fn resize_grip(
    overlay: &gtk::Overlay,
    width: i32,
    height: i32,
    halign: gtk::Align,
    valign: gtk::Align,
    cursor: &str,
) -> gtk::Box {
    let grip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grip.set_size_request(width, height);
    grip.set_halign(halign);
    grip.set_valign(valign);
    grip.set_cursor_from_name(Some(cursor));
    overlay.add_overlay(&grip);
    grip
}

/// Pressing a grip and dragging asks the compositor to resize the window
/// from that edge (`xdg_toplevel.resize`): labwc does the work, draws its
/// own resize indicator and enforces the window's minimum size. The grips
/// and the video live on different widgets, so their gestures never race.
fn setup_resize(app: &Shared, widget: &impl IsA<gtk::Widget>, edge: gdk::SurfaceEdge) {
    let gesture = gtk::GestureDrag::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let started = Rc::new(Cell::new(false));
    {
        let started = started.clone();
        let app = app.clone();
        gesture.connect_drag_begin(move |_, _, _| {
            started.set(false);
            touch(&app);
        });
    }
    {
        let started = started.clone();
        let app = app.clone();
        gesture.connect_drag_update(move |gesture, dx, dy| {
            // a few pixels first: a plain click on the edge does nothing,
            // only a real drag hands the grab over to the compositor
            if started.get() || !over_threshold(dx, dy) {
                return;
            }
            started.set(true);
            begin_resize(&app, edge, gesture);
        });
    }
    {
        let app = app.clone();
        gesture.connect_drag_end(move |_, _, _| touch(&app));
    }
    widget.add_controller(gesture);
}

/// Dragging the video moves the window: one `xdg_toplevel.move` and the
/// compositor takes over — it drags 1:1 with the pointer, keeps the window
/// on screen and raises it while it moves, exactly like a title bar drag.
fn setup_drag(app: &Shared, widget: &impl IsA<gtk::Widget>) {
    let gesture = gtk::GestureDrag::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let started = Rc::new(Cell::new(false));
    {
        let started = started.clone();
        let app = app.clone();
        gesture.connect_drag_begin(move |_, _, _| {
            started.set(false);
            touch(&app);
        });
    }
    {
        let started = started.clone();
        let app = app.clone();
        gesture.connect_drag_update(move |gesture, dx, dy| {
            // a click on the video must stay a click (it focuses the
            // window); only a real drag asks for the move
            if started.get() || !over_threshold(dx, dy) {
                return;
            }
            started.set(true);
            begin_move(&app, gesture);
        });
    }
    {
        let app = app.clone();
        gesture.connect_drag_end(move |_, _, _| touch(&app));
    }
    widget.add_controller(gesture);
}

/// `true` once a drag has travelled far enough to count as a drag.
fn over_threshold(dx: f64, dy: f64) -> bool {
    dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD
}

/// The `GdkToplevel` behind the app window — the object that sends
/// `xdg_toplevel.move` / `xdg_toplevel.resize` to the compositor.
fn toplevel(app: &Shared) -> Option<gdk::Toplevel> {
    app.window.surface()?.downcast::<gdk::Toplevel>().ok()
}

/// Device, position and time of the event GTK is dispatching right now:
/// the three arguments GDK's `begin_move` / `begin_resize` ask for. Falls
/// back to the seat's pointer when the controller has no current event.
fn current_input(
    controller: &impl IsA<gtk::EventController>,
) -> Option<(gdk::Device, f64, f64, u32)> {
    let device = controller.current_event_device().or_else(|| {
        gdk::Display::default()
            .and_then(|display| display.default_seat())
            .and_then(|seat| seat.pointer())
    })?;
    let (x, y) = controller
        .current_event()
        .and_then(|event| event.position())
        .unwrap_or((0.0, 0.0));
    Some((device, x, y, controller.current_event_time()))
}

/// Starts a compositor-side move of the window.
fn begin_move(app: &Shared, controller: &impl IsA<gtk::EventController>) {
    let Some(toplevel) = toplevel(app) else {
        return;
    };
    let Some((device, x, y, time)) = current_input(controller) else {
        return;
    };
    toplevel.begin_move(&device, gdk::BUTTON_PRIMARY as i32, x, y, time);
}

/// Starts a compositor-side resize of the window from `edge`.
fn begin_resize(app: &Shared, edge: gdk::SurfaceEdge, controller: &impl IsA<gtk::EventController>) {
    let Some(toplevel) = toplevel(app) else {
        return;
    };
    let Some((device, x, y, time)) = current_input(controller) else {
        return;
    };
    toplevel.begin_resize(edge, Some(&device), gdk::BUTTON_PRIMARY as i32, x, y, time);
}
