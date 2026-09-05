//! Non-secret preferences, in a plain config file.
//!
//! Pairing secrets deliberately do *not* live here — they go in the platform credential
//! store (see `pairdrop-pairing`). Everything in this file is safe to read.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The instance to connect to. Empty by default: it must be configured by the user,
    /// and there is no sensible default to guess.
    pub server: String,
    /// The name other devices see. Empty means "use the hostname".
    pub display_name: String,
    pub download_directory: PathBuf,
    /// Only for an instance on your own network behind a self-signed certificate.
    pub allow_untrusted_tls: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server: String::new(),
            display_name: String::new(),
            download_directory: default_download_directory(),
            allow_untrusted_tls: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = config_path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = config_path() else {
            return Err(std::io::Error::other("no config directory"));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }

    /// What to announce over the data channel — the server can't derive a useful name
    /// for a native client, so this is the name peers actually see.
    pub fn effective_display_name(&self) -> String {
        if !self.display_name.trim().is_empty() {
            return self.display_name.trim().to_string();
        }
        hostname()
    }
}

fn config_path() -> Option<PathBuf> {
    // XDG first, falling back to ~/.config, which is what the spec says the default is.
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("pairdrop").join("config.json"))
}

fn default_download_directory() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()))
        .unwrap_or_else(|| "Linux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_leave_the_server_unset() {
        let settings = Settings::default();
        assert!(settings.server.is_empty(), "a server must be configured, never guessed");
        assert!(!settings.allow_untrusted_tls);
    }

    #[test]
    fn display_name_falls_back_to_the_hostname() {
        let mut settings = Settings::default();
        assert_eq!(settings.effective_display_name(), hostname());

        settings.display_name = "  Living Room Pi  ".into();
        assert_eq!(settings.effective_display_name(), "Living Room Pi");
    }

    /// A config written by an older build must still load rather than resetting
    /// everything to defaults.
    #[test]
    fn partial_config_keeps_known_fields() {
        let json = r#"{"server":"https://drop.example.com"}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.server, "https://drop.example.com");
        assert!(!settings.allow_untrusted_tls);
        assert_eq!(settings.download_directory, default_download_directory());
    }

    #[test]
    fn unknown_fields_do_not_break_loading() {
        let json = r#"{"server":"https://a.example","future_option":42}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.server, "https://a.example");
    }
}
