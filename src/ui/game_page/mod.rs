//! The page for a single game: art, blurb and the Play button. Everything that
//! is configuration or reference material lives behind the header bar menu.
//!
//! This file assembles the page and owns the small things every part of it
//! shares; each block it puts together lives in the module named after it.

mod achievements;
mod collections;
mod install;
mod launch;
mod maintenance;
mod settings;
mod store;

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::models::{human_size, Game};
use crate::ui::{game_card, spawn_bg, window::App};

use self::achievements::achievements_block;
use self::install::{components_dialog, dlc_dialog, tags_size};
pub use self::launch::restore_running;
use self::launch::{action_button, force_stop};
use self::maintenance::{confirm_uninstall, move_dialog, sync_saves_now, verify_files};
use self::settings::{details_group, settings_dialog};
use self::store::{stars_row, store_section};

/// Epic's portrait art is 3:4, and the page holds the cover to this width
/// whatever the texture behind it happens to be.
const COVER_WIDTH: i32 = 150;

/// What a tile shows while it is waiting for an answer, and what it shows when
/// the answer turns out to be that nobody knows. Every tile is built at its
/// final place and stays there: a card that turns up halfway through reading
/// the page pushes the rest of the band aside.
const WAITING: &str = "\u{2026}";
const UNKNOWN: &str = "Unknown";

pub fn page(app: &Rc<App>, index: usize) -> Option<adw::NavigationPage> {
    let game = app.games.borrow().get(index)?.clone();

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .width_request(COVER_WIDTH)
        .height_request(COVER_WIDTH * 4 / 3)
        .valign(gtk::Align::Start)
        .build();
    picture.add_css_class("hero-cover");
    picture.set_overflow(gtk::Overflow::Hidden);
    game_card::load_cover(&game, &picture);

    // A size request is only a minimum, and a Picture asks for the whole
    // texture — 360 px of it, until the screenshots below arrive and take the
    // width back. That is the cover visibly shrinking while the page loads, so
    // it is capped by the one widget that caps a maximum.
    let cover = adw::Clamp::builder()
        .maximum_size(COVER_WIDTH)
        .orientation(gtk::Orientation::Horizontal)
        .valign(gtk::Align::Start)
        .child(&picture)
        .build();

    let title = gtk::Label::builder()
        .label(&game.title)
        .wrap(true)
        .xalign(0.0)
        .justify(gtk::Justification::Left)
        .build();
    title.add_css_class("title-1");

    let byline = byline(app, &game);

    let stars = stars_row();

    let heading = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    heading.append(&title);
    heading.append(&byline);
    heading.append(&stars.row);
    let actions = action_button(app, &game);
    actions.set_margin_top(6);
    heading.append(&actions);

    let header_block = gtk::Box::builder().spacing(18).build();
    header_block.append(&cover);
    header_block.append(&heading);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .build();
    content.append(&header_block);

    // Made here because the band of facts holds it and the store fills it in,
    // and the store answers first when its page has been read before.
    let age = value_label("");
    let store = store_section(app, &game, stars, &age);
    content.append(&store.screenshots);
    content.append(&info_tiles(app, &game, &age));
    content.append(&achievements_block(app, &game));
    content.append(&store.details);
    content.append(&details_group(app, &game));

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(
            &adw::Clamp::builder()
                .maximum_size(760)
                .child(&content)
                .build(),
        )
        .build();

    let header = adw::HeaderBar::new();
    header.pack_end(&collections::favorite_button(app, &game));
    let menu = gio::Menu::new();
    menu.append(Some("Collections…"), Some("game.collections"));
    menu.append(Some("Game Settings"), Some("game.settings"));
    if game.is_installed() {
        if !game.dlcs.is_empty() {
            menu.append(Some("Add-ons…"), Some("game.dlcs"));
        }
        if crate::sdl::has_components(&crate::config::legendary_config_dir(), &game.app_name) {
            menu.append(Some("Components…"), Some("game.components"));
        }
        menu.append(Some("Verify and Repair"), Some("game.verify"));
        menu.append(Some("Move…"), Some("game.move"));
        if game.cloud_saves {
            menu.append(Some("Sync Saves"), Some("game.sync-saves"));
        }
        menu.append(Some("Uninstall…"), Some("game.uninstall"));
        menu.append(Some("Force Stop"), Some("game.force-stop"));
    }
    header.pack_end(
        &gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .tooltip_text("Game menu")
            .build(),
    );

    let view = adw::ToolbarView::builder().content(&scroller).build();
    view.add_top_bar(&header);
    view.insert_action_group("game", Some(&menu_actions(app, &game)));

    Some(
        adw::NavigationPage::builder()
            .title(&game.title)
            // Tagged so a finished download knows whether this page is the one
            // still showing its game.
            .tag(&game.app_name)
            .child(&view)
            .build(),
    )
}

/// Developer and release date as one line under the title.
///
/// The developer is a link into the library's search rather than plain text:
/// the studio is the one thing on this page that says something about other
/// games the user owns, and a search by name is the only way to reach them.
fn byline(app: &Rc<App>, game: &Game) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).use_markup(true).build();
    label.add_css_class("dim-label");

    let mut parts = Vec::new();
    if !game.developer.is_empty() {
        // The title shows as the link's tooltip, which is where GTK expects to
        // be told what following it does.
        parts.push(format!(
            "<a href=\"developer\" title=\"Find other games by this developer\">{}</a>",
            glib::markup_escape_text(&game.developer)
        ));
    }
    if !game.release_date.is_empty() {
        parts.push(glib::markup_escape_text(&game.release_date).to_string());
    }
    label.set_markup(&parts.join(" · "));

    let weak = Rc::downgrade(app);
    let developer = game.developer.clone();
    label.connect_activate_link(move |_, _| {
        if let Some(app) = weak.upgrade() {
            app.search_for(&developer);
        }
        // Nothing to open: the href is a marker, not an address.
        glib::Propagation::Stop
    });
    label
}

/// A row of small cards under the title, holding what is worth knowing before
/// pressing Play. GNOME Software lays the same kind of facts out this way, which
/// reads better than a stack of loose centred labels.
fn info_tiles(app: &Rc<App>, game: &Game, age: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::builder().build();
    row.add_css_class("card");
    row.add_css_class("info-band");

    let mut tiles = vec![played_tile(app, game), protondb_tile(app, game)];
    tiles.extend(size_tiles(app, game));

    // Whether there is an age rating at all is only known once the store has
    // answered, so the tile starts hidden and the store shows it.
    let tile = tile("Age rating", age);
    tile.set_visible(!age.label().is_empty());
    tiles.push(tile);

    for (index, tile) in tiles.iter().enumerate() {
        if index > 0 {
            // Homogeneous would hand the separators as much width as the
            // tiles; the tiles share the row between themselves instead. A
            // separator follows the tile behind it for as long as both live,
            // since several tiles only appear once an answer arrives, and a
            // game without an age rating would show a divider into nothing.
            let separator = gtk::Separator::new(gtk::Orientation::Vertical);
            tile.bind_property("visible", &separator, "visible")
                .sync_create()
                .build();
            row.append(&separator);
        }
        tile.set_hexpand(true);
        row.append(tile);
    }
    row
}

/// Time played, as Epic counts it plus anything Divine has recorded and not
/// handed over yet. Epic's half arrives over the network, so the tile opens
/// with what is on disk and grows once the answer lands.
fn played_tile(app: &Rc<App>, game: &Game) -> gtk::Box {
    let local = crate::epic::playtime::unreported()
        .get(&game.app_name)
        .copied()
        .unwrap_or_default();
    let (counted, unasked) = {
        let cache = app.playtime.borrow();
        (
            cache.get(&game.app_name).copied().unwrap_or_default(),
            cache.is_empty(),
        )
    };

    let label = value_label(&crate::epic::playtime::human(counted + local));
    let tile = tile("Played", &label);

    // One request per run of Divine: the answer covers the whole library.
    if unasked {
        let user_file = app.lgd.borrow().user_file();
        let app_name = game.app_name.clone();
        let weak = Rc::downgrade(app);
        let label = label.clone();
        spawn_bg(
            move || crate::epic::playtime::totals(&user_file),
            move |result| {
                let (Ok(totals), Some(app)) = (result, weak.upgrade()) else {
                    return;
                };
                let counted = totals.get(&app_name).copied().unwrap_or_default();
                *app.playtime.borrow_mut() = totals;
                label.set_label(&crate::epic::playtime::human(counted + local));
            },
        );
    }
    tile
}

/// One card: a caption above whatever the value happens to be.
fn tile(caption: &str, value: &impl IsA<gtk::Widget>) -> gtk::Box {
    let caption = gtk::Label::builder().label(caption).build();
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");

    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .build();
    tile.add_css_class("info-tile");
    tile.append(&caption);
    tile.append(value);
    tile
}

fn value_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(12)
        .build();
    label.add_css_class("heading");
    label
}

/// ProtonDB only knows Steam app ids, so the game has to be found in one of the
/// databases that map an Epic app name to one. Two lookups over the network,
/// which is why the tile is there from the start with nothing in it: appearing
/// later would shove the rest of the band sideways while it is being read.
fn protondb_tile(app: &Rc<App>, game: &Game) -> gtk::Box {
    // Only the dot carries the tier colour: coloured text on a card is a
    // contrast problem, a dot next to a normal label is not.
    let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
    dot.add_css_class("protondb-dot");
    let value = value_label(WAITING);

    let line = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    line.append(&dot);
    line.append(&value);

    let tile = tile("ProtonDB", &line);

    let app_name = game.app_name.clone();
    let pinned = app.cfg.borrow().game(&game.app_name).umu_id;
    let target = tile.clone();
    spawn_bg(
        move || crate::protondb::summary(steam_app_id(&app_name, &pinned)?),
        move |summary| {
            let Some(summary) = summary else {
                // Not on Steam, or nobody has reported on it. Said out loud
                // rather than left as an empty card.
                value.set_label(UNKNOWN);
                target.set_tooltip_text(Some("ProtonDB has nothing about this game"));
                return;
            };
            value.set_label(&summary.label());
            dot.add_css_class(&summary.css_class());
            target.set_tooltip_text(Some(&summary.tooltip()));
        },
    );
    tile
}

/// What the game costs: a download for one not installed yet, disk space and
/// build for one that is.
fn size_tiles(app: &Rc<App>, game: &Game) -> Vec<gtk::Box> {
    if let Some(installed) = &game.installed {
        return vec![
            tile("Size", &value_label(&human_size(installed.install_size))),
            tile("Version", &value_label(&installed.version)),
        ];
    }

    let download = value_label(WAITING);
    let disk = value_label(WAITING);
    let tiles = vec![tile("Download", &download), tile("On disk", &disk)];

    let lgd = app.lgd.borrow().clone();
    let app_name = game.app_name.clone();
    let version = game.available_version.clone();
    let legendary_config = crate::config::legendary_config_dir();
    let hide = tiles.clone();
    let marked = tiles.clone();
    spawn_bg(
        move || {
            let manifest = lgd.manifest(&app_name, &version);
            let selective = crate::sdl::selective(&legendary_config, &app_name).unwrap_or_default();
            (manifest, selective)
        },
        move |(manifest, selective)| {
            let Ok(manifest) = manifest else {
                // No manifest, no numbers. The tiles stay where they are and
                // say so: taking them away now would shuffle the whole band.
                download.set_label(UNKNOWN);
                disk.set_label(UNKNOWN);
                for tile in hide {
                    tile.set_tooltip_text(Some("Epic did not hand over the file list"));
                }
                return;
            };
            // A game shipped in parts costs what its default set costs, not
            // what every optional component would add up to.
            let (down, on_disk) = match &selective {
                Some(selective) => {
                    let mut tags = vec![String::new()];
                    tags.extend(selective.required.iter().cloned());
                    (
                        tags_size(&tags, &manifest.tag_download),
                        tags_size(&tags, &manifest.tag_disk),
                    )
                }
                None => (manifest.download_size, manifest.disk_size),
            };
            if selective.is_some() {
                for tile in &marked {
                    tile.set_tooltip_text(Some(
                        "Without the optional components, which are picked when installing",
                    ));
                }
            }
            download.set_label(&human_size(down));
            disk.set_label(&human_size(on_disk));
        },
    );
    tiles
}

/// A umu id pinned by hand wins, since the user chose it deliberately. Failing
/// that, GOG's database knows nearly every game, and umu's only the ones that
/// need a protonfix, so it is the fallback rather than the first stop.
fn steam_app_id(app_name: &str, pinned: &str) -> Option<u32> {
    if !pinned.is_empty() {
        return crate::umu::steam_app_id(pinned);
    }
    crate::gamesdb::steam_app_id(app_name)
        .or_else(|| crate::umu::steam_app_id(&crate::umu::game_id(app_name)?))
}

fn menu_actions(app: &Rc<App>, game: &Game) -> gio::SimpleActionGroup {
    let actions = gio::SimpleActionGroup::new();
    for (name, handler) in [
        ("collections", collections::dialog as fn(&Rc<App>, &Game)),
        ("settings", settings_dialog),
        ("force-stop", force_stop),
        ("dlcs", dlc_dialog),
        ("components", components_dialog),
        ("verify", verify_files),
        ("move", move_dialog),
        ("sync-saves", sync_saves_now),
        ("uninstall", confirm_uninstall),
    ] {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(app);
        let game = game.clone();
        action.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                handler(&app, &game);
            }
        });
        actions.add_action(&action);
    }
    actions
}

/// Puts the cost of the current choice in the dialog's body.
/// Below this much left over, the disk gets a warning rather than a plain
/// number. A game is never the only thing that needs room: the prefix, the
/// shader cache and the next update all land on the same disk.
pub(super) const ELBOW_ROOM: u64 = 10 * 1024 * 1024 * 1024;

/// A fact and its value, as a row.
///
/// Markup is turned off, here and on every other row built from something
/// Divine did not write itself. AdwPreferencesRow reads its title and subtitle
/// as Pango markup, and a value that fails to parse as markup does not merely
/// lose its punctuation — the row comes out blank. An install path with a `<`
/// in it, or a game called "Mount & Blade II", is enough.
pub(super) fn row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_selectable(true)
        .use_markup(false)
        .css_classes(["property"])
        .build()
}
