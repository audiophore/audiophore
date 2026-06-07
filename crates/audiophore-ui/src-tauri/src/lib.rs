#![cfg(target_os = "macos")]

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // C.1.b spike: updater checks the test feed + verifies its minisign
        // signature against the pubkey in tauri.conf.json; process exposes the
        // relaunch command used after an update is applied.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|_app| Ok(()))
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
