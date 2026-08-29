// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod choreograph;
mod hooks;
mod platform;
mod pty;
mod settings;

use tauri::Manager;
use tauri_plugin_global_shortcut::ShortcutState;

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

    // Summons or hides the overlay from any app.
    let toggle = settings::hotkey_or_default(&settings::load().hotkey);

    tauri::Builder::default()
        .manage(pty::Sessions::default())
        .manage({
            let stored = settings::load();
            let (width, height) =
                settings::sane_panel_size(stored.window.panel_width, stored.window.panel_height);
            Choreographer::new(stored.choreo(), width as f64, height as f64)
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    // Only ever one chord is registered: changing it
                    // releases the old one first. So anything arriving
                    // here is the summon key, whatever it currently is.
                    if event.state != ShortcutState::Pressed {
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
            // Somebody dragging a window edge is choosing a size, so it
            // is kept. macOS sends a great many of these during one drag,
            // so the write waits until the dragging stops.
            {
                let handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Resized(size) = event {
                        panel_resized(&handle, *size);
                    }
                });
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
            settings::dismiss_hooks_notice,
            settings::set_hotkey,
            settings::settings,
            settings::save_settings,
            settings::set_panel_size,
            settings::size_preset,
            app_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OverTerm");
}

/// Note a new window size and, once the dragging stops, store it.
///
/// A single drag produces a stream of these, so each one bumps a counter
/// and only the last one still holding it writes anything.
fn panel_resized(app: &tauri::AppHandle, size: tauri::PhysicalSize<u32>) {
    let choreo = app.state::<Choreographer>();
    let scale = app
        .get_webview_window(MAIN_WINDOW)
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0);
    let width = size.width as f64 / scale;
    let height = size.height as f64 / scale;
    if !choreo.remember_panel_size(width, height) {
        return;
    }

    static PENDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mine = PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        if PENDING.load(std::sync::atomic::Ordering::SeqCst) == mine {
            settings::save_panel_size(width.round() as u32, height.round() as u32);
        }
    });
}

/// The version this build reports.
///
/// Taken from the crate version, which a release checks against
/// `tauri.conf.json`, so it is the same number the bundle carries and the
/// same one somebody reading Info.plist would quote back.
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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
