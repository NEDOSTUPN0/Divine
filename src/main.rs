// Divine - native GTK4 launcher for Epic Games, powered by Legendary.
// Copyright (C) 2026 NEDOSTUPN0
// SPDX-License-Identifier: GPL-3.0-or-later

//! Divine wraps the legendary CLI in a GTK4 front end. legendary owns the Epic
//! account, the library and the downloads; everything else — the store text,
//! achievements, playtime, Proton builds, the launch environment — is Divine's,
//! and each of those is a module below.

mod cleanup;
mod config;
mod covers;
mod discord;
mod disk;
mod epic;
mod gamesdb;
mod http;
mod legendary;
mod log;
mod models;
mod proton;
mod protondb;
mod runner;
mod running;
mod runtimes;
mod sdl;
mod shim;
mod throttle;
mod ui;
mod umu;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::log::{error, warn};

pub const APP_ID: &str = "io.github.NEDOSTUPN0.Divine";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> glib::ExitCode {
    if let Err(e) = config::ensure_dirs() {
        error!("cannot prepare config directories: {e:#}");
        return glib::ExitCode::FAILURE;
    }
    if let Err(e) = log::to_file(&config::log_file()) {
        warn!("this session will not be written down: {e:#}");
    }

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| load_css());
    app.connect_activate(|app| {
        ui::window::App::build(app);
    });
    app.run()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../data/style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
