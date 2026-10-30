//! What Divine sets up for this game on its own, and why, for the games the
//! known fixes cover. A setting nobody switched on is invisible until it goes
//! wrong, so the page says what is on and what in the game's settings has
//! turned it back off.
//!
//! It opens unfolded, since the note is worth reading once, and remembers
//! being folded per game, since it is not worth reading on every visit.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::models::Game;
use crate::runtimes::Runtime;
use crate::ui::window::App;

pub(super) fn fixes_group(app: &Rc<App>, game: &Game) -> Option<adw::PreferencesGroup> {
    let fix = crate::known_fixes::for_game(&game.app_name)?;
    let saved = app.cfg.borrow().game(&game.app_name);
    let folded = app
        .cfg
        .borrow()
        .window
        .folded_fixes
        .contains(&game.app_name);

    let fixes = adw::ExpanderRow::builder()
        .title("Known Fixes")
        .expanded(!folded)
        .build();
    fixes.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));

    if !fix.note.is_empty() {
        fixes.add_row(
            &adw::ActionRow::builder()
                .title(&fix.note)
                .use_markup(false)
                .build(),
        );
    }
    if fix.fake_epic_launcher {
        fixes.add_row(&state_row(
            "Fake Epic Games Launcher",
            saved.fake_epic_launcher,
        ));
    }
    for runtime in Runtime::ALL {
        if fix.runtime(runtime) {
            fixes.add_row(&state_row(runtime.title(), saved.runtime_override(runtime)));
        }
    }
    if let Some(url) = fix.page_url() {
        fixes.add_row(&page_row(app, &fix.page_title, url));
    }

    let weak = Rc::downgrade(app);
    let app_name = game.app_name.clone();
    fixes.connect_expanded_notify(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        {
            let folded = &mut app.cfg.borrow_mut().window.folded_fixes;
            if row.is_expanded() {
                folded.remove(&app_name);
            } else {
                folded.insert(app_name.clone());
            }
        }
        let saved = app.cfg.borrow().save();
        if let Err(e) = saved {
            app.toast_error("Cannot save the settings", &e);
        }
    });

    let group = adw::PreferencesGroup::new();
    group.add(&fixes);
    Some(group)
}

/// One setting the fix turns on, and whether it still is. Forcing it on in the
/// game's settings changes nothing worth saying; turning it off is.
fn state_row(title: &str, chosen: Option<bool>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    let state = gtk::Label::new(Some(if chosen == Some(false) {
        "Off in Game Settings"
    } else {
        "On"
    }));
    state.add_css_class("dim-label");
    row.add_suffix(&state);
    row
}

/// The longer write-up, opened in the browser.
fn page_row(app: &Rc<App>, title: &str, url: String) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(if title.is_empty() { "Read More" } else { title })
        .use_markup(false)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let weak = Rc::downgrade(app);
    row.connect_activated(move |_| {
        if let Some(app) = weak.upgrade() {
            gtk::UriLauncher::new(&url).launch(Some(&app.window), gio::Cancellable::NONE, |_| {});
        }
    });
    row
}
