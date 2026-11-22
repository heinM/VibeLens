//! Window/app lifecycle: never lose unsaved editor tabs to a stray ⌘W, ⌘Q or close button.
//!
//! The frontend reports how many tabs have unsaved changes (`app_set_unsaved`). Close and quit
//! requests are intercepted **in Rust** and answered with a native dialog, so the guard still works
//! when the web UI is wedged or has crashed (a JS `onCloseRequested` handler would not, and would also
//! need the `core:window:allow-destroy` permission).
//!
//! macOS menu: the default Tauri menu with two changes, matching editors like VS Code / DataGrip:
//! - ⌘W closes the active **tab** (sent to the UI as `vg://menu` `"close-tab"`); ⇧⌘W closes the window.
//! - ⌘Q is our own item so it can be guarded (the predefined Quit calls `terminate:` directly, which
//!   tao does not let us intercept).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tauri::{AppHandle, Emitter, Manager, Runtime, Window, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Event the UI listens on for menu actions it owns.
pub const MENU_EVENT: &str = "vg://menu";
const ID_QUIT: &str = "vg-quit";
const ID_CLOSE_TAB: &str = "vg-close-tab";
const ID_CLOSE_WINDOW: &str = "vg-close-window";

#[derive(Default)]
pub struct Lifecycle {
    unsaved: AtomicUsize,
    prompting: AtomicBool,
}

/// Called by the UI whenever the number of dirty tabs changes.
#[tauri::command]
pub fn app_set_unsaved(state: tauri::State<'_, Lifecycle>, count: usize) {
    state.unsaved.store(count, Ordering::Relaxed);
}

fn prompt_text(n: usize, verb: &str) -> String {
    let tabs = if n == 1 {
        "1 tab has".to_string()
    } else {
        format!("{n} tabs have")
    };
    format!(
        "{tabs} unsaved changes. {verb} anyway? The text is restored the next time VibeLens opens (tabs over 1 MiB excepted), but it is not saved to a file."
    )
}

/// Ask before discarding unsaved tabs; `proceed` runs on confirmation (or immediately when clean).
fn guard<R: Runtime>(
    app: &AppHandle<R>,
    verb: &'static str,
    proceed: impl FnOnce() + Send + 'static,
) -> bool {
    let lc = app.state::<Lifecycle>();
    let n = lc.unsaved.load(Ordering::Relaxed);
    if n == 0 {
        proceed();
        return false;
    }
    // A second ⌘Q while the dialog is up must not stack dialogs (or slip past the first one).
    if lc.prompting.swap(true, Ordering::AcqRel) {
        return true;
    }
    let handle = app.clone();
    app.dialog()
        .message(prompt_text(n, verb))
        .title("Unsaved changes")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            format!("{verb} without saving"),
            "Cancel".into(),
        ))
        .show(move |ok| {
            handle
                .state::<Lifecycle>()
                .prompting
                .store(false, Ordering::Release);
            if ok {
                proceed();
            }
        });
    true
}

/// `Builder::on_window_event` hook.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        let w = window.clone();
        let prevented = guard(window.app_handle(), "Close", move || {
            let _ = w.destroy();
        });
        if prevented {
            api.prevent_close();
        }
    }
}

/// `Builder::on_menu_event` hook.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        ID_QUIT => {
            let h = app.clone();
            guard(app, "Quit", move || h.exit(0));
        }
        ID_CLOSE_TAB => {
            let _ = app.emit(MENU_EVENT, "close-tab");
        }
        // Goes through `Window::close`, which emits `CloseRequested` and so hits the guard.
        ID_CLOSE_WINDOW => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.close();
            }
        }
        _ => {}
    }
}

/// The app menu. Only macOS shows an app menu by default; elsewhere we keep the platform default.
#[cfg(target_os = "macos")]
pub fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem as P, Submenu};
    let pkg = app.package_info();
    let about = AboutMetadata {
        name: Some(pkg.name.clone()),
        version: Some(pkg.version.to_string()),
        ..Default::default()
    };
    let app_menu = Submenu::with_items(
        app,
        pkg.name.clone(),
        true,
        &[
            &P::about(app, None, Some(about))?,
            &P::separator(app)?,
            &P::services(app, None)?,
            &P::separator(app)?,
            &P::hide(app, None)?,
            &P::hide_others(app, None)?,
            &P::show_all(app, None)?,
            &P::separator(app)?,
            &MenuItem::with_id(
                app,
                ID_QUIT,
                format!("Quit {}", pkg.name),
                true,
                Some("CmdOrCtrl+Q"),
            )?,
        ],
    )?;
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &MenuItem::with_id(app, ID_CLOSE_TAB, "Close Tab", true, Some("CmdOrCtrl+W"))?,
            &MenuItem::with_id(
                app,
                ID_CLOSE_WINDOW,
                "Close Window",
                true,
                Some("CmdOrCtrl+Shift+W"),
            )?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &P::undo(app, None)?,
            &P::redo(app, None)?,
            &P::separator(app)?,
            &P::cut(app, None)?,
            &P::copy(app, None)?,
            &P::paste(app, None)?,
            &P::select_all(app, None)?,
        ],
    )?;
    let view = Submenu::with_items(app, "View", true, &[&P::fullscreen(app, None)?])?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[&P::minimize(app, None)?, &P::maximize(app, None)?],
    )?;
    Menu::with_items(app, &[&app_menu, &file, &edit, &view, &window])
}

#[cfg(test)]
mod tests {
    use super::prompt_text;

    #[test]
    fn wording() {
        assert_eq!(
            prompt_text(1, "Quit"),
            "1 tab has unsaved changes. Quit anyway? The text is restored the next time VibeLens opens (tabs over 1 MiB excepted), but it is not saved to a file."
        );
        assert!(prompt_text(3, "Close").starts_with("3 tabs have"));
    }
}
