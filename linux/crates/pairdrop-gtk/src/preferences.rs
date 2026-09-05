//! Preferences and pairing.
//!
//! Both live in one dialog: the server address people set once, and the pairing flow
//! they come back to occasionally.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pairdrop_client::{Command, Settings};
use pairdrop_pairing::PairedDevice;

use crate::window::Ui;

thread_local! {
    /// The live pairing widgets, so an engine event can update them without threading a
    /// handle through every call. Single-threaded by construction — GTK owns this thread.
    static PAIRING: RefCell<Option<PairingWidgets>> = const { RefCell::new(None) };
    static PAIRED_GROUP: RefCell<Option<adw::PreferencesGroup>> = const { RefCell::new(None) };
    static PAIRED_ROWS: RefCell<Vec<adw::ActionRow>> = const { RefCell::new(Vec::new()) };
}

struct PairingWidgets {
    key_row: adw::ActionRow,
    create: gtk::Button,
    cancel: gtk::Button,
}

pub fn present(ui: &Rc<Ui>) {
    build_dialog(ui, false);
}

pub fn present_pairing(ui: &Rc<Ui>) {
    build_dialog(ui, true);
}

fn build_dialog(ui: &Rc<Ui>, focus_pairing: bool) {
    let dialog = adw::PreferencesWindow::builder()
        .transient_for(&ui.window)
        .modal(true)
        .search_enabled(false)
        .build();

    dialog.add(&connection_page(ui, &dialog));
    let pairing = pairing_page(ui);
    dialog.add(&pairing);
    if focus_pairing {
        dialog.set_visible_page(&pairing);
    }

    // The widgets are only valid while the dialog is up.
    dialog.connect_close_request(|_| {
        PAIRING.with(|p| *p.borrow_mut() = None);
        PAIRED_GROUP.with(|g| *g.borrow_mut() = None);
        PAIRED_ROWS.with(|r| r.borrow_mut().clear());
        glib::Propagation::Proceed
    });

    dialog.present();
}

fn connection_page(ui: &Rc<Ui>, dialog: &adw::PreferencesWindow) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Connection")
        .icon_name("network-server-symbolic")
        .build();

    let settings = ui.settings.borrow().clone();

    let server_group = adw::PreferencesGroup::builder()
        .title("Server")
        .description(
            "Your PairDrop instance — the same address you'd open in a browser. \
             Nothing is configured by default.",
        )
        .build();

    let server = adw::EntryRow::builder().title("Address").text(&settings.server).build();
    server_group.add(&server);

    let untrusted = adw::SwitchRow::builder()
        .title("Trust self-signed certificates")
        .subtitle("Only for an instance on your own network")
        .active(settings.allow_untrusted_tls)
        .build();
    server_group.add(&untrusted);

    let device_group = adw::PreferencesGroup::builder().title("This device").build();

    let name = adw::EntryRow::builder()
        .title("Device name")
        .text(&settings.display_name)
        .build();
    // Empty means "use the hostname", which is worth saying rather than leaving blank.
    name.set_tooltip_text(Some(&format!(
        "What other devices call this one. Leave empty to use \"{}\".",
        pairdrop_client::settings::hostname()
    )));
    device_group.add(&name);

    let folder = adw::ActionRow::builder()
        .title("Save files to")
        .subtitle(settings.download_directory.display().to_string())
        .activatable(true)
        .build();
    let folder_icon = gtk::Image::from_icon_name("folder-open-symbolic");
    folder.add_suffix(&folder_icon);
    device_group.add(&folder);

    let chosen_folder = Rc::new(RefCell::new(settings.download_directory.clone()));
    folder.connect_activated({
        let ui = Rc::clone(ui);
        let chosen = Rc::clone(&chosen_folder);
        let row = folder.clone();
        move |_| {
            let chooser = gtk::FileDialog::builder().title("Save Files To").modal(true).build();
            let chosen = Rc::clone(&chosen);
            let row = row.clone();
            chooser.select_folder(
                Some(&ui.window.clone()),
                gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            row.set_subtitle(&path.display().to_string());
                            *chosen.borrow_mut() = path;
                        }
                    }
                },
            );
        }
    });

    page.add(&server_group);
    page.add(&device_group);

    // Applying on close rather than on every keystroke: a half-typed address would
    // otherwise send the engine into a reconnect loop.
    dialog.connect_close_request({
        let ui = Rc::clone(ui);
        let server = server.clone();
        let name = name.clone();
        let untrusted = untrusted.clone();
        let chosen_folder = Rc::clone(&chosen_folder);
        move |_| {
            let updated = Settings {
                server: server.text().trim().to_string(),
                display_name: name.text().trim().to_string(),
                download_directory: chosen_folder.borrow().clone(),
                allow_untrusted_tls: untrusted.is_active(),
            };

            let changed = *ui.settings.borrow() != updated;
            if changed {
                if let Err(error) = updated.save() {
                    ui.toasts.add_toast(adw::Toast::new(&format!(
                        "Couldn't save preferences: {error}"
                    )));
                }
                *ui.settings.borrow_mut() = updated.clone();
                ui.engine.send(Command::Connect(updated));
            }
            glib::Propagation::Proceed
        }
    });

    page
}

fn pairing_page(ui: &Rc<Ui>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Devices")
        .icon_name("phone-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Pair a device")
        .description(
            "Pairing lets two devices find each other even on different networks. \
             One creates a key, the other enters it.",
        )
        .build();

    let key_row = adw::ActionRow::builder()
        .title("Pairing key")
        .subtitle("No pairing in progress")
        .build();
    // `.property` dims the title and emphasises the subtitle, which is what we want:
    // the key is the thing someone has to read out.
    key_row.add_css_class("property");

    let create = gtk::Button::builder()
        .label("Create key")
        .valign(gtk::Align::Center)
        .build();
    let cancel = gtk::Button::builder()
        .label("Cancel")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    key_row.add_suffix(&create);
    key_row.add_suffix(&cancel);
    group.add(&key_row);

    create.connect_clicked({
        let ui = Rc::clone(ui);
        move |_| ui.engine.send(Command::BeginPairing)
    });
    cancel.connect_clicked({
        let ui = Rc::clone(ui);
        move |_| ui.engine.send(Command::CancelPairing)
    });

    let join = adw::EntryRow::builder().title("Their key").build();
    let join_button = gtk::Button::builder()
        .label("Join")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    join.add_suffix(&join_button);
    group.add(&join);

    // Six digits or nothing — the server rate-limits wrong guesses.
    join.connect_changed({
        let button = join_button.clone();
        move |entry| {
            let digits = entry.text().chars().filter(char::is_ascii_digit).count();
            button.set_sensitive(digits == 6);
        }
    });
    join_button.connect_clicked({
        let ui = Rc::clone(ui);
        let entry = join.clone();
        move |_| {
            ui.engine.send(Command::JoinPairing { key: entry.text().to_string() });
            entry.set_text("");
        }
    });

    let paired = adw::PreferencesGroup::builder().title("Paired devices").build();

    page.add(&group);
    page.add(&paired);

    PAIRING.with(|p| {
        *p.borrow_mut() = Some(PairingWidgets {
            key_row: key_row.clone(),
            create: create.clone(),
            cancel: cancel.clone(),
        })
    });
    PAIRED_GROUP.with(|g| *g.borrow_mut() = Some(paired.clone()));

    // Draw whatever the engine already told us about.
    //
    // Bound to a local first: passing `ui.paired_devices.borrow().clone()` inline keeps
    // the shared borrow alive for the whole call, and `set_paired_devices` takes
    // `borrow_mut()` — which aborts the process rather than unwinding, because the
    // panic crosses the GTK callback boundary.
    let known = ui.paired_devices.borrow().clone();
    set_paired_devices(ui, known);

    page
}

// MARK: engine-driven updates

pub fn show_pairing_key(ui: &Rc<Ui>, key: &str) {
    PAIRING.with(|p| {
        if let Some(widgets) = p.borrow().as_ref() {
            widgets.key_row.set_title("Enter this on the other device");
            widgets.key_row.set_subtitle(key);
            widgets.create.set_visible(false);
            widgets.cancel.set_visible(true);
        }
    });
    // Also surfaced as a toast, because the dialog may not be open.
    ui.toasts.add_toast(adw::Toast::new(&format!("Pairing key: {key}")));
}

pub fn pairing_ended(_ui: &Rc<Ui>) {
    PAIRING.with(|p| {
        if let Some(widgets) = p.borrow().as_ref() {
            widgets.key_row.set_title("Pairing key");
            widgets.key_row.set_subtitle("No pairing in progress");
            widgets.create.set_visible(true);
            widgets.cancel.set_visible(false);
        }
    });
}

pub fn set_paired_devices(ui: &Rc<Ui>, devices: Vec<PairedDevice>) {
    *ui.paired_devices.borrow_mut() = devices.clone();

    PAIRED_GROUP.with(|group| {
        let group = group.borrow();
        let Some(group) = group.as_ref() else { return };

        PAIRED_ROWS.with(|rows| {
            let mut rows = rows.borrow_mut();
            for row in rows.drain(..) {
                group.remove(&row);
            }

            if devices.is_empty() {
                let empty = adw::ActionRow::builder()
                    .title("No paired devices")
                    .subtitle("Devices you pair stay visible from any network")
                    .build();
                group.add(&empty);
                rows.push(empty);
                return;
            }

            for device in devices {
                let row = adw::ActionRow::builder().title(&device.display_name).build();

                let auto = gtk::Switch::builder()
                    .valign(gtk::Align::Center)
                    .active(device.auto_accept)
                    .tooltip_text("Accept files from this device without asking")
                    .build();
                auto.connect_state_set({
                    let ui = Rc::clone(ui);
                    let secret = device.secret.clone();
                    move |_, enabled| {
                        ui.engine.send(Command::SetAutoAccept {
                            secret: secret.clone(),
                            enabled,
                        });
                        glib::Propagation::Proceed
                    }
                });

                let forget = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text("Forget this device")
                    .build();
                forget.add_css_class("flat");
                forget.connect_clicked({
                    let ui = Rc::clone(ui);
                    let secret = device.secret.clone();
                    move |_| ui.engine.send(Command::Unpair { secret: secret.clone() })
                });

                row.add_suffix(&auto);
                row.add_suffix(&forget);
                group.add(&row);
                rows.push(row);
            }
        });
    });
}
