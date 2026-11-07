const COMMANDS: &[&str] = &[
    "check",
    "install",
    "prepare",
    "launch",
    "download_full",
    "install_full",
    "diagnose",
    "cancel",
    "reset_safety",
    "pending",
    "discard_pending",
    "install_pending",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
