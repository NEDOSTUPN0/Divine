//! Global settings. Everything here is applied per launch, never written into
//! legendary's own config.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;

use crate::cleanup::{self, Sweep};
use crate::config::{Config, ProxyKind, LATEST_GE_PROTON};
use crate::epic::playtime;
use crate::legendary::Legendary;
use crate::models::human_size;
use crate::runner::Runner;
use crate::ui::proton_page;
use crate::ui::spawn_bg;
use crate::ui::{accessible_label, window::App, wrapping_list};

/// Writes one setting through, the moment it is changed.
///
/// Nothing here waits for an OK button, because there is none: closing a
/// preferences dialog is not a decision, and collecting the whole page at that
/// point meant a session that ended any other way threw the lot away. Each row
/// hands over its own field and nothing else.
fn apply(app: &Rc<App>, change: impl FnOnce(&mut Config)) {
    change(&mut app.cfg.borrow_mut());
    let cfg = app.cfg.borrow().clone();
    // legendary is told which binary to run and how fast to download through
    // this, so it is rebuilt from whatever the config now says.
    *app.lgd.borrow_mut() = Legendary::new(&cfg);
    crate::proxy::apply(&cfg.proxy);
    if let Err(e) = cfg.save() {
        app.toast_error("Cannot save preferences", &e);
    }
}

/// A switch that writes its own setting as it is flipped.
fn switch(
    app: &Rc<App>,
    title: &str,
    subtitle: &str,
    active: bool,
    set: impl Fn(&mut Config, bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build();

    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let on = row.is_active();
        apply(&app, |cfg| set(cfg, on));
    });
    row
}

pub fn show(app: &Rc<App>) {
    let cfg = app.cfg.borrow().clone();
    app.rescan_runners();

    let umu = switch(
        app,
        "Use umu",
        "Sets up the prefix, applies protonfixes and the Steam Linux Runtime. \
         Turning this off is not recommended. A single game can override it \
         in its own settings.",
        cfg.runner.use_umu,
        |cfg, on| cfg.runner.use_umu = on,
    );

    let proton = adw::ComboRow::builder()
        .title("Proton")
        .use_markup(false)
        .list_factory(&wrapping_list())
        .build();
    fill_protons(app, &proton);
    let weak = Rc::downgrade(app);
    proton.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let picked = app
            .runners
            .borrow()
            .get(row.selected() as usize)
            .map(Runner::proton_path);
        let picked = picked.unwrap_or_else(|| LATEST_GE_PROTON.to_string());
        row.set_subtitle(subtitle_for(&app, row.selected()));
        apply(&app, |cfg| cfg.runner.proton = picked);
    });

    let esync = switch(
        app,
        "Esync",
        "Eventfd based synchronisation",
        cfg.runner.esync,
        |cfg, on| cfg.runner.esync = on,
    );
    let fsync = switch(
        app,
        "Fsync",
        "Futex based synchronisation, faster than esync",
        cfg.runner.fsync,
        |cfg, on| cfg.runner.fsync = on,
    );
    let ntsync = switch(
        app,
        "Ntsync",
        "Kernel side synchronisation, needs Linux 6.14 or newer. Wine prefers it \
         over esync and fsync where it is available.",
        cfg.runner.ntsync,
        |cfg, on| cfg.runner.ntsync = on,
    );

    let sync_group = adw::PreferencesGroup::builder()
        .title("Synchronisation")
        .description("Proton enables all of these; turning one off passes PROTON_NO_* to it")
        .build();
    sync_group.add(&ntsync);
    sync_group.add(&fsync);
    sync_group.add(&esync);

    let wayland = switch(
        app,
        "Wine Wayland",
        "Experimental. Runs the game on winewayland instead of XWayland, and only \
         works on a Proton build that supports it: GE-Proton and CachyOS do, \
         Valve's does not. Ignored outside a Wayland session.",
        cfg.runner.wayland,
        |cfg, on| cfg.runner.wayland = on,
    );

    let hdr = switch(
        app,
        "HDR",
        "Passes PROTON_ENABLE_HDR. Only reaches the game with Wine Wayland on, \
         and needs a display and a Proton build that support it.",
        cfg.runner.hdr,
        |cfg, on| cfg.runner.hdr = on,
    );

    let eos_overlay = switch(
        app,
        "EOS Overlay",
        "Epic's in-game overlay, for the games that use Epic Online Services. \
         One shared download of about 600 MB, then switched on in each \
         game's prefix. DirectX 12 games usually cannot show it under Proton: \
         it opens and takes the input, but nothing is drawn.",
        cfg.runner.eos_overlay,
        |cfg, on| cfg.runner.eos_overlay = on,
    );
    // HDR rides on winewayland, so it is out of reach while that is off.
    wayland
        .bind_property("active", &hdr, "sensitive")
        .sync_create()
        .build();

    let weak = Rc::downgrade(app);
    eos_overlay.connect_active_notify(move |row| {
        if let (Some(app), true) = (weak.upgrade(), row.is_active()) {
            crate::ui::ensure_eos_overlay(&app);
        }
    });

    let cloud_saves = switch(
        app,
        "Cloud Saves",
        "Sync saves with Epic before a game starts and after it exits, for the \
         games that keep them there.",
        cfg.runner.cloud_saves,
        |cfg, on| cfg.runner.cloud_saves = on,
    );

    let report_playtime = switch(
        app,
        "Report Playtime to Epic",
        "Sends each finished session to the same service Epic's own launcher \
         uses, so its counter keeps up with Divine. Divine records playtime \
         either way; this only decides whether it leaves the machine.",
        cfg.runner.report_playtime,
        |cfg, on| cfg.runner.report_playtime = on,
    );
    warn_about_old_sessions(app, &report_playtime);

    let gamemode = switch(
        app,
        "GameMode",
        "Launch through gamemoderun",
        cfg.runner.gamemode,
        |cfg, on| cfg.runner.gamemode = on,
    );
    let mangohud = switch(
        app,
        "MangoHud",
        "Performance overlay",
        cfg.runner.mangohud,
        |cfg, on| cfg.runner.mangohud = on,
    );

    let steam_protons = switch(
        app,
        "Show Steam Proton Versions",
        "Valve's own builds, listed alongside the recommended ones",
        cfg.runner.show_steam_proton,
        |cfg, on| cfg.runner.show_steam_proton = on,
    );
    confirm_steam_protons(app, &steam_protons);
    // The list above is what this setting decides, so it is redrawn with it.
    let weak = Rc::downgrade(app);
    let listed = proton.clone();
    steam_protons.connect_active_notify(move |_| {
        if let Some(app) = weak.upgrade() {
            app.rescan_runners();
            fill_protons(&app, &listed);
        }
    });

    let runner_group = adw::PreferencesGroup::builder()
        .title("Proton")
        .description("Which build runs the games, and how")
        .build();
    runner_group.add(&umu);
    runner_group.add(&proton);
    runner_group.add(&steam_protons);
    runner_group.add(&wayland);
    runner_group.add(&hdr);

    let epic_group = adw::PreferencesGroup::builder()
        .title("Epic Services")
        .description("What Divine asks of Epic beyond downloading the games")
        .build();
    epic_group.add(&eos_overlay);
    epic_group.add(&cloud_saves);
    epic_group.add(&report_playtime);

    let limited = adw::SwitchRow::builder()
        .title("Limit Download Speed")
        .active(cfg.downloads.limit > 0)
        .build();
    let limit = adw::SpinRow::builder()
        .title("Ceiling")
        .subtitle("MiB/s, shared by every download at once")
        .adjustment(&gtk::Adjustment::new(
            speed_of(&cfg),
            0.1,
            1000.0,
            0.1,
            1.0,
            0.0,
        ))
        .digits(1)
        .sensitive(cfg.downloads.limit > 0)
        .build();
    limited
        .bind_property("active", &limit, "sensitive")
        .sync_create()
        .build();

    // Both rows say the same thing between them, so they save the same way.
    let save_limit = {
        let weak = Rc::downgrade(app);
        let limited = limited.clone();
        let limit = limit.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let kib = if limited.is_active() {
                (limit.value() * 1024.0).round() as u64
            } else {
                0
            };
            apply(&app, |cfg| cfg.downloads.limit = kib);
            // A download already running picks the new ceiling up straight
            // away, as long as it was started with one to begin with.
            crate::relay::set_limit(app.cfg.borrow().downloads.limit_bytes());
        }
    };
    let on_toggle = save_limit.clone();
    limited.connect_active_notify(move |_| on_toggle());
    limit.connect_value_notify(move |_| save_limit());

    let downloads_group = adw::PreferencesGroup::builder()
        .title("Downloads")
        .description(
            "legendary has no bandwidth setting, so Divine runs its downloads \
             through a local proxy and holds the ceiling there",
        )
        .build();
    downloads_group.add(&limited);
    downloads_group.add(&limit);

    let tools_group = adw::PreferencesGroup::builder().title("Tools").build();
    tools_group.add(&gamemode);
    tools_group.add(&mangohud);

    let defaults = Config::default();
    let install_dir = path_row(
        app,
        "Game Install Folder",
        &cfg.general.install_dir,
        defaults.general.install_dir,
        |cfg, path| cfg.general.install_dir = path,
    );
    let discord_folders = switch(
        app,
        "Name Folders for Discord",
        "A new install goes into the folder Discord recognises the game by, so \
         it can show what you are playing. When Discord has no answer, the \
         folder keeps Epic's name.",
        cfg.downloads.discord_folders,
        |cfg, on| cfg.downloads.discord_folders = on,
    );
    let prefixes_dir = path_row(
        app,
        "Prefix Folder",
        &cfg.general.prefixes_dir,
        defaults.general.prefixes_dir,
        |cfg, path| cfg.general.prefixes_dir = path,
    );
    let username = text_row(
        app,
        "Name Games See",
        &cfg.runner.override_username,
        "",
        |cfg, name| cfg.runner.override_username = name,
    );
    let legendary_bin = text_row(
        app,
        "legendary",
        &cfg.general.legendary,
        &defaults.general.legendary,
        |cfg, bin| cfg.general.legendary = bin,
    );
    let umu_bin = text_row(
        app,
        "umu-run",
        &cfg.general.umu,
        &defaults.general.umu,
        |cfg, bin| cfg.general.umu = bin,
    );
    // A different legendary knows about different games, so the library has to
    // be read again once one is pointed at.
    let weak = Rc::downgrade(app);
    legendary_bin.connect_apply(move |_| {
        if let Some(app) = weak.upgrade() {
            app.refresh();
        }
    });

    let paths_group = adw::PreferencesGroup::builder()
        .title("Paths")
        .description("Leave a field empty to restore its default")
        .build();
    paths_group.add(&install_dir);
    paths_group.add(&discord_folders);
    paths_group.add(&prefixes_dir);

    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    general.add(&runner_group);
    general.add(&sync_group);
    general.add(&epic_group);
    general.add(&downloads_group);
    general.add(&proxy_group(app, &cfg));
    general.add(&tools_group);
    general.add(&paths_group);

    let dialog = adw::PreferencesDialog::new();
    dialog.add(&general);
    dialog.add(&proton_page::page(app));
    dialog.add(&advanced_page(
        app,
        &dialog,
        &legendary_bin,
        &umu_bin,
        &username,
    ));
    dialog.present(Some(&app.window));
}

/// Where everything goes out through, for a network that cannot reach Epic
/// directly. The details fold away behind the switch, since most people never
/// need them.
fn proxy_group(app: &Rc<App>, cfg: &Config) -> adw::PreferencesGroup {
    let proxy = &cfg.proxy;
    let defaults = Config::default().proxy;

    let use_proxy = adw::ExpanderRow::builder()
        .title("Use a Proxy")
        .show_enable_switch(true)
        .enable_expansion(proxy.enabled)
        .build();
    let weak = Rc::downgrade(app);
    use_proxy.connect_enable_expansion_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            let on = row.enables_expansion();
            apply(&app, |cfg| cfg.proxy.enabled = on);
        }
    });

    const KINDS: [(ProxyKind, &str); 2] =
        [(ProxyKind::Http, "HTTP"), (ProxyKind::Socks5, "SOCKS5")];
    let kind = adw::ComboRow::builder()
        .title("Type")
        .model(&gtk::StringList::new(&KINDS.map(|(_, name)| name)))
        .selected(
            KINDS
                .iter()
                .position(|(kind, _)| *kind == proxy.kind)
                .unwrap_or(0) as u32,
        )
        .build();
    let weak = Rc::downgrade(app);
    kind.connect_selected_notify(move |row| {
        if let (Some(app), Some((kind, _))) = (weak.upgrade(), KINDS.get(row.selected() as usize)) {
            apply(&app, |cfg| cfg.proxy.kind = *kind);
        }
    });

    let host = text_row(app, "Server", &proxy.host, &defaults.host, |cfg, host| {
        cfg.proxy.host = host
    });
    let port = adw::SpinRow::builder()
        .title("Port")
        .adjustment(&gtk::Adjustment::new(
            proxy.port as f64,
            1.0,
            65535.0,
            1.0,
            10.0,
            0.0,
        ))
        .build();
    let weak = Rc::downgrade(app);
    port.connect_value_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            let port = row.value() as u16;
            apply(&app, |cfg| cfg.proxy.port = port);
        }
    });

    let username = text_row(app, "Username", &proxy.username, "", |cfg, name| {
        cfg.proxy.username = name
    });
    // Not text_row: a password is taken exactly as typed, spaces and all.
    let password = adw::PasswordEntryRow::builder()
        .title("Password")
        .text(&proxy.password)
        .show_apply_button(true)
        .build();
    let weak = Rc::downgrade(app);
    password.connect_apply(move |row| {
        if let Some(app) = weak.upgrade() {
            let password = row.text().to_string();
            apply(&app, |cfg| cfg.proxy.password = password);
        }
    });

    use_proxy.add_row(&kind);
    use_proxy.add_row(&host);
    use_proxy.add_row(&port);
    use_proxy.add_row(&username);
    use_proxy.add_row(&password);

    let group = adw::PreferencesGroup::builder()
        .title("Proxy")
        .description(
            "For a network that cannot reach Epic directly. Divine, legendary, umu \
             and the games Divine starts all connect through it. Leave the username \
             empty for a proxy that does not ask for one.",
        )
        .build();
    group.add(&use_proxy);
    group
}

/// Redraws the list of Proton builds from what is on disk now, keeping the
/// selection on whatever the config asks for.
fn fill_protons(app: &Rc<App>, row: &adw::ComboRow) {
    let runners = app.runners.borrow();
    let labels: Vec<&str> = runners.iter().map(|runner| runner.name.as_str()).collect();
    let chosen = app.cfg.borrow().runner.proton.clone();
    let selected = runners
        .iter()
        .position(|runner| runner.proton_path() == chosen)
        .unwrap_or(0) as u32;

    row.set_model(Some(&gtk::StringList::new(&labels)));
    row.set_selected(selected);
    row.set_subtitle(
        runners
            .get(selected as usize)
            .map(Runner::subtitle)
            .unwrap_or_default(),
    );
}

fn subtitle_for(app: &Rc<App>, index: u32) -> &'static str {
    app.runners
        .borrow()
        .get(index as usize)
        .map(Runner::subtitle)
        .unwrap_or_default()
}

/// The stored ceiling in MiB/s, or a sane starting point for someone who is
/// switching the limit on for the first time.
fn speed_of(cfg: &Config) -> f64 {
    match cfg.downloads.limit {
        0 => 10.0,
        kib => kib as f64 / 1024.0,
    }
}

/// Housekeeping nobody needs on an ordinary day.
fn advanced_page(
    app: &Rc<App>,
    dialog: &adw::PreferencesDialog,
    legendary_bin: &adw::EntryRow,
    umu_bin: &adw::EntryRow,
    username: &adw::EntryRow,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Advanced")
        .icon_name("applications-engineering-symbolic")
        .build();

    let programs = adw::PreferencesGroup::builder()
        .title("Programs")
        .description(
            "The two Divine drives. A bare name is looked up on PATH, so the one \
             the system installed keeps its place; a full path picks a particular \
             build instead. Leave a field empty to restore its default.",
        )
        .build();
    programs.add(legendary_bin);
    programs.add(umu_bin);
    page.add(&programs);

    let identity = adw::PreferencesGroup::builder()
        .title("Identity")
        .description(
            "Some games put the Epic display name in their own interface, and this \
             replaces it there. Nothing else changes: the account id and the login \
             code stay real, so Epic and any anti-cheat still see the real name, and \
             a game that asks Epic over the network rather than reading its launch \
             arguments will not notice. Leave it empty to use the real one. A single \
             game can override this in its own settings.",
        )
        .build();
    identity.add(username);
    page.add(&identity);

    let logs = adw::ActionRow::builder()
        .title("Logs")
        .subtitle("What Divine and the programs it runs have said")
        .activatable(true)
        .build();
    logs.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let weak = Rc::downgrade(app);
    let showing = dialog.clone();
    logs.connect_activated(move |_| {
        if let Some(app) = weak.upgrade() {
            showing.push_subpage(&crate::ui::logs::page(&app));
        }
    });

    let troubleshooting = adw::PreferencesGroup::builder()
        .title("Troubleshooting")
        .build();
    troubleshooting.add(&logs);
    page.add(&troubleshooting);

    let row = adw::ActionRow::builder()
        .title("Unused Cached Data")
        .subtitle("Looking…")
        .build();
    let clear = gtk::Button::builder()
        .label("Clear")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    row.add_suffix(&clear);

    let group = adw::PreferencesGroup::builder()
        .title("Storage")
        .description(
            "Cover art, screenshots and manifests kept so they need not be fetched \
             again. Only what nothing in the library points at any more is removed: \
             art a game no longer uses, pages for games no longer owned, and the \
             manifests of builds that have been updated past. A download in progress \
             keeps everything it needs.",
        )
        .build();
    group.add(&row);
    page.add(&group);

    // Held between the two steps so Clear removes exactly what was counted,
    // rather than whatever a second look might turn up.
    let found: Rc<RefCell<Option<Sweep>>> = Rc::new(RefCell::new(None));

    let games = app.games.borrow().clone();
    let busy: HashSet<String> = app.busy.borrow().keys().cloned().collect();
    let counted = found.clone();
    let filling = row.clone();
    let enabling = clear.clone();
    spawn_bg(
        move || cleanup::unused(&games, &busy),
        move |sweep| {
            if sweep.is_empty() {
                filling.set_subtitle("Nothing to clear");
            } else {
                filling.set_subtitle(&format!(
                    "{} across {} files",
                    human_size(sweep.bytes),
                    sweep.files.len()
                ));
                enabling.set_sensitive(true);
            }
            *counted.borrow_mut() = Some(sweep);
        },
    );

    let weak = Rc::downgrade(app);
    let showing = row.clone();
    clear.connect_clicked(move |button| {
        let Some(sweep) = found.borrow_mut().take() else {
            return;
        };
        button.set_sensitive(false);
        let weak = weak.clone();
        let showing = showing.clone();
        spawn_bg(
            move || cleanup::remove(&sweep),
            move |freed| {
                showing.set_subtitle("Nothing to clear");
                if let Some(app) = weak.upgrade() {
                    app.toast(&format!("Freed {}", human_size(freed)));
                }
            },
        );
    });

    page
}

/// A text field that writes its value when the apply button is pressed.
///
/// On demand rather than on every keystroke: half a typed path is not a path,
/// and the field would otherwise save a dozen broken ones on the way to a good
/// one. An empty field falls back to `fallback`, which is how a setting is put
/// back to its default; passing an empty fallback means empty is a real answer.
fn text_row(
    app: &Rc<App>,
    title: &str,
    value: &str,
    fallback: &str,
    set: impl Fn(&mut Config, String) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).text(value).build();
    row.set_show_apply_button(true);

    let weak = Rc::downgrade(app);
    let fallback = fallback.to_string();
    row.connect_apply(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let text = row.text();
        let text = text.trim();
        let value = if text.is_empty() {
            fallback.clone()
        } else {
            text.to_string()
        };
        row.set_text(&value);
        apply(&app, |cfg| set(cfg, value));
    });
    row
}

/// A path field with a browse button, because typing one out is no fun.
fn path_row(
    app: &Rc<App>,
    title: &str,
    value: &Path,
    fallback: PathBuf,
    set: impl Fn(&mut Config, PathBuf) + 'static,
) -> adw::EntryRow {
    let row = text_row(
        app,
        title,
        &value.display().to_string(),
        &fallback.display().to_string(),
        move |cfg, text| set(cfg, PathBuf::from(text)),
    );

    let browse = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Choose a Folder")
        .valign(gtk::Align::Center)
        .build();
    browse.add_css_class("flat");
    accessible_label(&browse, "Choose a Folder");
    row.add_suffix(&browse);

    let weak = Rc::downgrade(app);
    let entry = row.clone();
    let title = title.to_string();
    browse.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let chooser = gtk::FileDialog::builder().title(&title).modal(true).build();
        let current = PathBuf::from(entry.text().to_string());
        if current.is_dir() {
            chooser.set_initial_folder(Some(&gtk::gio::File::for_path(&current)));
        }
        let entry = entry.clone();
        chooser.select_folder(
            Some(&app.window),
            gtk::gio::Cancellable::NONE,
            move |result| {
                let Some(path) = result.ok().and_then(|folder| folder.path()) else {
                    return;
                };
                entry.set_text(&path.display().to_string());
                // Picked from a chooser rather than typed, so there is nothing
                // half-finished to wait for: it is applied at once.
                entry.emit_by_name::<()>("apply", &[]);
            },
        );
    });
    row
}

/// Only the last day of the journal is ever reported, so switching reporting on
/// after a long time leaves Epic's counter behind for good. Say so once, rather
/// than let the two numbers quietly disagree.
fn warn_about_old_sessions(app: &Rc<App>, row: &adw::SwitchRow) {
    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        if !row.is_active() {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let stale = crate::epic::playtime::stale();
        if stale.is_empty() {
            return;
        }

        let played =
            crate::epic::playtime::human(stale.iter().map(playtime::Session::seconds).sum());
        let sessions = match stale.len() {
            1 => "one session".to_string(),
            count => format!("{count} sessions"),
        };
        let dialog = adw::AlertDialog::new(
            Some("Older playtime stays on this machine"),
            Some(&format!(
                "Divine has {played} recorded over {sessions} more than a day ago. Only the \
                 last day is ever reported, so Epic's counter will stay that much behind the \
                 hours Divine shows. From now on each session is sent as it ends."
            )),
        );
        dialog.add_response("ok", "Got it");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("ok");
        dialog.present(Some(&app.window));
    });
}

/// Steam's builds are hidden by default, so ask before putting them on the list.
fn confirm_steam_protons(app: &Rc<App>, row: &adw::SwitchRow) {
    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        if !row.is_active() {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Show Steam Proton versions?"),
            Some(
                "Valve's builds work, but GE-Proton and Proton CachyOS carry the media \
                 codecs and game fixes most Epic titles need. Sticking to the suggested \
                 versions is recommended.",
            ),
        );
        dialog.add_response("cancel", "Cancel");
        // Plain, neither suggested nor destructive: nothing is lost by going
        // on, and it is not the choice being recommended either.
        dialog.add_response("continue", "Continue");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let row = row.clone();
        dialog.choose(
            Some(&app.window),
            gtk::gio::Cancellable::NONE,
            move |response| {
                if response != "continue" {
                    row.set_active(false);
                }
            },
        );
    });
}
