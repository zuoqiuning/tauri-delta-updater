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
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
