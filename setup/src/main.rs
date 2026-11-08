//! The setup program and uninstaller. See `live_listen_setup::run` and `docs/INSTALLATION.md`.

#![windows_subsystem = "windows"]

fn main() {
    std::process::exit(live_listen_setup::run());
}
