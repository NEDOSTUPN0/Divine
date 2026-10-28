//! Epic sign-in: open the login page in a browser, paste the code back.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::log::error;
use crate::ui::{spawn_bg, window::App};

/// legendary's own redirect helper; it lands on a JSON blob with the code.
const LOGIN_URL: &str = "https://legendary.gl/epiclogin";

pub fn view(app: &Rc<App>) -> gtk::Widget {
    let open = gtk::Button::builder()
        .label("Open the Sign-in Page")
        .halign(gtk::Align::Center)
        .build();
    open.add_css_class("suggested-action");
    open.add_css_class("pill");

    let code = adw::EntryRow::builder().title("authorizationCode").build();

    let group = adw::PreferencesGroup::builder()
        .description(
            "Sign in to Epic in your browser, then paste the authorizationCode value \
             here. Pasting the whole JSON response works too.",
        )
        .build();
    group.add(&code);

    let submit = gtk::Button::builder()
        .label("Sign In")
        .halign(gtk::Align::Center)
        .sensitive(false)
        .build();
    submit.add_css_class("suggested-action");
    submit.add_css_class("pill");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    content.append(&open);
    content.append(&group);
    content.append(&submit);

    let status = adw::StatusPage::builder()
        .icon_name("avatar-default-symbolic")
        .title("Sign In to Epic Games")
        .child(
            &adw::Clamp::builder()
                .maximum_size(480)
                .child(&content)
                .build(),
        )
        .build();

    let window = app.window.clone();
    open.connect_clicked(move |_| {
        gtk::UriLauncher::new(LOGIN_URL).launch(Some(&window), gio::Cancellable::NONE, |result| {
            if let Err(e) = result {
                error!("cannot open browser: {e:#}");
            }
        });
    });

    let button = submit.clone();
    code.connect_changed(move |entry| {
        button.set_sensitive(!extract_code(&entry.text()).is_empty());
    });

    let weak = Rc::downgrade(app);
    let entry = code.clone();
    submit.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let code = extract_code(&entry.text());
        if code.is_empty() {
            return;
        }
        button.set_sensitive(false);
        button.set_label("Signing in…");

        let lgd = app.lgd.borrow().clone();
        let button = button.clone();
        let weak = Rc::downgrade(&app);
        spawn_bg(
            move || lgd.auth(&code),
            move |result| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                button.set_sensitive(true);
                button.set_label("Sign In");
                match result {
                    Ok(name) => {
                        app.toast(&format!("Signed in as {name}"));
                        app.show_library();
                        app.refresh();
                    }
                    Err(e) => app.toast_error("Sign-in failed", &e),
                }
            },
        );
    });

    let view = adw::ToolbarView::builder().content(&status).build();
    view.add_top_bar(&adw::HeaderBar::new());
    view.upcast()
}

/// Accepts a bare code, a quoted code, or the whole JSON response Epic shows.
fn extract_code(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('{') {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(code) = json.get("authorizationCode").and_then(|c| c.as_str()) {
                return code.to_string();
            }
        }
    }
    trimmed.trim_matches('"').to_string()
}
