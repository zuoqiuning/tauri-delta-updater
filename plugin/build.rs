const COMMANDS: &[&str] = &[
    "check",
    "install",
    "download_full",
    "install_full",
    "diagnose",
    "cancel",
    "reset_safety",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
