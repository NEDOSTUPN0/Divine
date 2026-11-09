//! An icon in the system tray, spoken over D-Bus as a StatusNotifierItem with
//! its menu as a `com.canonical.dbusmenu`. GTK 4 has no tray of its own, and
//! these two interfaces are what every tray host reads: Plasma, waybar,
//! quickshell, and GNOME through the AppIndicator extension.
//!
//! The item lives for as long as its [`Tray`] does. Dropping it gives up the
//! bus name, which is how a host learns that the icon has gone; there is no
//! call for that.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::{Context, Result};
use gtk::gio;
use gtk::glib::{self, variant::ToVariant, Variant, VariantTy};

use crate::log::warn;

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const ITEM_XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="i" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionMovieName" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewAttentionIcon"/>
    <signal name="NewOverlayIcon"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg name="status" type="s"/></signal>
  </interface>
</node>"#;

const MENU_XML: &str = r#"<node>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout">
      <arg name="parentId" type="i" direction="in"/>
      <arg name="recursionDepth" type="i" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="revision" type="u" direction="out"/>
      <arg name="layout" type="(ia{sv}av)" direction="out"/>
    </method>
    <method name="GetGroupProperties">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="properties" type="a(ia{sv})" direction="out"/>
    </method>
    <method name="GetProperty">
      <arg name="id" type="i" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="Event">
      <arg name="id" type="i" direction="in"/>
      <arg name="eventId" type="s" direction="in"/>
      <arg name="data" type="v" direction="in"/>
      <arg name="timestamp" type="u" direction="in"/>
    </method>
    <method name="EventGroup">
      <arg name="events" type="a(isvu)" direction="in"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <method name="AboutToShow">
      <arg name="id" type="i" direction="in"/>
      <arg name="needUpdate" type="b" direction="out"/>
    </method>
    <method name="AboutToShowGroup">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="updatesNeeded" type="ai" direction="out"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <signal name="ItemsPropertiesUpdated">
      <arg name="updatedProps" type="a(ia{sv})"/>
      <arg name="removedProps" type="a(ias)"/>
    </signal>
    <signal name="LayoutUpdated">
      <arg name="revision" type="u"/>
      <arg name="parent" type="i"/>
    </signal>
    <signal name="ItemActivationRequested">
      <arg name="id" type="i"/>
      <arg name="timestamp" type="u"/>
    </signal>
  </interface>
</node>"#;

/// One line of the tray menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Item(String),
    Separator,
}

/// What the D-Bus handlers share with the [`Tray`] that owns them.
struct State {
    title: String,
    entries: RefCell<Vec<Entry>>,
    /// Bumped with every change to the menu, as dbusmenu asks.
    revision: Cell<u32>,
    on_activate: Box<dyn Fn()>,
    /// Called with the index into the entries of the item picked.
    on_pick: Box<dyn Fn(usize)>,
}

pub struct Tray {
    connection: gio::DBusConnection,
    state: Rc<State>,
    item: Option<gio::RegistrationId>,
    menu: Option<gio::RegistrationId>,
    name: Option<gio::OwnerId>,
    /// Stops watching for the host's watcher. Held as a closure because gio
    /// re-exports another type under the name the watch id would have.
    unwatch: Option<Box<dyn FnOnce()>>,
}

impl Tray {
    /// Puts the icon up. `on_activate` is a click on the icon itself;
    /// `on_pick` a pick from its menu.
    pub fn new(
        title: &str,
        entries: Vec<Entry>,
        on_activate: impl Fn() + 'static,
        on_pick: impl Fn(usize) + 'static,
    ) -> Result<Self> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .context("connecting to the session bus")?;
        let state = Rc::new(State {
            title: title.to_string(),
            entries: RefCell::new(entries),
            revision: Cell::new(1),
            on_activate: Box::new(on_activate),
            on_pick: Box::new(on_pick),
        });

        let item_info = interface(ITEM_XML, "org.kde.StatusNotifierItem")?;
        let held = Rc::clone(&state);
        let item = connection
            .register_object(ITEM_PATH, &item_info)
            .method_call(move |_, _, _, _, method, _, invocation| {
                invocation.return_value(None);
                // Both clicks mean "show me Divine": a host may send either.
                if matches!(method, "Activate" | "SecondaryActivate") {
                    let held = Rc::clone(&held);
                    glib::idle_add_local_once(move || (held.on_activate)());
                }
            })
            .property({
                let held = Rc::clone(&state);
                move |_, _, _, _, name| item_property(&held, name)
            })
            .build()
            .context("offering the tray icon")?;

        let menu_info = interface(MENU_XML, "com.canonical.dbusmenu")?;
        let held = Rc::clone(&state);
        let menu = connection
            .register_object(MENU_PATH, &menu_info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                menu_call(&held, method, &params, invocation);
            })
            .property(|_, _, _, _, name| menu_property(name))
            .build()
            .context("offering the tray menu")?;

        // The spec asks for this shape of name, so that hosts which list the
        // bus can tell items apart from everything else on it.
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        // The watcher only takes a name that is owned, and the name only
        // becomes ours a moment after asking, so the icon is offered once both
        // are true: when the name arrives, and whenever a watcher (re)appears.
        let acquired = Rc::new(Cell::new(false));
        let owned = gio::bus_own_name_on_connection(
            &connection,
            &name,
            gio::BusNameOwnerFlags::NONE,
            {
                let acquired = Rc::clone(&acquired);
                move |connection, name| {
                    acquired.set(true);
                    register(&connection, name);
                }
            },
            |_, name| warn!("the tray icon lost its bus name {name}"),
        );
        let watcher = gio::bus_watch_name_on_connection(
            &connection,
            WATCHER,
            gio::BusNameWatcherFlags::NONE,
            move |connection, _, _| {
                if acquired.get() {
                    register(&connection, &name);
                }
            },
            |_, _| {},
        );

        Ok(Tray {
            connection,
            state,
            item: Some(item),
            menu: Some(menu),
            name: Some(owned),
            unwatch: Some(Box::new(move || gio::bus_unwatch_name(watcher))),
        })
    }

    /// Replaces the menu, telling the host to read it again.
    pub fn set_entries(&self, entries: Vec<Entry>) {
        if *self.state.entries.borrow() == entries {
            return;
        }
        *self.state.entries.borrow_mut() = entries;
        let revision = self.state.revision.get() + 1;
        self.state.revision.set(revision);
        if let Err(e) = self.connection.emit_signal(
            None,
            MENU_PATH,
            "com.canonical.dbusmenu",
            "LayoutUpdated",
            Some(&(revision, 0i32).to_variant()),
        ) {
            warn!("cannot update the tray menu: {e}");
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.take() {
            unwatch();
        }
        if let Some(name) = self.name.take() {
            gio::bus_unown_name(name);
        }
        for id in [self.item.take(), self.menu.take()].into_iter().flatten() {
            let _ = self.connection.unregister_object(id);
        }
    }
}

/// Hands the item to the host's watcher, which puts it in the tray.
fn register(connection: &gio::DBusConnection, name: &str) {
    connection.call(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        WATCHER,
        "RegisterStatusNotifierItem",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |result| {
            // No tray running is an ordinary state of a desktop, and the
            // watcher being started later is caught by the name watch.
            if let Err(e) = result {
                crate::log::debug!("no tray took the icon: {e}");
            }
        },
    );
}

fn interface(xml: &str, name: &str) -> Result<gio::DBusInterfaceInfo> {
    gio::DBusNodeInfo::for_xml(xml)
        .with_context(|| format!("describing {name}"))?
        .lookup_interface(name)
        .with_context(|| format!("{name} is missing from its own description"))
}

fn item_property(state: &State, name: &str) -> Variant {
    let no_pixmaps = || {
        Variant::array_from_iter_with_type(VariantTy::new("(iiay)").unwrap(), [] as [Variant; 0])
    };
    match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => "divine".to_variant(),
        "Title" => state.title.to_variant(),
        "Status" => "Active".to_variant(),
        "WindowId" => 0i32.to_variant(),
        "IconName" => crate::APP_ID.to_variant(),
        "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => no_pixmaps(),
        "OverlayIconName" | "AttentionIconName" | "AttentionMovieName" => "".to_variant(),
        "ToolTip" => Variant::tuple_from_iter([
            crate::APP_ID.to_variant(),
            no_pixmaps(),
            state.title.to_variant(),
            "".to_variant(),
        ]),
        // A click on the icon is Activate; the menu has its own button.
        "ItemIsMenu" => false.to_variant(),
        "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH)
            .map(|path| path.to_variant())
            .unwrap_or_else(|_| "/".to_variant()),
        _ => "".to_variant(),
    }
}

fn menu_property(name: &str) -> Variant {
    match name {
        "Version" => 3u32.to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "Status" => "normal".to_variant(),
        "IconThemePath" => Vec::<String>::new().to_variant(),
        _ => "".to_variant(),
    }
}

/// dbusmenu numbers its items; 0 is the menu itself, and entry `i` is `i + 1`.
fn menu_call(
    state: &Rc<State>,
    method: &str,
    params: &Variant,
    invocation: gio::DBusMethodInvocation,
) {
    match method {
        "GetLayout" => {
            let reply = Variant::tuple_from_iter([
                state.revision.get().to_variant(),
                layout(&state.entries.borrow()),
            ]);
            invocation.return_value(Some(&reply));
        }
        "GetGroupProperties" => {
            let ids: Vec<i32> = params.child_value(0).get().unwrap_or_default();
            let entries = state.entries.borrow();
            let found = ids.into_iter().filter_map(|id| {
                let props = if id == 0 {
                    root_properties()
                } else {
                    entry_properties(entries.get(usize::try_from(id - 1).ok()?)?)
                };
                Some(Variant::tuple_from_iter([id.to_variant(), props]))
            });
            let list =
                Variant::array_from_iter_with_type(VariantTy::new("(ia{sv})").unwrap(), found);
            invocation.return_value(Some(&Variant::tuple_from_iter([list])));
        }
        "GetProperty" => {
            let id: i32 = params.child_value(0).get().unwrap_or_default();
            let name: String = params.child_value(1).get().unwrap_or_default();
            let value = usize::try_from(id - 1)
                .ok()
                .and_then(|index| state.entries.borrow().get(index).cloned())
                .and_then(|entry| {
                    glib::VariantDict::new(Some(&entry_properties(&entry)))
                        .lookup_value(&name, None)
                })
                .unwrap_or_else(|| "".to_variant());
            invocation.return_value(Some(&(Variant::from_variant(&value),).to_variant()));
        }
        "Event" => {
            let id: i32 = params.child_value(0).get().unwrap_or_default();
            let event: String = params.child_value(1).get().unwrap_or_default();
            invocation.return_value(None);
            if event == "clicked" {
                pick(state, id);
            }
        }
        "EventGroup" => {
            let events = params.child_value(0);
            let mut picked = Vec::new();
            for event in events.iter() {
                let id: i32 = event.child_value(0).get().unwrap_or_default();
                let kind: String = event.child_value(1).get().unwrap_or_default();
                if kind == "clicked" {
                    picked.push(id);
                }
            }
            invocation.return_value(Some(&(Vec::<i32>::new(),).to_variant()));
            for id in picked {
                pick(state, id);
            }
        }
        "AboutToShow" => invocation.return_value(Some(&(false,).to_variant())),
        "AboutToShowGroup" => {
            invocation.return_value(Some(&(Vec::<i32>::new(), Vec::<i32>::new()).to_variant()))
        }
        _ => invocation.return_value(None),
    }
}

/// Runs the pick once the reply is out. What it does may well take the tray
/// down with it, Quit for one, and that is no place for a call in progress.
fn pick(state: &Rc<State>, id: i32) {
    let Ok(index) = usize::try_from(id - 1) else {
        return;
    };
    if matches!(state.entries.borrow().get(index), Some(Entry::Item(_))) {
        let state = Rc::clone(state);
        glib::idle_add_local_once(move || (state.on_pick)(index));
    }
}

/// The whole menu as dbusmenu lays it out: `(id, properties, children)`, the
/// children each wrapped in a variant of their own.
fn layout(entries: &[Entry]) -> Variant {
    let children = entries.iter().enumerate().map(|(index, entry)| {
        let child = Variant::tuple_from_iter([
            (index as i32 + 1).to_variant(),
            entry_properties(entry),
            Variant::array_from_iter_with_type(VariantTy::VARIANT, [] as [Variant; 0]),
        ]);
        Variant::from_variant(&child)
    });
    Variant::tuple_from_iter([
        0i32.to_variant(),
        root_properties(),
        Variant::array_from_iter_with_type(VariantTy::VARIANT, children),
    ])
}

fn root_properties() -> Variant {
    let props = glib::VariantDict::new(None);
    props.insert_value("children-display", &"submenu".to_variant());
    props.end()
}

fn entry_properties(entry: &Entry) -> Variant {
    let props = glib::VariantDict::new(None);
    match entry {
        Entry::Item(label) => {
            // dbusmenu reads an underscore as a mnemonic.
            props.insert_value("label", &label.replace('_', "__").to_variant());
        }
        Entry::Separator => props.insert_value("type", &"separator".to_variant()),
    }
    props.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_is_laid_out_the_way_dbusmenu_reads_it() {
        let menu = layout(&[
            Entry::Item("Show Divine".into()),
            Entry::Separator,
            Entry::Item("Quit".into()),
        ]);
        assert_eq!(menu.type_().as_str(), "(ia{sv}av)");
        let children = menu.child_value(2);
        assert_eq!(children.n_children(), 3);
        let quit = children.child_value(2).as_variant().unwrap();
        assert_eq!(quit.child_value(0).get::<i32>(), Some(3));
        assert_eq!(
            glib::VariantDict::new(Some(&quit.child_value(1)))
                .lookup_value("label", None)
                .and_then(|label| label.get::<String>()),
            Some("Quit".into())
        );
        let separator = children.child_value(1).as_variant().unwrap();
        assert_eq!(
            glib::VariantDict::new(Some(&separator.child_value(1)))
                .lookup_value("type", None)
                .and_then(|kind| kind.get::<String>()),
            Some("separator".into())
        );
    }

    #[test]
    fn both_interfaces_describe_themselves() {
        assert!(interface(ITEM_XML, "org.kde.StatusNotifierItem").is_ok());
        assert!(interface(MENU_XML, "com.canonical.dbusmenu").is_ok());
    }
}
