//! PairDrop for Linux.
//!
//! A small window listing nearby devices; drop files on one to send. All the networking
//! lives in `pairdrop-client`, on its own thread — this file is the GTK main loop and
//! nothing else.

mod preferences;
mod window;

use adw::prelude::*;
use pairdrop_client::{Engine, Settings};

const APP_ID: &str = "app.pairdrop.Linux";

fn main() -> gtk::glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();

    application.connect_startup(|_| {
        adw::init().expect("libadwaita failed to initialise");
    });

    application.connect_activate(|application| {
        // Re-activating (a second launch, or the desktop file being opened again)
        // should raise the window we already have rather than build another.
        if let Some(existing) = application.active_window() {
            existing.present();
            return;
        }

        let settings = Settings::load();
        let (engine, events) = Engine::start(settings.clone());
        window::build(application, engine, events, settings);
    });

    application.run()
}
