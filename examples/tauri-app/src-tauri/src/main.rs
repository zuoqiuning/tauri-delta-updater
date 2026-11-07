// Minimal host: the plugin reads its config from tauri.conf.json.
// Edit `plugins.delta-updater` there to point at a real release.
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_updater::init_from_config())
        .run(tauri::generate_context!())
        .expect("failed to run tauri application");
}
