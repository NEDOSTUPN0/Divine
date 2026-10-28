//! The windows, pages and dialogs, plus the few background errands that more
//! than one of them starts.

pub mod downloads;
pub mod game_card;
pub mod game_page;
pub mod login;
pub mod logs;
pub mod preferences;
pub mod proton_page;
pub mod video;
pub mod window;

use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::ui::window::App;

/// Downloads Epic's overlay unless it is already there. It is one shared install
/// of about 600 MB, so it is fetched the moment a switch asks for it rather than
/// silently in the middle of a launch.
pub fn ensure_eos_overlay(app: &Rc<App>) {
    let lgd = app.lgd.borrow().clone();
    if lgd.eos_overlay_installed() {
        return;
    }
    app.toast("Downloading Epic's overlay, about 600 MB…");

    let weak = Rc::downgrade(app);
    spawn_bg(
        move || lgd.eos_overlay_install(&crate::config::eos_overlay_dir()),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(()) => app.toast("Epic's overlay is ready"),
                Err(e) => app.toast_error("Cannot download Epic's overlay", &e),
            }
        },
    );
}

/// Gives a control the name assistive technology reads out.
///
/// GTK works a name out from a button's own label, so a button showing nothing
/// but an icon has none and is announced by the icon it happens to carry. A
/// tooltip does not stand in: GTK files that as the description, which is read
/// after the name and often not at all. Everything here that shows an icon
/// instead of a word, and every list a label was never put beside, says what it
/// is through this.
pub fn accessible_label(control: &impl IsA<gtk::Accessible>, name: &str) {
    control
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(name)]);
}

/// Runs blocking work off the main loop and delivers the result back on it.
pub fn spawn_bg<T, W, D>(work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    D: FnOnce(T) + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    gio::spawn_blocking(move || {
        let _ = tx.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(value) = rx.recv().await {
            done(value);
        }
    });
}
