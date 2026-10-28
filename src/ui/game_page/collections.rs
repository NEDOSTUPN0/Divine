//! Putting a game in Epic's collections, and taking it out again.
//!
//! Every change goes straight to Epic and the sidebar is redrawn from what
//! comes back, so Divine never shows a membership it has not confirmed. The
//! switches are turned off while a request is in flight for the same reason:
//! two quick taps would otherwise race, and Epic has no way to tell the second
//! one that the first is still landing.

use std::rc::Rc;

use adw::prelude::*;

use crate::epic::collections::{self, Collection, Member, MAX_COLLECTIONS, MAX_NAME};
use crate::models::Game;
use crate::ui::{accessible_label, spawn_bg, window::App};

/// The star in the game page header. Filled when the game is a favourite.
pub(super) fn favorite_button(app: &Rc<App>, game: &Game) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("non-starred-symbolic")
        .build();
    // Both the tooltip and the name follow the state, so they are set together
    // with the icon rather than here.
    show_favorite(app, game, &button);

    let weak = Rc::downgrade(app);
    let clicked = game.clone();
    button.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(star) = collections::favorites(&app.collections.borrow()).cloned() else {
            app.toast("Epic has not sent the collections yet");
            return;
        };
        apply(&app, &star.id, &clicked, !star.holds(&clicked.namespace));
    });

    // The page is usually built before Epic has answered, so the star starts
    // out empty and has to catch up when the collections land. Returning false
    // once the button is gone unsubscribes.
    let weak = Rc::downgrade(app);
    let watched = button.downgrade();
    let game_again = game.clone();
    app.watch(move |_| {
        let (Some(app), Some(button)) = (weak.upgrade(), watched.upgrade()) else {
            return false;
        };
        show_favorite(&app, &game_again, &button);
        true
    });

    button
}

fn show_favorite(app: &Rc<App>, game: &Game, button: &gtk::Button) {
    let starred = collections::favorites(&app.collections.borrow())
        .is_some_and(|star| star.holds(&game.namespace));
    button.set_icon_name(if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    let name = if starred {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    };
    button.set_tooltip_text(Some(name));
    accessible_label(button, name);
}

/// Which collections hold this game, with a switch each.
pub(super) fn dialog(app: &Rc<App>, game: &Game) {
    // A plain list rather than an AdwPreferencesGroup: the rows are redrawn
    // whenever Epic answers, and only a ListBox can be emptied again.
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");

    let group = adw::PreferencesGroup::builder()
        .title("Collections")
        .description("Shared with the Epic Games Launcher")
        .build();
    group.add(&list);

    let page = adw::PreferencesPage::new();
    page.add(&group);
    page.add(&new_group(app, game));

    let view = adw::ToolbarView::builder().content(&page).build();
    view.add_top_bar(&adw::HeaderBar::new());

    let dialog = adw::Dialog::builder()
        .title(&game.title)
        .content_width(460)
        .content_height(520)
        .child(&view)
        .build();

    fill(app, game, &list);
    // Redrawn whenever the collections are re-read, which is what follows every
    // change. Returning false once the dialog is gone unsubscribes.
    let weak = Rc::downgrade(app);
    let watched = dialog.downgrade();
    let game_again = game.clone();
    let list_again = list.clone();
    app.watch(move |_| {
        let (Some(app), Some(_)) = (weak.upgrade(), watched.upgrade()) else {
            return false;
        };
        fill(&app, &game_again, &list_again);
        true
    });

    dialog.present(Some(&app.window));
}

fn fill(app: &Rc<App>, game: &Game, list: &gtk::ListBox) {
    list.remove_all();

    let collections = app.collections.borrow().clone();
    if collections.is_empty() {
        let empty = adw::ActionRow::builder()
            .title("No collections yet")
            .subtitle("Make one below, or in the Epic Games Launcher")
            .build();
        empty.set_sensitive(false);
        list.append(&empty);
        return;
    }

    for collection in collections {
        list.append(&membership_row(app, game, &collection));
    }
}

fn membership_row(app: &Rc<App>, game: &Game, collection: &Collection) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(&collection.name)
        .active(collection.holds(&game.namespace))
        .use_markup(false)
        .build();
    if collection.is_favorites() {
        row.set_subtitle("Epic's own collection");
    }

    let weak = Rc::downgrade(app);
    let id = collection.id.clone();
    let game = game.clone();
    row.connect_active_notify(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let held = app
            .collections
            .borrow()
            .iter()
            .any(|c| c.id == id && c.holds(&game.namespace));
        // Also fires when the row is redrawn from the answer, and sending the
        // change that is already made would be a second, pointless request.
        if held == row.is_active() {
            return;
        }
        apply(&app, &id, &game, row.is_active());
    });
    row
}

/// The bottom of the dialog: a name, and a button that makes a collection with
/// this game already in it — the way the Epic launcher does it.
fn new_group(app: &Rc<App>, game: &Game) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("New Collection")
        .build();

    let name = adw::EntryRow::builder().title("Name").build();

    let create = gtk::Button::builder()
        .label("Create")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    create.add_css_class("suggested-action");
    name.add_suffix(&create);

    let button = create.clone();
    name.connect_changed(move |entry| {
        // Epic answers a longer name with a 400, so it is cut here rather than
        // typed in full and refused. The launcher stops at the same length.
        let text = entry.text();
        if text.chars().count() > MAX_NAME {
            entry.set_text(&text.chars().take(MAX_NAME).collect::<String>());
            entry.set_position(-1);
            return;
        }
        button.set_sensitive(!text.trim().is_empty());
    });

    let weak = Rc::downgrade(app);
    let game = game.clone();
    let entry = name.clone();
    create.connect_clicked(move |create| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.collections.borrow().len() >= MAX_COLLECTIONS {
            app.toast(&format!(
                "Epic allows {MAX_COLLECTIONS} collections, Favorites included"
            ));
            return;
        }

        let wanted = entry.text().trim().to_string();
        let user_file = app.lgd.borrow().user_file();
        let member = member(&game);
        let namespace = game.namespace.clone();
        create.set_sensitive(false);

        let weak = Rc::downgrade(&app);
        let entry = entry.clone();
        let create = create.clone();
        spawn_bg(
            move || {
                // Only Epic knows the new id, so this one waits. It hands the
                // id back, which is everything the sidebar needs.
                let id = collections::create(&user_file, &wanted)?;
                collections::set_member(&user_file, &id, &member, true)?;
                Ok::<_, anyhow::Error>((id, wanted))
            },
            move |result| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                create.set_sensitive(true);
                match result {
                    Ok((id, name)) => {
                        entry.set_text("");
                        app.show_collections(|all| {
                            all.push(Collection {
                                id,
                                name,
                                members: vec![namespace],
                            })
                        });
                    }
                    Err(e) => app.toast_error("Cannot make the collection", &e),
                }
            },
        );
    });

    group.add(&name);
    group
}

/// Shows one membership change at once, then sends it.
///
/// Put back and re-read from Epic if the request fails, which is the only time
/// Divine and the account can disagree.
fn apply(app: &Rc<App>, collection_id: &str, game: &Game, held: bool) {
    let namespace = game.namespace.clone();
    let id = collection_id.to_string();
    app.show_collections(|all| set_locally(all, &id, &namespace, held));

    let user_file = app.lgd.borrow().user_file();
    let member = member(game);
    let weak = Rc::downgrade(app);
    let sending = id.clone();

    spawn_bg(
        move || collections::set_member(&user_file, &sending, &member, held),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if let Err(e) = result {
                app.show_collections(|all| set_locally(all, &id, &namespace, !held));
                app.toast_error("Cannot change the collection", &e);
                app.load_collections();
            }
        },
    );
}

/// The same change to the copy Divine is showing that Epic is being asked for.
fn set_locally(all: &mut [Collection], collection_id: &str, namespace: &str, held: bool) {
    let Some(collection) = all.iter_mut().find(|c| c.id == collection_id) else {
        return;
    };
    collection.members.retain(|member| member != namespace);
    if held {
        collection.members.push(namespace.to_string());
    }
}

fn member(game: &Game) -> Member {
    Member {
        namespace: game.namespace.clone(),
        catalog_id: game.catalog_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holding(members: &[&str]) -> Vec<Collection> {
        vec![Collection {
            id: "f6f381b2".into(),
            name: "Racing".into(),
            members: members.iter().map(|s| s.to_string()).collect(),
        }]
    }

    /// What the sidebar shows between the click and Epic's answer.
    #[test]
    fn a_game_lands_in_the_collection_at_once_and_only_once() {
        let mut all = holding(&["calluna"]);
        set_locally(&mut all, "f6f381b2", "catnip", true);
        assert_eq!(all[0].members, ["calluna", "catnip"]);

        // Adding it twice must not list it twice, which a plain push would.
        set_locally(&mut all, "f6f381b2", "catnip", true);
        assert_eq!(all[0].members, ["calluna", "catnip"]);

        set_locally(&mut all, "f6f381b2", "catnip", false);
        assert_eq!(all[0].members, ["calluna"]);
        // Taking out what was never in leaves the rest alone.
        set_locally(&mut all, "f6f381b2", "angelonia", false);
        assert_eq!(all[0].members, ["calluna"]);
    }

    /// A collection Epic has since dropped is not one to invent a place for.
    #[test]
    fn a_change_to_a_collection_that_is_gone_does_nothing() {
        let mut all = holding(&["calluna"]);
        set_locally(&mut all, "vanished", "catnip", true);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].members, ["calluna"]);
    }
}
