//! Playing a trailer inside the page.
//!
//! GTK can play media by itself, but only through a loadable backend that not
//! every build of GTK ships — the one in nixpkgs has none — so Divine drives
//! GStreamer directly instead. `playbin3` does the work and hands its frames to
//! `gtk4paintablesink`, which turns them into a [`gdk::Paintable`] that any
//! [`gtk::Picture`] can show.
//!
//! The trailers are either MP4 over HTTPS, which the server serves in ranges,
//! or HLS and DASH streams, which hlsdemux2 and dashdemux2 from
//! gst-plugins-good follow. Nothing is downloaded ahead of time either way:
//! the pipeline pulls the parts it needs as it plays. Hardware decoding is GStreamer's own decision —
//! it takes whatever decoder the machine has and falls back to software.
//!
//! Everything here degrades to nothing. A machine without the plugin gets no
//! player and the page shows what it always showed.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use gst::prelude::*;
use gtk::{gdk, glib};

use crate::log::debug;

/// One trailer, playing or paused.
///
/// Dropping it takes the pipeline down, which is what stops the sound when a
/// page is closed halfway through.
pub struct Player {
    pipeline: gst::Element,
    paintable: gdk::Paintable,
}

impl Player {
    /// Builds a pipeline for `url`, paused on its first frame.
    pub fn new(url: &str) -> Result<Self> {
        gst::init().context("starting GStreamer")?;

        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .context("gtk4paintablesink is not installed")?;
        let paintable: gdk::Paintable = sink.property("paintable");

        let pipeline = gst::ElementFactory::make("playbin3")
            .property("uri", url)
            .property("video-sink", &sink)
            .build()
            .context("building the pipeline")?;

        follow_proxy(&pipeline);

        // Paused rather than playing: this fills the first frame in, so the
        // widget shows the trailer's own opening rather than a black square.
        pipeline
            .set_state(gst::State::Paused)
            .context("opening the trailer")?;

        Ok(Self {
            pipeline,
            paintable,
        })
    }

    /// What to hand a [`gtk::Picture`] to show what is playing.
    pub fn paintable(&self) -> &gdk::Paintable {
        &self.paintable
    }

    /// Between 0 and 1. Set before playing, so a trailer never speaks first.
    pub fn set_volume(&self, volume: f64) {
        self.pipeline.set_property("volume", volume.clamp(0.0, 1.0));
    }

    pub fn set_muted(&self, muted: bool) {
        self.pipeline.set_property("mute", muted);
    }

    /// How far in it is, and how long it runs. Either can be missing while the
    /// pipeline is still working out what it has.
    pub fn position(&self) -> Option<gst::ClockTime> {
        self.pipeline.query_position::<gst::ClockTime>()
    }

    pub fn duration(&self) -> Option<gst::ClockTime> {
        self.pipeline.query_duration::<gst::ClockTime>()
    }

    /// Jumps to `at`. Flushing, so the picture follows the slider rather than
    /// finishing what it had buffered first.
    pub fn seek_to(&self, at: gst::ClockTime) {
        let _ = self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, at);
    }

    pub fn is_playing(&self) -> bool {
        self.pipeline.current_state() == gst::State::Playing
    }

    pub fn play(&self) {
        let _ = self.pipeline.set_state(gst::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
    }

    /// Back to the beginning, stopped. This is what a trailer scrolled past
    /// gets, so coming back to it starts it over rather than halfway through.
    pub fn rewind(&self) {
        let _ = self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::ZERO,
        );
        self.pause();
    }

    /// Whether the trailer has reached its end, which is the one thing a
    /// caller has to ask about: the pipeline stops on its own but the button
    /// over it has to go back to saying Play.
    pub fn watch_end(&self, ended: impl Fn() + 'static) {
        let Some(bus) = self.pipeline.bus() else {
            return;
        };
        let _ = bus.add_watch_local(move |_, message| {
            match message.view() {
                gst::MessageView::Eos(_) => ended(),
                gst::MessageView::Error(e) => {
                    debug!("the trailer stopped: {}", e.error());
                    ended();
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// souphttpsrc fetches the trailer, and takes its proxy from here rather than
/// from anything Divine's own requests go through.
fn follow_proxy(pipeline: &gst::Element) {
    if let Some(relay) = crate::relay::address() {
        pipeline.connect("source-setup", false, move |values| {
            let source = values.get(1)?.get::<gst::Element>().ok()?;
            if source.has_property("proxy") {
                source.set_property("proxy", &relay);
            }
            None
        });
    }
}

/// How long opening a trailer or finding a frame in it may take before the
/// poster is given up on.
const FRAME_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(20);

/// How far into a trailer its stand-in poster is taken from. Trailers tend to
/// open on black or on a publisher's logo, and a few seconds in is past both.
const FRAME_AT: gst::ClockTime = gst::ClockTime::from_seconds(5);

/// A poster for a trailer the store gave none, cut from the trailer itself and
/// kept beside the screenshots under the trailer's URL. Blocking: it opens the
/// stream, which means a request or two and the start of the file.
pub fn poster_frame(url: &str) -> Result<PathBuf> {
    let dir = crate::covers::screenshots_dir();
    let path = dir.join(format!("{}.img", crate::covers::screenshot_name(url)));
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let image = grab_frame(url).context("taking a frame from the trailer")?;
    // Written aside and moved into place, so a page opened halfway through
    // never takes half a picture for the whole one.
    let part = path.with_extension("part");
    fs::write(&part, image).with_context(|| format!("writing {}", part.display()))?;
    fs::rename(&part, &path).with_context(|| format!("moving {} into place", path.display()))?;
    Ok(path)
}

/// One frame of `url`, as a JPEG.
///
/// playbin3 opens it into sinks that throw everything away, and `convert-sample`
/// hands back the frame the video sink was last given, encoded as asked. The
/// video sink asks for plain `video/x-raw`: a sink that takes anything lets a
/// hardware decoder keep its frames in GPU memory, as NVIDIA's does in CUDA,
/// and the conversion cannot read them out of there.
fn grab_frame(url: &str) -> Result<Vec<u8>> {
    gst::init().context("starting GStreamer")?;
    let video_sink =
        gst::parse::bin_from_description("capsfilter caps=video/x-raw ! fakesink sync=false", true)
            .context("building the video sink")?;
    let audio_sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .context("building the audio sink")?;
    let pipeline = gst::ElementFactory::make("playbin3")
        .property("uri", url)
        .property("video-sink", &video_sink)
        .property("audio-sink", &audio_sink)
        .build()
        .context("building the pipeline")?;
    follow_proxy(&pipeline);
    let frame = settle_and_grab(&pipeline);
    let _ = pipeline.set_state(gst::State::Null);
    frame
}

fn settle_and_grab(pipeline: &gst::Element) -> Result<Vec<u8>> {
    pipeline
        .set_state(gst::State::Paused)
        .context("opening the trailer")?;
    settle(pipeline)?;

    // A trailer shorter than that is shown from its middle.
    let at = match pipeline.query_duration::<gst::ClockTime>() {
        Some(length) if length < FRAME_AT * 2 => length / 2,
        _ => FRAME_AT,
    };
    pipeline
        .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, at)
        .context("seeking into the trailer")?;
    settle(pipeline)?;

    let caps = gst::Caps::builder("image/jpeg").build();
    let sample = pipeline
        .emit_by_name::<Option<gst::Sample>>("convert-sample", &[&caps])
        .context("the trailer gave no frame")?;
    let buffer = sample.buffer().context("the frame is empty")?;
    let map = buffer.map_readable().context("reading the frame")?;
    Ok(map.as_slice().to_vec())
}

/// Waits for the pipeline to finish what it was last asked to do.
fn settle(pipeline: &gst::Element) -> Result<()> {
    let (result, _, _) = pipeline.state(FRAME_TIMEOUT);
    match result {
        Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll) => Ok(()),
        Ok(gst::StateChangeSuccess::Async) => bail!("the trailer took too long to open"),
        Err(_) => bail!("the trailer would not open: {}", bus_error(pipeline)),
    }
}

/// What went wrong, as GStreamer told its bus.
fn bus_error(pipeline: &gst::Element) -> String {
    pipeline
        .bus()
        .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
        .and_then(|message| match message.view() {
            gst::MessageView::Error(e) => Some(e.error().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "no reason given".into())
}

/// Whether trailers can be played at all. False on a machine without the
/// GStreamer plugin that draws into a GTK widget, where the pages simply carry
/// on showing their pictures.
pub fn available() -> bool {
    if gst::init().is_err() {
        return false;
    }
    gst::ElementFactory::find("gtk4paintablesink").is_some()
}
