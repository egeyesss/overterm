// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pty;

fn main() {
    tauri::Builder::default()
        .manage(pty::Sessions::default())
        .invoke_handler(tauri::generate_handler![
            pty::spawn_session,
            pty::write_pty,
            pty::resize_pty,
            pty::kill_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OverTerm");
}
