// Prevents a console window from spawning on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    audiophore_ui_lib::run();
}
