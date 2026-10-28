//! Epic's achievements on the page: a card showing the latest one earned and
//! how far along the game is, and the full sortable list behind it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::log::warn;
use crate::models::Game;
use crate::ui::{accessible_label, spawn_bg, window::App};

/// The last thing earned, and a way into the rest — the shape Steam settled on.
///
/// Starts hidden and stays that way for a game Epic has no achievements for,
/// which is most of the library. Nothing here announces its own absence.
pub(super) fn achievements_block(app: &Rc<App>, game: &Game) -> gtk::Box {
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .visible(false)
        .build();

    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    count.add_css_class("numeric");
    let bar = gtk::ProgressBar::builder()
        .width_request(120)
        .valign(gtk::Align::Center)
        .build();
    accessible_label(&bar, "Achievements Earned");
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    head.append(&bar);
    head.append(&count);

    let group = adw::PreferencesGroup::builder()
        .title("Achievements")
        .build();
    group.set_header_suffix(Some(&head));

    let row = adw::ActionRow::builder()
        .activatable(true)
        .subtitle_lines(2)
        .use_markup(false)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    group.add(&row);
    block.append(&group);

    let weak = Rc::downgrade(app);
    let owned = game.clone();
    row.connect_activated(move |_| {
        if let Some(app) = weak.upgrade() {
            achievements_dialog(&app, &owned);
        }
    });

    let showing = block.clone();
    let target = row.clone();
    with_achievements(app, game, move |progress| {
        let Some(progress) = progress else {
            return;
        };
        count.set_label(&format!("{} of {}", progress.unlocked, progress.total));
        bar.set_fraction(progress.fraction());
        fill_latest(&target, &progress);
        showing.set_visible(true);
    });
    block
}

/// Fills the block's one row with the newest earned achievement. The list
/// arrives with earned ones first and newest first, so the first hit is it.
fn fill_latest(row: &adw::ActionRow, progress: &crate::epic::achievements::Progress) {
    match progress.achievements.iter().find(|a| a.unlocked) {
        Some(latest) => {
            row.set_title(&latest.title);
            row.set_subtitle(&latest_blurb(latest));
            row.set_tooltip_text(Some(&latest.name));
            row.add_prefix(&achievement_icon(latest));
        }
        None => {
            row.set_title("Nothing earned yet");
            row.set_subtitle(&format!("{} waiting", progress.total));
        }
    }
}

fn latest_blurb(achievement: &crate::epic::achievements::Achievement) -> String {
    let day = achievement
        .unlocked_at
        .split('T')
        .next()
        .unwrap_or_default();
    let mut parts = Vec::new();
    if !achievement.description.is_empty() {
        parts.push(achievement.description.clone());
    }
    if !day.is_empty() {
        parts.push(format!("earned {day}"));
    }
    parts.push(format!("{}% of players", achievement.rarity));
    parts.join("  ·  ")
}

/// What Epic has for this game, and how far this account has got with it.
fn achievements_dialog(app: &Rc<App>, game: &Game) {
    use crate::epic::achievements::{Order, Progress};

    let group = adw::PreferencesGroup::builder()
        .title("Achievements")
        .build();
    let waiting = adw::ActionRow::builder().title("Looking…").build();
    group.add(&waiting);

    let order = gtk::DropDown::from_strings(&["Earned", "Rarity", "Name"]);
    order.set_tooltip_text(Some("What to line the list up by"));
    accessible_label(&order, "Sort By");
    let flip = gtk::ToggleButton::builder()
        .icon_name("view-sort-descending-symbolic")
        .tooltip_text("Newest, commonest or last first")
        .active(true)
        .build();
    accessible_label(&flip, "Reverse Order");
    flip.add_css_class("flat");
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .visible(false)
        .build();
    controls.append(&order);
    controls.append(&flip);
    group.set_header_suffix(Some(&controls));

    // Content, not settings: see the add-ons dialog for why this is an
    // AdwDialog rather than the preferences one.
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let view = adw::ToolbarView::builder().content(&page).build();
    view.add_top_bar(&adw::HeaderBar::new());

    let dialog = adw::Dialog::builder()
        .title(&game.title)
        .content_width(560)
        .content_height(640)
        .child(&view)
        .build();
    dialog.present(Some(&app.window));

    let held: Rc<RefCell<Option<Progress>>> = Rc::new(RefCell::new(None));
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    // Every control writes the whole list back, so they share one handler.
    let relist = {
        let group = group.clone();
        let held = held.clone();
        let rows = rows.clone();
        let order = order.clone();
        let flip = flip.clone();
        move || {
            let borrowed = held.borrow();
            let Some(progress) = borrowed.as_ref() else {
                return;
            };
            for row in rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            let by = match order.selected() {
                1 => Order::Rarity,
                2 => Order::Name,
                _ => Order::Earned,
            };
            let mut fresh = Vec::new();
            for achievement in progress.in_order(by, flip.is_active()) {
                let row = achievement_row(&achievement);
                group.add(&row);
                fresh.push(row);
            }
            *rows.borrow_mut() = fresh;
        }
    };

    {
        let relist = relist.clone();
        order.connect_selected_notify(move |_| relist());
    }
    {
        let relist = relist.clone();
        flip.connect_toggled(move |button| {
            button.set_icon_name(match button.is_active() {
                true => "view-sort-descending-symbolic",
                false => "view-sort-ascending-symbolic",
            });
            relist();
        });
    }

    with_achievements(app, game, move |progress| {
        group.remove(&waiting);
        let Some(progress) = progress else {
            group.set_description(Some("Epic has none for this game."));
            return;
        };
        group.set_description(Some(&format!(
            "{} of {} earned · {} of {} XP",
            progress.unlocked, progress.total, progress.xp, progress.total_xp
        )));
        controls.set_visible(true);
        *held.borrow_mut() = Some(progress);
        relist();
    });
}

/// Hands over a game's achievements, from the session cache once they have been
/// asked for. A failure is not cached: a flaky minute should not hide the block
/// until Divine is restarted.
fn with_achievements(
    app: &Rc<App>,
    game: &Game,
    done: impl FnOnce(Option<crate::epic::achievements::Progress>) + 'static,
) {
    if let Some(cached) = app.achievements.borrow().get(&game.namespace) {
        done(cached.clone());
        return;
    }
    let namespace = game.namespace.clone();
    let key = game.namespace.clone();
    let user_file = app.lgd.borrow().user_file();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || crate::epic::achievements::for_game(&namespace, &user_file),
        move |result| {
            let found = match result {
                Ok(found) => found,
                Err(e) => {
                    warn!("cannot read the achievements: {e:#}");
                    done(None);
                    return;
                }
            };
            if let Some(app) = weak.upgrade() {
                app.achievements.borrow_mut().insert(key, found.clone());
            }
            done(found);
        },
    );
}

/// Epic's own art, cached like everything else it serves. Locked ones get the
/// icon Epic ships for that state, greyed rather than hidden: it is still a
/// hint at what the thing takes.
fn achievement_icon(achievement: &crate::epic::achievements::Achievement) -> gtk::Image {
    let icon = gtk::Image::builder()
        .pixel_size(48)
        .icon_name("emblem-default-symbolic")
        .build();
    if !achievement.unlocked {
        icon.set_opacity(0.45);
    }
    if achievement.icon.is_empty() {
        return icon;
    }
    let url = achievement.icon.clone();
    let target = icon.clone();
    spawn_bg(
        move || crate::covers::fetch_achievement(&url),
        move |result| match result {
            Ok(path) => match gtk::gdk::Texture::from_filename(&path) {
                Ok(texture) => target.set_paintable(Some(&texture)),
                Err(e) => warn!("broken achievement icon: {e:#}"),
            },
            Err(e) => warn!("achievement icon failed: {e:#}"),
        },
    );
    icon
}

fn achievement_row(achievement: &crate::epic::achievements::Achievement) -> adw::ActionRow {
    // Epic keeps a hidden one's blurb back until it is earned, and says so
    // where the blurb would go rather than leaving the row looking broken.
    let blurb = match (achievement.hidden, achievement.unlocked) {
        (true, false) => "Hidden until earned".to_string(),
        _ => achievement.description.clone(),
    };
    let earned = match achievement.unlocked_at.split('T').next() {
        Some(day) if !day.is_empty() => format!("  ·  earned {day}"),
        _ => String::new(),
    };

    let row = adw::ActionRow::builder()
        .title(&achievement.title)
        .subtitle(format!("{blurb}{earned}"))
        .subtitle_lines(2)
        .use_markup(false)
        // Epic's own id for it, which is what guides and trackers go by.
        .tooltip_text(&achievement.name)
        .build();

    row.add_prefix(&achievement_icon(achievement));

    let facts = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    let rarity = gtk::Label::new(Some(&format!("{}%", achievement.rarity)));
    rarity.add_css_class("numeric");
    let worth = gtk::Label::new(Some(&match achievement.tier.as_str() {
        "" => format!("{} XP", achievement.xp),
        tier => format!("{} XP · {tier}", achievement.xp),
    }));
    worth.add_css_class("dim-label");
    worth.add_css_class("caption");
    facts.append(&rarity);
    facts.append(&worth);
    row.add_suffix(&facts);

    if achievement.unlocked {
        let tick = gtk::Image::from_icon_name("object-select-symbolic");
        tick.add_css_class("success");
        row.add_suffix(&tick);
    }
    row
}
