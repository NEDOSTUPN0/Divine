//! One tile in the library grid: the cover, with a title strip under it.

use adw::prelude::*;
use gtk::gdk;

use crate::covers;
use crate::log::warn;
use crate::models::Game;
use crate::ui::spawn_bg;

/// Epic ships 1200x1600 portrait art, so the cover is a fixed 3:4 box.
pub const COVER_WIDTH: i32 = 160;
pub const COVER_HEIGHT: i32 = COVER_WIDTH * 4 / 3;
/// Room for two lines of title. Fixed, so every tile ends up the same height.
const TITLE_HEIGHT: i32 = 54;

pub fn build(game: &Game) -> gtk::Widget {
    // No expand flags anywhere in the tile: an expanding child propagates the
    // flag up the tree, and an expanding widget ignores its own alignment, which
    // is how the grid cell ended up stretching the cover.
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .build();

    let cover = gtk::Overlay::builder()
        .child(&picture)
        .height_request(COVER_HEIGHT)
        .build();

    // A game that cannot be played shows a grey copy of the cover with the
    // colour one on top of it, hidden. Hovering fades that top layer in.
    let missing = game.folder_missing();
    let playable = game.is_installed() && !missing;
    let colour = if playable {
        None
    } else {
        picture.add_css_class("cover-grey");
        let colour = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .opacity(0.0)
            .build();
        cover.add_overlay(&colour);
        Some(colour)
    };

    if game.cover.is_none() {
        let placeholder = gtk::Image::builder()
            .icon_name("applications-games-symbolic")
            .pixel_size(48)
            .valign(gtk::Align::Center)
            .build();
        placeholder.add_css_class("dim-label");
        cover.add_overlay(&placeholder);
    }
    if game.is_installed() {
        let (icon, tooltip) = if missing {
            (
                "dialog-warning-symbolic",
                "Installed, but its folder cannot be found",
            )
        } else {
            ("object-select-symbolic", "Installed")
        };
        let badge = gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(14)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .tooltip_text(tooltip)
            .build();
        badge.add_css_class("installed-badge");
        if missing {
            badge.add_css_class("missing");
        }
        cover.add_overlay(&badge);
    }

    let title = gtk::Label::builder()
        .label(&game.title)
        .wrap(true)
        .lines(2)
        .max_width_chars(1)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .justify(gtk::Justification::Center)
        .valign(gtk::Align::Center)
        .build();
    title.add_css_class("card-title");

    // Clamped vertically as well, so a two-line title cannot make one tile
    // taller than its neighbours.
    let title_bar = adw::Clamp::builder()
        .maximum_size(TITLE_HEIGHT)
        .orientation(gtk::Orientation::Vertical)
        .child(&title)
        .height_request(TITLE_HEIGHT)
        .build();
    title_bar.add_css_class("card-title-bar");

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(COVER_WIDTH)
        // Centred rather than filled, so the grid cell cannot stretch the tile.
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .hexpand(false)
        .vexpand(false)
        .build();
    card.append(&cover);
    card.append(&title_bar);
    // The theme's card, so a tile matches the cards on the game page rather
    // than being a second, slightly different one drawn by hand.
    card.add_css_class("card");
    card.add_css_class("game-card");
    if !playable {
        card.add_css_class("uninstalled");
    }
    card.set_overflow(gtk::Overflow::Hidden);
    card.set_tooltip_text(Some(&game.title));

    match &colour {
        Some(colour) => {
            fade_on_hover(&card, colour);
            load_covers(game, &[picture, colour.clone()]);
        }
        None => load_covers(game, &[picture]),
    }

    // size_request only sets a minimum, and both children ask for far more than
    // the tile is meant to be: Picture reports the texture size (covers are
    // fetched at 360x480), and a wrapping label reports the width that fits its
    // text in two lines. Clamp is the one widget that caps the maximum, and it
    // has to sit outside the whole tile to cap both at once.
    adw::Clamp::builder()
        .maximum_size(COVER_WIDTH)
        .orientation(gtk::Orientation::Horizontal)
        .child(&card)
        .build()
        .upcast()
}

/// Fades the colour layer in while the pointer is over the tile. Each step
/// starts from wherever the last one got to, so flicking across the grid does
/// not make the covers jump.
fn fade_on_hover(card: &gtk::Box, colour: &gtk::Picture) {
    let target = adw::PropertyAnimationTarget::new(colour, "opacity");
    let animation = adw::TimedAnimation::builder()
        .widget(card)
        .value_from(0.0)
        .value_to(0.0)
        .duration(220)
        .easing(adw::Easing::EaseOutCubic)
        .target(&target)
        .build();

    let motion = gtk::EventControllerMotion::new();
    let entering = animation.clone();
    motion.connect_enter(move |_, _, _| fade_to(&entering, 1.0));
    let leaving = animation.clone();
    motion.connect_leave(move |_| fade_to(&leaving, 0.0));
    card.add_controller(motion);
}

fn fade_to(animation: &adw::TimedAnimation, value: f64) {
    animation.set_value_from(animation.value());
    animation.set_value_to(value);
    animation.play();
}

/// Fills the picture from the disk cache, downloading the cover if needed.
pub fn load_cover(game: &Game, picture: &gtk::Picture) {
    load_covers(game, std::slice::from_ref(picture));
}

/// Same, for the tiles that show the cover twice: one grey, one in colour.
fn load_covers(game: &Game, pictures: &[gtk::Picture]) {
    let Some(cover) = game.cover.clone() else {
        return;
    };
    let pictures = pictures.to_vec();
    spawn_bg(
        move || covers::fetch(&cover),
        move |result| match result {
            Ok(path) => match gdk::Texture::from_filename(&path) {
                Ok(texture) => {
                    for picture in pictures {
                        picture.set_paintable(Some(&texture));
                    }
                }
                Err(e) => warn!("broken cover {}: {e:#}", path.display()),
            },
            Err(e) => warn!("cover download failed: {e:#}"),
        },
    );
}
