//! Camera discovery and the GStreamer pipeline behind the preview.
//!
//! Pipeline: `device ! capsfilter ! [jpegdec] ! videoconvert ! gtk4paintablesink`
//! The sink hands a [`gtk4::Paintable`] to a `GtkPicture`.
//!
//! Most webcams deliver their high resolutions as **MJPEG only**: raw 1920x1080
//! is typically capped at 5 fps while MJPEG runs at 30 fps over the same bus.
//! The pipeline therefore asks for `image/jpeg` whenever the camera offers it
//! and decodes it in software (`jpegdec`, libjpeg-turbo — no hardware JPEG
//! decoder plugin is installed on this system).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use gstreamer as gst;
use gst::prelude::*;

/// One resolution a camera can deliver, with the best frame rate it offers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    /// `true` when the size is available as MJPEG (fast) rather than raw only.
    pub mjpeg: bool,
}

impl Format {
    /// `1920x1080 · 30 fps`
    pub fn label(&self) -> String {
        format!("{}x{} · {} fps", self.width, self.height, self.fps)
    }
}

/// A running camera preview.
pub struct Camera {
    pipeline: gst::Pipeline,
    sink: gst::Element,
    /// Frames seen by the sink, sampled by [`Camera::fps`]. Shared with the
    /// pad probe, which runs on the streaming thread.
    frames: Arc<AtomicU64>,
    /// `(last sample time, frame count then, fps reported then)`
    sample: RefCell<(Instant, u64, u32)>,
    /// Dropping the guard detaches the bus watch from the main loop.
    _watch: gst::bus::BusWatchGuard,
}

impl Camera {
    /// Builds and starts the pipeline for `device`.
    ///
    /// `format` is the resolution to stream; `None` lets the device pick.
    /// `on_error` is invoked (on the main loop) if the pipeline reports an error,
    /// e.g. because the camera is busy.
    pub fn open(
        device: &gst::Device,
        format: Option<Format>,
        on_error: impl Fn(String) + 'static,
    ) -> Result<Self, String> {
        let source = device.create_element(None).map_err(err)?;

        // MJPEG only when the size really is offered as MJPEG and a decoder exists
        let mjpeg = format.is_some_and(|f| f.mjpeg) && gst::ElementFactory::find("jpegdec").is_some();
        let caps = match format {
            Some(f) if mjpeg => gst::Caps::builder("image/jpeg")
                .field("width", f.width)
                .field("height", f.height)
                .field("framerate", gst::Fraction::new(f.fps.max(1), 1))
                .build(),
            Some(f) => gst::Caps::builder("video/x-raw")
                .field("width", f.width)
                .field("height", f.height)
                .field("framerate", gst::Fraction::new(f.fps.max(1), 1))
                .build(),
            // no known formats: accept whatever the camera likes best, but stay
            // away from raw modes the driver only does at a handful of fps
            None if device_supports_jpeg(device) => gst::Caps::builder("image/jpeg").build(),
            None => gst::Caps::builder("video/x-raw").build(),
        };
        let format_el = gst::ElementFactory::make("capsfilter")
            .property("caps", caps)
            .build()
            .map_err(err)?;
        let convert = gst::ElementFactory::make("videoconvert").build().map_err(err)?;
        let sink = gst::ElementFactory::make("gtk4paintablesink").build().map_err(err)?;

        let pipeline = gst::Pipeline::new();
        let decoder = if mjpeg {
            Some(gst::ElementFactory::make("jpegdec").build().map_err(err)?)
        } else {
            None
        };
        pipeline
            .add_many([&source, &format_el, &convert, &sink])
            .map_err(err)?;
        if let Some(decoder) = &decoder {
            pipeline.add(decoder).map_err(err)?;
        }
        source.link(&format_el).map_err(err)?;
        if let Some(decoder) = &decoder {
            format_el.link(decoder).map_err(err)?;
            decoder.link(&convert).map_err(err)?;
        } else {
            format_el.link(&convert).map_err(err)?;
        }
        convert.link(&sink).map_err(err)?;

        // count the frames that actually reach the sink (the fps read-out)
        let frames = Arc::new(AtomicU64::new(0));
        if let Some(pad) = sink.static_pad("sink") {
            let counter = Arc::clone(&frames);
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                counter.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
        }

        pipeline.set_state(gst::State::Playing).map_err(err)?;

        let bus = pipeline
            .bus()
            .ok_or_else(|| "the pipeline has no bus".to_string())?;
        let watch = bus
            .add_watch_local(move |_bus, message| {
                if let gst::MessageView::Error(error) = message.view() {
                    on_error(error.error().to_string());
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            })
            .map_err(err)?;

        Ok(Self {
            pipeline,
            sink,
            frames,
            sample: RefCell::new((Instant::now(), 0, 0)),
            _watch: watch,
        })
    }

    /// The texture the `GtkPicture` should display (may be `None` for the first
    /// frames, the sink fills it in later).
    pub fn paintable(&self) -> Option<gtk4::gdk::Paintable> {
        self.sink.property("paintable")
    }

    /// `1920x1080` style size of the negotiated format, once known.
    pub fn resolution(&self) -> Option<String> {
        let (width, height) = self.size()?;
        Some(format!("{width}x{height}"))
    }

    /// Negotiated size, once the pipeline has started.
    pub fn size(&self) -> Option<(i32, i32)> {
        let pad = self.sink.static_pad("sink")?;
        let caps = pad.current_caps()?;
        let structure = caps.structure(0)?;
        let width: i32 = structure.get("width").ok()?;
        let height: i32 = structure.get("height").ok()?;
        Some((width, height))
    }

    /// Frames per second measured at the sink. Needs two calls at least
    /// ~200 ms apart, otherwise the previous value is repeated.
    pub fn fps(&self) -> u32 {
        let now = Instant::now();
        let frames = self.frames.load(Ordering::Relaxed);
        let mut sample = self.sample.borrow_mut();
        let elapsed = now.duration_since(sample.0).as_secs_f64();
        if elapsed < 0.2 {
            return sample.2;
        }
        let fps = ((frames.saturating_sub(sample.1)) as f64 / elapsed).round() as u32;
        *sample = (now, frames, fps);
        fps
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Keeps a [`gst::DeviceMonitor`] running for the whole lifetime of the app so
/// that cameras plugged in later show up without restarting.
pub struct Devices {
    monitor: gst::DeviceMonitor,
    _watch: gst::bus::BusWatchGuard,
}

impl Devices {
    /// Starts monitoring; `on_change` runs on the main loop whenever a camera
    /// appears or disappears. `None` when the monitor cannot be started.
    pub fn watch(on_change: impl Fn() + 'static) -> Option<Self> {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), Some(&gst::Caps::builder("video/x-raw").build()));
        monitor.start().ok()?;
        // `start()` re-announces the cameras the monitor already knows about;
        // remember them so that the caller's initial refresh is not repeated
        // for every device that was there all along.
        let known: Rc<RefCell<HashSet<String>>> =
            Rc::new(RefCell::new(collect(&monitor).iter().map(key).collect()));
        let watch = monitor
            .bus()
            .add_watch_local({
                let known = Rc::clone(&known);
                move |_bus, message| {
                    let changed = match message.view() {
                        gst::MessageView::DeviceAdded(added) => {
                            known.borrow_mut().insert(key(&added.device()))
                        }
                        gst::MessageView::DeviceRemoved(removed) => {
                            known.borrow_mut().remove(&key(&removed.device()))
                        }
                        _ => false,
                    };
                    if changed {
                        on_change();
                    }
                    glib::ControlFlow::Continue
                }
            })
            .ok()?;
        Some(Self {
            monitor,
            _watch: watch,
        })
    }

    /// The cameras the monitor currently knows about.
    pub fn devices(&self) -> Vec<gst::Device> {
        collect(&self.monitor)
    }
}

/// All video sources the machine knows about, de-duplicated by device path
/// (the v4l2 and PipeWire providers happily report the same camera twice).
pub fn list_devices() -> Vec<gst::Device> {
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::builder("video/x-raw").build();
    let _ = monitor.add_filter(Some("Video/Source"), Some(&caps));
    if monitor.start().is_err() {
        return Vec::new();
    }
    let devices = collect(&monitor);
    monitor.stop();
    devices
}

fn collect(monitor: &gst::DeviceMonitor) -> Vec<gst::Device> {
    let mut seen = HashSet::new();
    let mut devices = Vec::new();
    for device in monitor.devices() {
        if seen.insert(key(&device)) {
            devices.push(device);
        }
    }
    devices
}

/// Every resolution the camera advertises, largest first, keeping the best
/// frame rate each size offers and whether it is available as MJPEG.
pub fn formats(device: &gst::Device) -> Vec<Format> {
    let Some(caps) = device.caps() else {
        return Vec::new();
    };
    let mut formats: Vec<Format> = Vec::new();
    for index in 0..caps.len() {
        let Some(structure) = caps.structure(index) else {
            continue;
        };
        // non-fixed entries (ranges) are skipped, real devices list them fixed
        let (Ok(width), Ok(height)) = (
            structure.get::<i32>("width"),
            structure.get::<i32>("height"),
        ) else {
            continue;
        };
        let fps = structure
            .get::<gst::Fraction>("framerate")
            .map(|rate| (rate.numer() as f64 / rate.denom() as f64).round() as i32)
            .unwrap_or(0)
            .max(1);
        let mjpeg = structure.name() == "image/jpeg";
        match formats
            .iter_mut()
            .find(|f| f.width == width && f.height == height)
        {
            Some(existing) => {
                existing.fps = existing.fps.max(fps);
                existing.mjpeg |= mjpeg;
            }
            None => formats.push(Format {
                width,
                height,
                fps,
                mjpeg,
            }),
        }
    }
    formats.sort_by(|a, b| {
        (b.width * b.height)
            .cmp(&(a.width * a.height))
            .then(b.fps.cmp(&a.fps))
    });
    formats
}

/// Short human readable name of a camera.
pub fn label(device: &gst::Device) -> String {
    let name = device.display_name().to_string();
    if name.is_empty() {
        key(device)
    } else {
        name
    }
}

/// Stable identifier, usually `/dev/video0`.
pub fn key(device: &gst::Device) -> String {
    if let Some(properties) = device.properties() {
        for field in [
            "device.path",
            "object.path",
            "api.v4l2.path",
            "api.pipewire.path",
            "device.name",
        ] {
            if let Ok(value) = properties.get::<String>(field) {
                if let Some(index) = value.find("/dev/") {
                    return value[index..].to_string();
                }
                if !value.is_empty() {
                    return value;
                }
            }
        }
    }
    device.display_name().to_string()
}

/// One label per device; duplicates get the device path appended.
pub fn labels(devices: &[gst::Device]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for device in devices {
        let name = label(device);
        if labels.contains(&name) {
            labels.push(format!("{name} ({})", key(device)));
        } else {
            labels.push(name);
        }
    }
    labels
}

/// `true` when the device advertises an `image/jpeg` mode.
fn device_supports_jpeg(device: &gst::Device) -> bool {
    let Some(caps) = device.caps() else {
        return false;
    };
    (0..caps.len()).any(|index| {
        caps.structure(index)
            .is_some_and(|structure| structure.name() == "image/jpeg")
    })
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
