/// XDG-like config/data directory helpers.
use std::path::PathBuf;

/// `$XDG_CONFIG_HOME/dip`  (falls back to `~/.config/dip`)
pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        });
    base.join(crate::config::BIN_NAME)
}

/// `~/.config/dip/proxy`
pub fn proxy_dir() -> PathBuf {
    config_dir().join("proxy")
}
