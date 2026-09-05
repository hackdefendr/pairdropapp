//! The main window: a connection banner, the list of nearby devices, and an activity
//! area. Everything here reacts to [`Event`]s from the engine; nothing here does I/O.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use pairdrop_client::{Command, ConnectionState, Engine, Event, PeerView, Settings};
use pairdrop_pairing::PairedDevice;

pub struct Ui {
    pub engine: Rc<Engine>,
    pub settings: RefCell<Settings>,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub status: adw::StatusPage,
    pub banner: adw::Banner,
    pub peer_group: adw::PreferencesGroup,
    /// Last list the engine sent, so the preferences dialog can draw it whenever it
    /// happens to be opened rather than only when a pairing changes.
    pub paired_devices: RefCell<Vec<PairedDevice>>,
    /// Rows currently on screen, so a peer snapshot can be diffed rather than rebuilt
    /// from scratch on every progress tick.
    rows: RefCell<HashMap<String, PeerRow>>,
}

struct PeerRow {
    row: adw::ActionRow,
    progress: gtk::ProgressBar,
    view: PeerView,
}

pub fn build(
    application: &adw::Application,
    engine: Engine,
    events: async_channel::Receiver<Event>,
    settings: Settings,
) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("PairDrop")
        .default_width(420)
        .default_height(560)
        .build();

    let header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    menu.append(Some("Pair a Device…"), Some("app.pair"));
    menu.append(Some("Preferences…"), Some("app.preferences"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu")
        .build();
    header.pack_end(&menu_button);

    let banner = adw::Banner::builder().revealed(false).build();

    // Shown instead of the list while there is nobody to show, which is most of the
    // time on a quiet network — so it has to say something useful.
    let status = adw::StatusPage::builder()
        .icon_name("network-wireless-symbolic")
        .title("Looking for devices")
        .description("Open PairDrop on another device on the same network.")
        .vexpand(true)
        .build();

    let peer_group = adw::PreferencesGroup::builder()
        .title("Nearby devices")
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .visible(false)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&status);
    content.append(&peer_group);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner);
    body.append(&scroller);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&toasts);
    window.set_content(Some(&root));

    let ui = Rc::new(Ui {
        engine: Rc::new(engine),
        settings: RefCell::new(settings),
        window: window.clone(),
        toasts,
        status,
        banner,
        peer_group,
        paired_devices: RefCell::new(Vec::new()),
        rows: RefCell::new(HashMap::new()),
    });

    install_actions(application, &ui);

    // The banner's button is the main way in when nothing is configured yet, so it has
    // to actually open the dialog.
    ui.banner.connect_button_clicked({
        let ui = Rc::clone(&ui);
        move |_| crate::preferences::present(&ui)
    });

    // Bridge the engine's channel into the GTK main loop.
    glib::spawn_future_local({
        let ui = Rc::clone(&ui);
        async move {
            while let Ok(event) = events.recv().await {
                apply(&ui, event);
            }
        }
    });

    window.present();
}

fn install_actions(application: &adw::Application, ui: &Rc<Ui>) {
    let preferences = gio::SimpleAction::new("preferences", None);
    preferences.connect_activate({
        let ui = Rc::clone(ui);
        move |_, _| crate::preferences::present(&ui)
    });
    application.add_action(&preferences);

    let pair = gio::SimpleAction::new("pair", None);
    pair.connect_activate({
        let ui = Rc::clone(ui);
        move |_, _| crate::preferences::present_pairing(&ui)
    });
    application.add_action(&pair);
}

// MARK: applying engine events

fn apply(ui: &Rc<Ui>, event: Event) {
    match event {
        Event::Connection(state) => show_connection(ui, state),

        Event::Peers(peers) => update_peers(ui, peers),

        Event::IncomingRequest { peer_id, peer_name, files, total_size } => {
            ask_about_request(ui, peer_id, peer_name, files, total_size);
        }

        Event::FilesReceived { peer_name, paths } => {
            let toast = adw::Toast::new(&match paths.len() {
                1 => format!("Received {} from {peer_name}", file_name(&paths[0])),
                n => format!("Received {n} files from {peer_name}"),
            });
            // Opening the folder is the thing people want next.
            if let Some(first) = paths.first().and_then(|p| p.parent()).map(PathBuf::from) {
                toast.set_button_label(Some("Show"));
                toast.connect_button_clicked(move |_| {
                    let uri = gio::File::for_path(&first).uri();
                    let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                });
            }
            ui.toasts.add_toast(toast);
        }

        Event::TextReceived { peer_name, text } => {
            // A message with nowhere to go is useless, so put it on the clipboard, which
            // is what the web client does.
            ui.window.clipboard().set_text(&text);
            ui.toasts.add_toast(adw::Toast::new(&format!(
                "Message from {peer_name} copied to the clipboard"
            )));
        }

        Event::SendingFinished { peer_name, files } => {
            ui.toasts.add_toast(adw::Toast::new(&match files {
                1 => format!("Sent 1 file to {peer_name}"),
                n => format!("Sent {n} files to {peer_name}"),
            }));
        }

        Event::Notice(message) | Event::Problem(message) => {
            ui.toasts.add_toast(adw::Toast::new(&message));
        }

        Event::PairingKey(key) => crate::preferences::show_pairing_key(ui, &key),
        Event::PairingEnded => crate::preferences::pairing_ended(ui),
        Event::PairedDevices(devices) => crate::preferences::set_paired_devices(ui, devices),

        Event::SecretStorage { problem, .. } => {
            if problem.is_some() {
                ui.toasts.add_toast(adw::Toast::new(
                    "No desktop keyring — pairings will be lost on quit",
                ));
            }
        }
    }
}

fn show_connection(ui: &Rc<Ui>, state: ConnectionState) {
    match state {
        ConnectionState::NotConfigured => {
            ui.banner.set_title("No server configured — open Preferences to add one.");
            ui.banner.set_button_label(Some("Preferences"));
            ui.banner.set_revealed(true);
            ui.status.set_title("Not connected");
            ui.status
                .set_description(Some("PairDrop needs the address of a PairDrop instance."));
        }
        ConnectionState::Connecting => {
            ui.banner.set_revealed(false);
            ui.status.set_title("Connecting…");
            ui.status.set_description(None);
        }
        ConnectionState::Connected => {
            ui.banner.set_revealed(false);
            ui.status.set_title("Looking for devices");
            ui.status.set_description(Some(
                "Open PairDrop on another device on the same network.",
            ));
        }
        ConnectionState::Retrying { seconds } => {
            ui.banner.set_title(&format!("Connection lost — retrying in {seconds}s"));
            ui.banner.set_button_label(None);
            ui.banner.set_revealed(true);
        }
        ConnectionState::Failed(reason) => {
            ui.banner.set_title(&reason);
            ui.banner.set_button_label(Some("Preferences"));
            ui.banner.set_revealed(true);
            ui.status.set_title("Not connected");
            ui.status.set_description(Some(&reason));
        }
    }
}

fn update_peers(ui: &Rc<Ui>, peers: Vec<PeerView>) {
    let mut rows = ui.rows.borrow_mut();

    // Drop rows for peers that have gone.
    let present: Vec<String> = peers.iter().map(|p| p.id.clone()).collect();
    rows.retain(|id, entry| {
        let keep = present.contains(id);
        if !keep {
            ui.peer_group.remove(&entry.row);
        }
        keep
    });

    for view in peers {
        match rows.get_mut(&view.id) {
            Some(existing) => {
                if existing.view != view {
                    update_row(existing, &view);
                    existing.view = view;
                }
            }
            None => {
                let entry = build_row(ui, &view);
                ui.peer_group.add(&entry.row);
                rows.insert(view.id.clone(), entry);
            }
        }
    }

    let any = !rows.is_empty();
    ui.peer_group.set_visible(any);
    ui.status.set_visible(!any);
}

fn build_row(ui: &Rc<Ui>, view: &PeerView) -> PeerRow {
    let row = adw::ActionRow::builder()
        .title(&view.name)
        .activatable(true)
        .build();

    let progress = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .width_request(80)
        .visible(false)
        .build();
    row.add_suffix(&progress);

    let icon = gtk::Image::from_icon_name("computer-symbolic");
    row.add_prefix(&icon);

    // Click to pick files, since not every desktop makes dragging convenient.
    row.connect_activated({
        let ui = Rc::clone(ui);
        let peer_id = view.id.clone();
        move |_| choose_files(&ui, &peer_id)
    });

    // Drop files straight onto the device — the gesture the whole app exists for.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop({
        let ui = Rc::clone(ui);
        let peer_id = view.id.clone();
        move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            if paths.is_empty() {
                return false;
            }
            ui.engine.send(Command::SendFiles { peer_id: peer_id.clone(), paths });
            true
        }
    });
    row.add_controller(drop);

    let mut entry = PeerRow { row, progress, view: view.clone() };
    update_row(&mut entry, view);
    entry
}

fn update_row(entry: &mut PeerRow, view: &PeerView) {
    entry.row.set_title(&view.name);

    let mut subtitle = if view.connected {
        "Connected".to_string()
    } else {
        "Connecting…".to_string()
    };
    if !view.detail.is_empty() {
        subtitle = format!("{} · {subtitle}", view.detail);
    }
    if view.paired {
        subtitle.push_str(" · paired");
    }
    if let Some(hash) = &view.connection_hash {
        // Shown so two users can confirm they're talking to each other.
        subtitle.push_str(&format!(" · {hash}"));
    }
    entry.row.set_subtitle(&subtitle);

    match view.progress {
        Some(fraction) => {
            entry.progress.set_fraction(fraction);
            entry.progress.set_visible(true);
        }
        None => entry.progress.set_visible(false),
    }

    entry.row.set_sensitive(view.connected && !view.busy);
}

fn choose_files(ui: &Rc<Ui>, peer_id: &str) {
    let dialog = gtk::FileDialog::builder().title("Send Files").modal(true).build();
    let ui = Rc::clone(ui);
    let peer_id = peer_id.to_string();

    dialog.open_multiple(Some(&ui.window.clone()), gio::Cancellable::NONE, move |result| {
        let Ok(files) = result else { return };
        let paths: Vec<PathBuf> = (0..files.n_items())
            .filter_map(|i| files.item(i))
            .filter_map(|o| o.downcast::<gio::File>().ok())
            .filter_map(|f| f.path())
            .collect();
        if !paths.is_empty() {
            ui.engine.send(Command::SendFiles { peer_id: peer_id.clone(), paths });
        }
    });
}

fn ask_about_request(
    ui: &Rc<Ui>,
    peer_id: String,
    peer_name: String,
    files: Vec<String>,
    total_size: i64,
) {
    let body = match files.len() {
        1 => format!("{} ({})", files[0], human_size(total_size)),
        n => format!("{n} files ({})", human_size(total_size)),
    };

    let dialog = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading(format!("{peer_name} wants to send you:"))
        .body(body)
        .build();
    dialog.add_response("decline", "Decline");
    dialog.add_response("accept", "Accept");
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("accept"));
    dialog.set_close_response("decline");

    let ui = Rc::clone(ui);
    dialog.connect_response(None, move |_, response| {
        ui.engine.send(Command::RespondToRequest {
            peer_id: peer_id.clone(),
            accept: response == "accept",
        });
    });
    dialog.present();
}

// MARK: formatting

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("a file")
        .to_string()
}

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value as i64, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn sizes_read_the_way_a_file_manager_shows_them() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1.0 kB");
        assert_eq!(human_size(2_500_000), "2.5 MB");
        assert_eq!(human_size(5_368_709_120), "5.4 GB");
        // A nonsense size must not panic or render as a negative.
        assert_eq!(human_size(-1), "0 B");
    }
}
