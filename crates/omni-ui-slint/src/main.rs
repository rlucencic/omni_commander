//! OmniCommander desktop application (Slint front end).
//!
//! Usage: `omnicommander [LEFT_DIR] [RIGHT_DIR]`
//!
//! Settings live in `settings.toml` and the last session in `session.toml`
//! (see the `omni-config` crate for their location). Logging goes to stderr;
//! set `OMNI_LOG` (e.g. `OMNI_LOG=debug`) using the
//! `tracing_subscriber::EnvFilter` syntax to change verbosity.

mod controller;
mod keys;
mod rows;
mod startup;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use omni_config::{ConfigDir, Renderer, Settings, Theme};
use omni_core::app::{self, AppHandle, Command, PaneId};
use omni_core::keymap::{Action, Keymap};
use omni_core::pane::Level;
use omni_vfs::{FileSystem, LocalFs};
use slint::ComponentHandle;
use tracing_subscriber::EnvFilter;

use crate::controller::Controller;
use crate::startup::{CommandLine, Defaults};

/// Code generated from `ui/*.slint`.
mod ui {
    // Generated types do not derive `Debug`.
    #![allow(missing_debug_implementations)]
    slint::include_modules!();
}
pub use ui::*;

fn main() -> anyhow::Result<()> {
    init_logging();
    let mut warnings = Vec::new();

    let config_dir = ConfigDir::locate();
    if config_dir.is_none() {
        warnings
            .push("no configuration directory found; settings and session are not saved".into());
    }
    let action_names: Vec<&str> = Action::ALL.iter().map(|(name, _)| *name).collect();
    let settings = match &config_dir {
        Some(dir) => take(dir.load_settings(&action_names), &mut warnings),
        None => Settings::default(),
    };
    let session = match &config_dir {
        Some(dir) if settings.panes.restore_session => take(dir.load_session(), &mut warnings),
        _ => None,
    };

    // Must happen before the first window exists.
    if let Err(err) = select_renderer(settings.appearance.renderer) {
        warnings.push(format!(
            "renderer \"{:?}\" is not available ({err}); using the default",
            settings.appearance.renderer
        ));
    }

    let mut keymap = Keymap::commander_defaults();
    let overrides = settings
        .keys
        .iter()
        .map(|(name, keys)| (name.as_str(), keys.as_slice()));
    warnings.extend(
        keymap
            .apply_overrides(overrides)
            .into_iter()
            .map(|w| format!("[keys] {w}")),
    );

    let fs: Arc<dyn FileSystem> = Arc::new(LocalFs::new());
    let config = startup::app_config(
        fs,
        &settings,
        session.as_ref(),
        &command_line(),
        &defaults(),
    );
    tracing::info!(
        config_dir = ?config_dir.as_ref().map(ConfigDir::path),
        left = %config.left.tabs[config.left.active_tab].path.display(),
        right = %config.right.tabs[config.right.active_tab].path.display(),
        "starting OmniCommander"
    );

    // Filesystem work runs on Tokio; the main thread belongs to Slint.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("omni-worker")
        .enable_time()
        .build()
        .context("failed to start the async runtime")?;

    let window = AppWindow::new().context("failed to create the main window")?;
    window.set_theme(match settings.appearance.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });

    let (app, updates, core) = app::spawn(runtime.handle(), config);
    report(&app, &warnings);

    let controller = Controller::attach(&window, app.clone(), keymap);
    let update_loop = slint::spawn_local(controller.run_updates(updates))
        .context("failed to start the UI update loop")?;

    window.run().context("event loop failed")?;

    update_loop.abort();
    if let Some(dir) = &config_dir {
        save_session(&runtime, &app, dir);
    }
    // Stop the core and give background work a moment. Blocking reads stuck
    // on an unresponsive mount are abandoned rather than awaited.
    core.abort();
    runtime.shutdown_timeout(Duration::from_millis(500));
    tracing::info!("bye");
    Ok(())
}

/// Log filter used when `OMNI_LOG` is not set.
///
/// The GPU stack (wgpu) logs adapter details at `info` and warns about a
/// buffer feature that Slint's renderer requests on purpose; neither is
/// actionable for users, so wgpu only reports errors by default.
const DEFAULT_LOG_FILTER: &str = "info,wgpu_core=error,wgpu_hal=error,naga=error";

fn init_logging() {
    let filter =
        EnvFilter::try_from_env("OMNI_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_thread_names(true)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
}

fn take<T>(loaded: omni_config::Loaded<T>, warnings: &mut Vec<String>) -> T {
    warnings.extend(loaded.warnings);
    loaded.value
}

/// Logs every warning and shows the first one in the left pane, so a broken
/// setting never goes unnoticed.
fn report(app: &AppHandle, warnings: &[String]) {
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    if let Some(first) = warnings.first() {
        let more = match warnings.len() - 1 {
            0 => String::new(),
            n => format!(" (and {n} more; see the log)"),
        };
        app.send(Command::ShowMessage {
            pane: PaneId::Left,
            level: Level::Warning,
            text: format!("{first}{more}"),
        });
    }
}

/// Applies the renderer setting, unless `SLINT_BACKEND` already decides.
fn select_renderer(renderer: Renderer) -> Result<(), slint::PlatformError> {
    let name = match renderer {
        Renderer::Auto => return Ok(()),
        Renderer::Gpu => "femtovg-wgpu",
        Renderer::Opengl => "femtovg",
        Renderer::Software => "software",
    };
    if std::env::var_os("SLINT_BACKEND").is_some() {
        tracing::info!("SLINT_BACKEND is set; ignoring the renderer setting");
        return Ok(());
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(name.into())
        .select()
}

/// Asks the core what is open and writes it to `session.toml`.
fn save_session(runtime: &tokio::runtime::Runtime, app: &AppHandle, dir: &ConfigDir) {
    let (reply, snapshot) = tokio::sync::oneshot::channel();
    app.send(Command::Snapshot(reply));
    // The timer must be created inside the runtime, hence the async block.
    let snapshot =
        runtime.block_on(async { tokio::time::timeout(Duration::from_secs(2), snapshot).await });
    let Ok(Ok(snapshot)) = snapshot else {
        tracing::warn!("the core did not report its state; the session is not saved");
        return;
    };
    let (session, warnings) = startup::session_from(&snapshot);
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    match dir.save_session(&session) {
        Ok(()) => tracing::debug!(path = %dir.session_path().display(), "session saved"),
        Err(err) => tracing::warn!(%err, "cannot save the session"),
    }
}

fn command_line() -> CommandLine {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    CommandLine {
        left: args.next().map(|p| resolve(&p)),
        right: args.next().map(|p| resolve(&p)),
    }
}

/// The current directory (left) and the home directory (right).
fn defaults() -> Defaults {
    let cwd = std::env::current_dir().ok();
    let home = std::env::home_dir();
    let root = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
    Defaults {
        left: cwd
            .clone()
            .or_else(|| home.clone())
            .unwrap_or_else(|| root.clone()),
        right: home.or(cwd).unwrap_or(root),
    }
}

/// Makes a user-supplied path absolute without failing: a path that cannot
/// be resolved is still shown, and the pane reports why it cannot be listed.
fn resolve(path: &Path) -> PathBuf {
    // On Windows `canonicalize` yields `\\?\` verbatim paths, which are
    // unfriendly to display; `absolute` normalizes without them.
    #[cfg(not(windows))]
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}
