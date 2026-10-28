//! The part of the page Epic's store fills in: the pictures, the blurb, the
//! tags, the system requirements and the outbound links. All of it is optional
//! — a game whose store page Divine could not read simply shows less.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::log::warn;
use crate::models::Game;
use crate::ui::{accessible_label, spawn_bg, video, window::App};

use super::row;

/// The pieces of a game page that only the store can fill in, handed back so
/// the page can put each one where GNOME Software would: stars by the title,
/// pictures under it, everything else further down.
pub(super) struct StoreSection {
    pub(super) screenshots: gtk::Box,
    pub(super) details: gtk::Box,
}

/// How tall a star is, which is how much room the rating takes before it has
/// arrived.
const STAR_SIZE: i32 = 16;

/// Five stars that only appear once there is a rating to show.
pub(super) struct Stars {
    pub(super) row: gtk::Box,
    icons: Vec<gtk::Image>,
    value: gtk::Label,
}

pub(super) fn stars_row() -> Stars {
    // The row holds its height with nothing in it. It sits above the Play
    // button, and a rating that turned up a moment later would push the button
    // down as the reader was reaching for it; a game with no rating shows a
    // blank line nobody can see instead.
    let row = gtk::Box::builder()
        .spacing(3)
        .height_request(STAR_SIZE)
        .build();
    let icons: Vec<gtk::Image> = (0..5)
        .map(|_| {
            let star = gtk::Image::from_icon_name("non-starred-symbolic");
            star.add_css_class("dim-label");
            star.set_visible(false);
            row.append(&star);
            star
        })
        .collect();
    let value = gtk::Label::builder().margin_start(6).build();
    value.add_css_class("dim-label");
    row.append(&value);
    Stars { row, icons, value }
}

fn fill_stars(stars: &Stars, rating: f64) {
    if rating <= 0.0 {
        return;
    }
    for (index, star) in stars.icons.iter().enumerate() {
        // Half a star up is still that star: 4.6 lights four and a bit, which
        // reads as five only from 4.5 on.
        let filled = rating >= index as f64 + 0.5;
        star.set_icon_name(Some(if filled {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        }));
        star.set_visible(true);
    }
    stars.value.set_label(&format!("{rating:.1}"));
}

/// Everything the store page has to say.
///
/// The answer is on disk for any game whose page has been opened before, and
/// reading it here rather than in the background is what lets the page be drawn
/// finished: nothing appears a moment later and pushes the rest down. Only a
/// game seen for the first time goes to the network, and its pictures land in
/// space already held for them.
pub(super) fn store_section(
    app: &Rc<App>,
    game: &Game,
    stars: Stars,
    age: &gtk::Label,
) -> StoreSection {
    let shots = shots_block();

    let details = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();

    // The catalogue text is the one thing already in hand, so the page is not
    // blank while the store is asked.
    let catalogue = if game.description == game.title {
        String::new()
    } else {
        game.description.clone()
    };
    let summary = gtk::Label::builder()
        .label(&catalogue)
        .wrap(true)
        .justify(gtk::Justification::Left)
        .xalign(0.0)
        .selectable(true)
        .build();
    let full = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Left)
        .xalign(0.0)
        .selectable(true)
        .margin_top(6)
        .build();
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&full)
        .build();
    let more = gtk::Button::builder()
        .label("Show More")
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    more.add_css_class("flat");
    let target = revealer.clone();
    more.connect_clicked(move |button| {
        let showing = !target.reveals_child();
        target.set_reveal_child(showing);
        button.set_label(if showing { "Show Less" } else { "Show More" });
    });

    let about = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(!catalogue.is_empty())
        .build();
    about.append(&summary);
    about.append(&revealer);
    about.append(&more);

    let tags = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(6)
        .row_spacing(6)
        .max_children_per_line(8)
        .visible(false)
        .build();

    let requirements = requirements_section();

    let links = adw::PreferencesGroup::builder()
        .title("Links")
        .visible(false)
        .build();

    details.append(&about);
    details.append(&tags);
    details.append(&requirements.group);
    details.append(&requirements.languages);
    details.append(&links);

    let blocks = Blocks {
        stars,
        shots: shots.clone(),
        about,
        summary,
        full,
        more,
        tags,
        requirements,
        links,
        age: age.clone(),
    };

    let section = StoreSection {
        screenshots: shots.block.clone(),
        details,
    };

    if let Some(store) = crate::epic::store::cached(&game.namespace) {
        blocks.fill(app, &store);
        return section;
    }

    let namespace = game.namespace.clone();
    let title = game.title.clone();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || crate::epic::store::about(&namespace, &title),
        move |result| {
            let (Ok(store), Some(app)) = (result, weak.upgrade()) else {
                // Nothing to show and nothing coming: the space held for the
                // pictures is given back rather than left as an empty card.
                blocks.shots.block.set_visible(false);
                return;
            };
            blocks.fill(&app, &store);
        },
    );

    section
}

/// Every part of the page the store fills in, kept together so the one that
/// arrives from disk and the one that arrives from the network are filled by
/// the same code.
struct Blocks {
    stars: Stars,
    shots: Shots,
    about: gtk::Box,
    summary: gtk::Label,
    full: gtk::Label,
    more: gtk::Button,
    tags: gtk::FlowBox,
    requirements: Requirements,
    links: adw::PreferencesGroup,
    /// The age rating shows in the band of facts the page builds itself, so
    /// this is the one block that is handed in rather than made here.
    age: gtk::Label,
}

impl Blocks {
    fn fill(self, app: &Rc<App>, store: &crate::epic::store::About) {
        fill_stars(&self.stars, store.rating);
        fill_media(app, &self.shots, store);
        fill_about(&self.about, &self.summary, &self.full, &self.more, store);
        fill_tags(&self.tags, &store.tags);
        fill_requirements(&self.requirements, store);
        fill_links(app, &self.links, &store.links);
        fill_age(&self.age, store);
    }
}

/// How tall the pictures are, and therefore how much room is held for them
/// while the store is still being asked.
const SHOTS_HEIGHT: i32 = 380;

/// The pictures, or the space they are going to take.
///
/// A carousel that appears once its contents arrive shoves the whole page down
/// in front of the reader, so the block is there from the start at its final
/// height, showing a placeholder card. Swapping the pictures in changes
/// nothing about the layout; only a game that turns out to have no pictures at
/// all gives the space back, and that is the one case the height is not known
/// in advance.
#[derive(Clone)]
struct Shots {
    block: gtk::Box,
    stack: gtk::Stack,
    carousel: adw::Carousel,
}

fn shots_block() -> Shots {
    let carousel = adw::Carousel::builder()
        .height_request(SHOTS_HEIGHT)
        .build();
    let dots = adw::CarouselIndicatorDots::builder()
        .carousel(&carousel)
        .build();

    let placeholder = gtk::Box::builder().height_request(SHOTS_HEIGHT).build();
    placeholder.add_css_class("card");
    placeholder.add_css_class("shots-placeholder");

    // Crossfade, so the pictures replace the placeholder rather than blink into
    // place. The dots sit outside the stack: they are part of the reserved
    // height, and a page that gained a row of them later would move again.
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&placeholder, Some("waiting"));
    stack.add_named(&carousel_with_arrows(&carousel), Some("shots"));

    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    block.append(&stack);
    block.append(&dots);

    Shots {
        block,
        stack,
        carousel,
    }
}

/// Screenshots with a button on either side, the way GNOME Software does it:
/// the carousel takes swipes and scrolls by itself, but a mouse wants
/// something to click.
fn carousel_with_arrows(carousel: &adw::Carousel) -> gtk::Overlay {
    let overlay = gtk::Overlay::builder().child(carousel).build();
    let back = arrow_button(
        "go-previous-symbolic",
        gtk::Align::Start,
        "Previous Screenshot",
    );
    let forward = arrow_button("go-next-symbolic", gtk::Align::End, "Next Screenshot");
    overlay.add_overlay(&back);
    overlay.add_overlay(&forward);

    for (button, step) in [(&back, -1.0), (&forward, 1.0)] {
        let carousel = carousel.clone();
        button.connect_clicked(move |_| {
            let last = carousel.n_pages().saturating_sub(1) as f64;
            let target = (carousel.position().round() + step).clamp(0.0, last);
            carousel.scroll_to(&carousel.nth_page(target as u32), true);
        });
    }

    // Nothing to go back to on the first picture, nothing forward on the last.
    let (start, end) = (back.clone(), forward.clone());
    let update = move |carousel: &adw::Carousel| {
        let pages = carousel.n_pages();
        let position = carousel.position();
        start.set_visible(pages > 1 && position > 0.01);
        end.set_visible(pages > 1 && position < pages.saturating_sub(1) as f64 - 0.01);
    };
    carousel.connect_position_notify(update.clone());
    carousel.connect_n_pages_notify(update.clone());
    update(carousel);
    overlay
}

fn arrow_button(icon: &str, align: gtk::Align, name: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .halign(align)
        .valign(gtk::Align::Center)
        .margin_start(9)
        .margin_end(9)
        .tooltip_text(name)
        .visible(false)
        .build();
    button.add_css_class("circular");
    button.add_css_class("osd");
    accessible_label(&button, name);
    button
}

/// The age rating tile, which stays out of the band until the store answers.
fn fill_age(label: &gtk::Label, store: &crate::epic::store::About) {
    if store.age_rating.is_empty() {
        return;
    }
    label.set_label(&store.age_rating);
    if !store.age_descriptors.is_empty() {
        label.set_tooltip_text(Some(&store.age_descriptors.join(", ")));
    }
    if let Some(tile) = label.parent() {
        tile.set_visible(true);
    }
}

/// Fills the carousel with what the store has, trailers first — which is the
/// order the store page itself uses, and the one a game is shown in.
fn fill_media(app: &Rc<App>, shots: &Shots, store: &crate::epic::store::About) {
    let playable = video::available();
    let trailers: Vec<_> = match playable {
        true => store.videos.iter().collect(),
        // Nothing here can play a trailer, so the page keeps to its pictures
        // rather than showing a poster that does nothing.
        false => Vec::new(),
    };

    if trailers.is_empty() && store.screenshots.is_empty() {
        shots.block.set_visible(false);
        return;
    }
    shots.stack.set_visible_child_name("shots");

    for clip in trailers {
        shots.carousel.append(&trailer_page(app, shots, clip));
    }
    for url in &store.screenshots {
        shots.carousel.append(&still(app, url));
    }
}

/// One picture in the carousel, fetched and cached like any other.
fn still(app: &Rc<App>, url: &str) -> gtk::Picture {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(240)
        .build();
    picture.add_css_class("card");

    let url = url.to_string();
    let weak = Rc::downgrade(app);
    let target = picture.clone();
    spawn_bg(
        move || crate::covers::fetch_screenshot(&url),
        move |result| {
            if let (Ok(path), Some(_)) = (result, weak.upgrade()) {
                target.set_filename(Some(&path));
            }
        },
    );
    picture
}

/// A trailer in the carousel: its opening frame, with a button over it and a
/// bar of controls along the bottom.
///
/// The pipeline is built on the first press rather than with the page — a page
/// with several trailers would otherwise open several connections to Epic for
/// videos nobody asked to watch — and it is taken down again the moment the
/// page is left. A paused pipeline still holds a decoder and its share of video
/// memory, which is not something to leave behind on the way out.
fn trailer_page(app: &Rc<App>, shots: &Shots, clip: &crate::epic::store::Clip) -> gtk::Overlay {
    let poster = still(app, &clip.poster);
    let page = gtk::Overlay::builder().child(&poster).build();

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .height_request(240)
        .visible(false)
        .build();
    page.add_overlay(&picture);

    let big = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .tooltip_text("Play the Trailer")
        .build();
    accessible_label(&big, "Play the Trailer");
    big.add_css_class("circular");
    big.add_css_class("osd");
    big.add_css_class("trailer-play");
    page.add_overlay(&big);

    let bar = controls(app);
    page.add_overlay(&bar.row);

    let parts = Trailer {
        page: page.downgrade(),
        picture: picture.downgrade(),
        big: big.downgrade(),
        bar: bar.row.downgrade(),
        play: bar.play.downgrade(),
        at: bar.at.downgrade(),
        elapsed: bar.elapsed.downgrade(),
        scrubbing: bar.scrubbing.clone(),
        player: Rc::new(RefCell::new(None)),
    };
    parts.wire(app, &bar, &clip.url);

    let weak = Rc::downgrade(app);
    let url = clip.url.clone();
    let held = parts.clone();
    big.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            held.start(&app, &url);
        }
    });

    // Scrolling on to the next picture stops the sound: a trailer nobody is
    // looking at should not still be playing.
    let held = parts.clone();
    shots.carousel.connect_position_notify(move |carousel| {
        let Some(page) = held.page.upgrade() else {
            return;
        };
        let at = carousel.position().round().max(0.0) as u32;
        if carousel.n_pages() > at && carousel.nth_page(at) == page.clone().upcast::<gtk::Widget>()
        {
            return;
        }
        held.pause();
    });

    // Minimising the window is not leaving the page, so this only pauses;
    // leaving it for good goes through the pipeline being taken down below.
    let held = parts.clone();
    page.connect_unmap(move |_| held.pause());

    // Leaving the game page takes the pipeline down, decoder and video memory
    // with it. The signal for that belongs to the page in the navigation view,
    // which is only found once this widget is in the window — and asked for
    // once, since the carousel maps its pages more than once.
    let held = parts.clone();
    let hooked = Cell::new(false);
    page.connect_map(move |page| {
        if hooked.replace(true) {
            return;
        }
        let Some(showing) = page
            .ancestor(adw::NavigationPage::static_type())
            .and_downcast::<adw::NavigationPage>()
        else {
            return;
        };
        let held = held.clone();
        showing.connect_hidden(move |_| held.close());
    });

    page
}

/// One trailer in a page, held the only way a signal handler safely can.
///
/// Every closure below is hung off a widget inside the page it would otherwise
/// point back at, and a strong reference either way round is a ring that keeps
/// the whole page — pipeline included — alive for as long as Divine runs.
#[derive(Clone)]
struct Trailer {
    page: glib::WeakRef<gtk::Overlay>,
    picture: glib::WeakRef<gtk::Picture>,
    /// The button in the middle of the poster, which is only there when
    /// nothing is playing.
    big: glib::WeakRef<gtk::Button>,
    bar: glib::WeakRef<gtk::Box>,
    play: glib::WeakRef<gtk::Button>,
    at: glib::WeakRef<gtk::Scale>,
    elapsed: glib::WeakRef<gtk::Label>,
    /// Set while the slider is being moved by the clock rather than by hand, so
    /// its own change signal is not read as a seek.
    scrubbing: Rc<Cell<bool>>,
    player: Rc<RefCell<Option<video::Player>>>,
}

impl Trailer {
    /// Hangs the bar's own buttons off this trailer. Kept apart from building
    /// the bar so that [`controls`] stays a widget builder and nothing else.
    fn wire(&self, app: &Rc<App>, bar: &Controls, url: &str) {
        let held = self.clone();
        let weak = Rc::downgrade(app);
        let url = url.to_string();
        bar.play.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match held.playing() {
                true => held.pause(),
                false => held.start(&app, &url),
            }
        });

        let held = self.clone();
        bar.at.connect_value_changed(move |at| {
            if held.scrubbing.get() {
                return;
            }
            if let Some(player) = held.player.borrow().as_ref() {
                player.seek_to(gst::ClockTime::from_seconds(at.value() as u64));
            }
        });

        let held = self.clone();
        let weak = Rc::downgrade(app);
        let sound = bar.mute.downgrade();
        bar.mute.connect_clicked(move |_| {
            let (Some(app), Some(button)) = (weak.upgrade(), sound.upgrade()) else {
                return;
            };
            let muted = !app.cfg.borrow().trailers.muted;
            if let Some(player) = held.player.borrow().as_ref() {
                player.set_muted(muted);
            }
            show_sound(&button, muted);
            app.cfg.borrow_mut().trailers.muted = muted;
            save_sound(&app);
        });

        let held = self.clone();
        let weak = Rc::downgrade(app);
        bar.volume.connect_value_changed(move |volume| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let wanted = volume.value();
            if let Some(player) = held.player.borrow().as_ref() {
                player.set_volume(wanted);
            }
            app.cfg.borrow_mut().trailers.volume = wanted;
            save_sound(&app);
        });
    }

    fn playing(&self) -> bool {
        self.player
            .borrow()
            .as_ref()
            .is_some_and(|player| player.is_playing())
    }

    /// Plays from wherever it stopped, building the pipeline if this is the
    /// first press or the first since the page was left.
    fn start(&self, app: &Rc<App>, url: &str) {
        if let Some(player) = self.player.borrow().as_ref() {
            player.play();
            self.showing(true);
            return;
        }

        let started = match video::Player::new(url) {
            Ok(started) => started,
            Err(e) => {
                app.toast_error("Cannot play the trailer", &e);
                return;
            }
        };
        let sound = app.cfg.borrow().trailers.clone();
        started.set_volume(sound.volume);
        started.set_muted(sound.muted);
        if let Some(picture) = self.picture.upgrade() {
            picture.set_paintable(Some(started.paintable()));
            picture.set_visible(true);
        }

        // Back to the poster when it runs out, so the button starts it again
        // rather than doing nothing.
        let held = self.clone();
        started.watch_end(move || {
            if let Some(player) = held.player.borrow().as_ref() {
                player.rewind();
            }
            held.showing(false);
        });

        started.play();
        *self.player.borrow_mut() = Some(started);
        self.showing(true);
        self.follow();
    }

    fn pause(&self) {
        if let Some(player) = self.player.borrow().as_ref() {
            player.pause();
            self.showing(false);
        }
    }

    /// The page is gone: down goes the pipeline, and the poster comes back for
    /// whenever the page is opened again.
    fn close(&self) {
        self.player.borrow_mut().take();
        if let Some(picture) = self.picture.upgrade() {
            picture.set_paintable(gdk::Paintable::NONE);
            picture.set_visible(false);
        }
        self.showing(false);
        if let Some(bar) = self.bar.upgrade() {
            bar.set_visible(false);
        }
        self.scrubbing.set(true);
        if let Some(at) = self.at.upgrade() {
            at.set_value(0.0);
        }
        self.scrubbing.set(false);
        if let Some(elapsed) = self.elapsed.upgrade() {
            elapsed.set_label("");
        }
    }

    /// Shows what state the trailer is in: the big button belongs to a stopped
    /// trailer, the bar to one that has been started at least once.
    fn showing(&self, playing: bool) {
        if let Some(big) = self.big.upgrade() {
            big.set_visible(!playing);
        }
        if let Some(bar) = self.bar.upgrade() {
            bar.set_visible(self.player.borrow().is_some());
        }
        if let Some(play) = self.play.upgrade() {
            let (icon, name) = match playing {
                true => ("media-playback-pause-symbolic", "Pause"),
                false => ("media-playback-start-symbolic", "Play"),
            };
            play.set_icon_name(icon);
            play.set_tooltip_text(Some(name));
            accessible_label(&play, name);
        }
    }

    /// Keeps the slider and the clock beside it on the trailer, four times a
    /// second, for as long as there is a trailer to follow.
    fn follow(&self) {
        let held = self.clone();
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let borrowed = held.player.borrow();
            let Some(player) = borrowed.as_ref() else {
                return glib::ControlFlow::Break;
            };
            let (Some(at), Some(whole)) = (player.position(), player.duration()) else {
                return glib::ControlFlow::Continue;
            };
            let (Some(slider), Some(elapsed)) = (held.at.upgrade(), held.elapsed.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            let (at, whole) = (at.seconds() as f64, whole.seconds() as f64);
            // The slider moves under the clock here, which is the one time its
            // change signal must not be taken for a seek. Setting the range
            // counts: it clamps the value, and that is a change like any other.
            held.scrubbing.set(true);
            slider.set_range(0.0, whole.max(1.0));
            slider.set_value(at);
            held.scrubbing.set(false);
            elapsed.set_label(&format!("{} / {}", clock(at), clock(whole)));
            glib::ControlFlow::Continue
        });
    }
}

/// The bar along the bottom of a trailer, before anything is hung off it.
struct Controls {
    row: gtk::Box,
    play: gtk::Button,
    at: gtk::Scale,
    elapsed: gtk::Label,
    mute: gtk::Button,
    volume: gtk::Scale,
    scrubbing: Rc<Cell<bool>>,
}

fn controls(app: &Rc<App>) -> Controls {
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .build();
    play.add_css_class("flat");
    accessible_label(&play, "Play");

    let at = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .draw_value(false)
        .hexpand(true)
        .build();
    at.set_range(0.0, 1.0);
    accessible_label(&at, "Position");

    let elapsed = gtk::Label::new(None);
    elapsed.add_css_class("numeric");
    elapsed.add_css_class("caption");

    let sound = app.cfg.borrow().trailers.clone();
    let mute = gtk::Button::builder().build();
    mute.add_css_class("flat");
    show_sound(&mute, sound.muted);

    let volume = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .draw_value(false)
        .width_request(90)
        .tooltip_text("Volume")
        .build();
    volume.set_range(0.0, 1.0);
    volume.set_value(sound.volume);
    accessible_label(&volume, "Volume");

    let row = gtk::Box::builder()
        .spacing(6)
        .valign(gtk::Align::End)
        .margin_start(9)
        .margin_end(9)
        .margin_bottom(9)
        .visible(false)
        .build();
    row.add_css_class("osd");
    row.add_css_class("toolbar");
    row.add_css_class("trailer-bar");
    for widget in [
        play.upcast_ref::<gtk::Widget>(),
        at.upcast_ref(),
        elapsed.upcast_ref(),
        mute.upcast_ref(),
        volume.upcast_ref(),
    ] {
        row.append(widget);
    }

    Controls {
        row,
        play,
        at,
        elapsed,
        mute,
        volume,
        scrubbing: Rc::new(Cell::new(false)),
    }
}

fn show_sound(button: &gtk::Button, muted: bool) {
    button.set_icon_name(match muted {
        true => "audio-volume-muted-symbolic",
        false => "audio-volume-high-symbolic",
    });
    let name = match muted {
        true => "Unmute",
        false => "Mute",
    };
    button.set_tooltip_text(Some(name));
    accessible_label(button, name);
}

/// Writes the sound settings down, but not on every step of a drag: the slider
/// fires as fast as the mouse moves and the file would be rewritten each time.
fn save_sound(app: &Rc<App>) {
    thread_local! {
        static PENDING: Cell<bool> = const { Cell::new(false) };
    }
    if PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    let weak = Rc::downgrade(app);
    glib::timeout_add_local_once(Duration::from_millis(700), move || {
        PENDING.with(|pending| pending.set(false));
        let Some(app) = weak.upgrade() else {
            return;
        };
        let saved = app.cfg.borrow().save();
        if let Err(e) = saved {
            warn!("cannot save the sound settings: {e:#}");
        }
    });
}

/// Seconds as a trailer's length reads: `1:07`.
fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn fill_about(
    about: &gtk::Box,
    summary: &gtk::Label,
    full: &gtk::Label,
    more: &gtk::Button,
    store: &crate::epic::store::About,
) {
    if store.short.trim().is_empty() && store.long.trim().is_empty() {
        return;
    }
    let long = store_markup(&store.long);
    let short = store.short.trim();
    if short.is_empty() {
        // Nothing to summarise with, so the first paragraph stands in.
        let (head, tail) = split_paragraph(&long);
        summary.set_markup(&head);
        full.set_markup(&tail);
        more.set_visible(!tail.is_empty());
    } else {
        summary.set_label(short);
        full.set_markup(&long);
        more.set_visible(!long.is_empty());
    }
    about.set_visible(true);
}

fn fill_tags(tags: &gtk::FlowBox, names: &[String]) {
    if names.is_empty() {
        return;
    }
    for name in names {
        let chip = gtk::Label::new(Some(name));
        chip.add_css_class("tag-chip");
        tags.append(&chip);
    }
    tags.set_visible(true);
}

/// The requirements list and the languages that sit under it.
#[derive(Clone)]
struct Requirements {
    group: adw::PreferencesGroup,
    languages: adw::PreferencesGroup,
}

fn requirements_section() -> Requirements {
    let group = adw::PreferencesGroup::builder()
        .title("System Requirements")
        .visible(false)
        .build();
    let languages = adw::PreferencesGroup::builder()
        .title("Languages")
        .visible(false)
        .build();

    Requirements { group, languages }
}

/// The captions naming the two value columns.
///
/// They live in the group header, which sits outside the card while the values
/// sit inside it, so the margin is what lines them up with the columns below.
fn requirement_captions(least: &gtk::SizeGroup, most: &gtk::SizeGroup) -> gtk::Box {
    let line = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_end(12)
        .build();
    for (text, group) in [("Minimum", least), ("Recommended", most)] {
        let caption = gtk::Label::builder().label(text).xalign(0.0).build();
        caption.add_css_class("dim-label");
        caption.add_css_class("caption");
        group.add_widget(&caption);
        line.append(&caption);
    }
    line
}

fn requirement_row(
    item: &crate::epic::store::Requirement,
    both: bool,
    least: &gtk::SizeGroup,
    most: &gtk::SizeGroup,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&item.title)
        .use_markup(false)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(requirement_icon(&item.title)));

    let values = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let minimum = requirement_value(&item.minimum);
    least.add_widget(&minimum);
    values.append(&minimum);

    if both {
        // A part the game says nothing extra about repeats the minimum. An
        // empty cell under a caption reads as "unknown", which it is not.
        let asked = match item.recommended.is_empty() {
            true => &item.minimum,
            false => &item.recommended,
        };
        let recommended = requirement_value(asked);
        most.add_widget(&recommended);
        values.append(&recommended);
    }
    row.add_suffix(&values);
    row
}

/// Capped rather than free: a size group takes the widest member, and one long
/// processor string would otherwise set the column width for the whole table.
fn requirement_value(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(24)
        .build();
    label.add_css_class("dim-label");
    label
}

fn fill_requirements(into: &Requirements, store: &crate::epic::store::About) {
    if !store.requirements.is_empty() {
        let both = store
            .requirements
            .iter()
            .any(|item| !item.recommended.is_empty() && item.recommended != item.minimum);
        // One size group per column, so every row starts its values at the same
        // place and the captions above them line up with what they name.
        let least = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let most = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        if both {
            into.group
                .set_header_suffix(Some(&requirement_captions(&least, &most)));
        }
        for item in &store.requirements {
            into.group.add(&requirement_row(item, both, &least, &most));
        }
        into.group.set_visible(true);
    }

    let sections = crate::epic::store::language_sections(&store.languages);
    for (title, listed) in &sections {
        let entry = row(title, listed);
        entry.add_prefix(&gtk::Image::from_icon_name(
            "preferences-desktop-locale-symbolic",
        ));
        into.languages.add(&entry);
    }
    into.languages.set_visible(!sections.is_empty());
}

/// Adwaita has no icon for a CPU or a stick of RAM, so the nearest thing that
/// exists in the theme stands in; a missing name would draw the broken-image
/// icon instead.
fn requirement_icon(title: &str) -> &'static str {
    let title = title.to_lowercase();
    if title.contains("os") || title.contains("system") {
        "computer-symbolic"
    } else if title.contains("processor") || title.contains("cpu") {
        "applications-system-symbolic"
    } else if title.contains("memory") || title.contains("ram") {
        "media-flash-symbolic"
    } else if title.contains("storage") || title.contains("space") || title.contains("disk") {
        "drive-harddisk-symbolic"
    } else if title.contains("graphics") || title.contains("video") || title.contains("gpu") {
        "video-display-symbolic"
    } else if title.contains("directx") || title.contains("api") {
        "applications-graphics-symbolic"
    } else if title.contains("sound") || title.contains("audio") {
        "audio-card-symbolic"
    } else if title.contains("network") || title.contains("internet") {
        "network-wired-symbolic"
    } else if title.contains("audio:") || title.contains("text") {
        "preferences-desktop-locale-symbolic"
    } else {
        "dialog-information-symbolic"
    }
}

fn fill_links(app: &Rc<App>, links: &adw::PreferencesGroup, entries: &[crate::epic::store::Link]) {
    if entries.is_empty() {
        return;
    }
    for entry in entries {
        let row = adw::ActionRow::builder()
            .title(&entry.title)
            .subtitle(&entry.url)
            .activatable(true)
            .use_markup(false)
            .build();

        let url = entry.url.clone();
        let weak = Rc::downgrade(app);
        row.connect_activated(move |_| {
            if let Some(app) = weak.upgrade() {
                gtk::UriLauncher::new(&url).launch(
                    Some(&app.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
        links.add(&row);
    }
    links.set_visible(true);
}

/// Splits marked up text after its first paragraph, for when the store has no
/// summary of its own.
fn split_paragraph(text: &str) -> (String, String) {
    match text.split_once("\n\n") {
        Some((head, tail)) => (head.to_string(), tail.to_string()),
        None => (text.to_string(), String::new()),
    }
}

/// The store writes its long description in Markdown. This is not a Markdown
/// renderer: it lifts headings into bold, marks list items, and leaves the
/// rest as the plain text it already is.
fn store_markup(text: &str) -> String {
    let mut out = String::new();
    let mut blanks = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blanks > 0 {
                out.push('\n');
            }
        }
        blanks = 0;

        if let Some(heading) = line.trim_start().strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            out.push_str(&format!("<b>{}</b>", inline_markup(heading)));
        } else if let Some(item) = list_item(line.trim_start()) {
            out.push_str(&format!("• {}", inline_markup(item)));
        } else {
            out.push_str(&inline_markup(line));
        }
    }
    out
}

fn list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim_start)
}

/// Bold spans survive as bold; every other marker is dropped rather than shown.
fn inline_markup(text: &str) -> String {
    let escaped = glib::markup_escape_text(text).to_string();
    let mut out = String::new();
    let mut bold = false;
    let mut rest = escaped.as_str();
    while let Some(at) = rest.find("**") {
        out.push_str(&rest[..at]);
        out.push_str(if bold { "</b>" } else { "<b>" });
        bold = !bold;
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    if bold {
        out.push_str("</b>");
    }
    out
}
