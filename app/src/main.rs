// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod choreograph;
mod hooks;
mod platform;
mod pty;
mod settings;

use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

use choreograph::Choreographer;
use platform::PlatformWindow;

pub const MAIN_WINDOW: &str = "main";

/// Flag a package manager calls while removing the app.
const UNINSTALL_HOOKS: &str = "--uninstall-hooks";

fn main() {
    // Nothing of ours runs when an app is dragged to the trash, so the
    // hook entries would outlive the thing that reads them. A package
    // manager can run this on the way out. No window, so it is answered
    // before anything starts.
    if std::env::args().any(|arg| arg == UNINSTALL_HOOKS) {
        match hooks::uninstall_on_removal() {
            Ok(true) => println!("removed the Claude Code hook entries"),
            Ok(false) => println!("no Claude Code hook entries to remove"),
            Err(e) => {
                eprintln!("could not remove the hook entries: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Summons or hides the overlay from any app. Configurable later.
    let toggle = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyO);

    tauri::Builder::default()
        .manage(pty::Sessions::default())
        .manage(Choreographer::new(settings::load().choreo()))
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state != ShortcutState::Pressed || shortcut != &toggle {
                        return;
                    }
                    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                        toggle_window(&window);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            // Before the window is shown, since it decides whether this
            // app owns a Space of its own.
            platform::make_accessory(app);
            let window = app
                .get_webview_window(MAIN_WINDOW)
                .expect("main window is defined in tauri.conf.json");
            if let Err(e) = platform::make_overlay(&window) {
                eprintln!("[platform] overlay setup failed: {e}");
            }
            settings::apply_to_window(&window);
            // Detection works without these; they make it exact for the
            // tools that report. Never fatal: a settings file we cannot
            // read or write leaves the fallback detector in charge.
            match hooks::install_on_first_run() {
                Ok(true) => eprintln!("[hooks] installed"),
                Ok(false) => {}
                Err(e) => eprintln!("[hooks] not installed: {e}"),
            }
            // A taken hotkey fails to register; the app is still usable, so
            // report it instead of refusing to start.
            if let Err(e) =
                tauri_plugin_global_shortcut::GlobalShortcutExt::global_shortcut(app.handle())
                    .register(toggle)
            {
                eprintln!("[hotkey] could not register the toggle shortcut: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pty::spawn_session,
            pty::write_pty,
            pty::resize_pty,
            pty::kill_session,
            choreograph::set_window_mode,
            choreograph::window_mode,
            choreograph::hide_window,
            hooks::install_hooks,
            hooks::uninstall_hooks,
            hooks::hooks_installed,
            settings::settings,
            settings::save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OverTerm");
}

/// Hide the overlay if it is showing, otherwise bring it up ready to type.
/// The hotkey is a deliberate summon, so taking focus here is wanted; the
/// agent-driven expand in the choreography is what must not.
fn toggle_window<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    // Visible is not the same as reachable. A window sitting on another
    // Space reports itself visible, so keying the toggle off that alone
    // meant the first press hid a window the user could not see and was
    // asking for.
    let showing = window.is_visible().unwrap_or(false) && window.is_on_active_space();
    let result = if showing {
        window.hide()
    } else {
        window.show().and_then(|()| window.set_focus())
    };
    if let Err(e) = result {
        eprintln!("[hotkey] toggle failed: {e}");
    }
}
