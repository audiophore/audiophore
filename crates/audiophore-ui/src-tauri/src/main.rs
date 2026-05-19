// Prevents a console window from spawning on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
fn main() {
    audiophore_ui_lib::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("audiophore-ui is a macOS-only crate (M2 packaging spike).");
    eprintln!("Linux is locked to headless/engine-only per research-gui-framework.md §5.8.");
    std::process::exit(1);
}
