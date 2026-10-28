//! Playing a trailer inside the page.
//!
//! GTK can play media by itself, but only through a loadable backend that not
//! every build of GTK ships — the one in nixpkgs has none — so Divine drives
//! GStreamer directly instead. `playbin3` does the work and hands its frames to
//! `gtk4paintablesink`, which turns them into a [`gdk::Paintable`] that any
//! [`gtk::Picture`] can show.
//!
//! The trailers are ordinary MP4 over HTTPS and the server takes range
//! requests, so nothing is downloaded ahead of time: the pipeline pulls the
//! parts it needs as it plays. Hardware decoding is GStreamer's own decision —
//! it takes whatever decoder the machine has and falls back to software.
//!
//! Everything here degrades to nothing. A machine without the plugin gets no
//! player and the page shows what it always showed.

use anyhow::{Context, Result};
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

/// Whether trailers can be played at all. False on a machine without the
/// GStreamer plugin that draws into a GTK widget, where the pages simply carry
/// on showing their pictures.
pub fn available() -> bool {
    if gst::init().is_err() {
        return false;
    }
    gst::ElementFactory::find("gtk4paintablesink").is_some()
}
