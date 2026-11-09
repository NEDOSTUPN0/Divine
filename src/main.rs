// Divine - native GTK4 launcher for Epic Games, powered by Legendary.
// Copyright (C) 2026 NEDOSTUPN0
// SPDX-License-Identifier: GPL-3.0-or-later

//! Divine wraps the legendary CLI in a GTK4 front end. legendary owns the Epic
//! account, the library and the downloads; everything else — the store text,
//! achievements, playtime, Proton builds, the launch environment — is Divine's,
//! and each of those is a module below.

mod accounts;
mod cleanup;
mod config;
mod covers;
mod discord;
mod disk;
mod epic;
mod exe_icon;
mod gamescope;
mod gamesdb;
mod http;
mod known_fixes;
mod launch_options;
mod legendary;
mod log;
mod models;
mod play;
mod proton;
mod protondb;
mod proxy;
mod relay;
mod runner;
mod running;
mod runtimes;
mod sdl;
mod shim;
mod shortcuts;
mod ui;
mod umu;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::log::{error, warn};

pub const APP_ID: &str = "io.github.NEDOSTUPN0.Divine";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> glib::ExitCode {
    if let Err(e) = config::ensure_dirs() {
        error!("cannot prepare config directories: {e:#}");
        return glib::ExitCode::FAILURE;
    }

    // A shortcut: the game and nothing else, with a log of its own so that it
    // does not push the window's aside.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(play::LAUNCH) {
        let Some(app_name) = args.get(2) else {
            error!("{} needs the app name of a game", play::LAUNCH);
            return glib::ExitCode::FAILURE;
        };
        if let Err(e) = log::to_file(&config::launch_log_file()) {
            warn!("this launch will not be written down: {e:#}");
        }
        return glib::ExitCode::from(play::from_shortcut(app_name).clamp(0, 255) as u8);
    }

    if let Err(e) = log::to_file(&config::log_file()) {
        warn!("this session will not be written down: {e:#}");
    }

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| load_css());
    app.connect_activate(|app| {
        ui::window::App::present(app);
    });
    // Runs in the window's process, whichever process the line was typed in.
    app.connect_command_line(|app, line| {
        let args: Vec<String> = line
            .arguments()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let window = ui::window::App::present(app);
        if let Some(request) = ui::window::Request::parse(&args) {
            window.request(request);
        }
        glib::ExitCode::SUCCESS
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
