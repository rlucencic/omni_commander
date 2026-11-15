//! OmniCommander desktop application (Slint front end).
//!
//! Usage: `omnicommander [LEFT_DIR] [RIGHT_DIR]`
//!
//! Logging goes to stderr; set `OMNI_LOG` (e.g. `OMNI_LOG=debug`) using the
//! `tracing_subscriber::EnvFilter` syntax to change verbosity.

mod controller;
mod keys;
mod rows;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use omni_core::SortSpec;
use omni_core::app::{self, AppConfig, PaneConfig};
use omni_core::keymap::Keymap;
use omni_vfs::{FileSystem, LocalFs};
use slint::ComponentHandle;
use tracing_subscriber::EnvFilter;

use crate::controller::Controller;

/// Code generated from `ui/*.slint`.
mod ui {
    // Generated types do not derive `Debug`.
    #![allow(missing_debug_implementations)]
    slint::include_modules!();
}
pub use ui::*;

fn main() -> anyhow::Result<()> {
    init_logging();
    let (left, right) = start_directories();
    tracing::info!(left = %left.display(), right = %right.display(), "starting OmniCommander");

    // Filesystem work runs on Tokio; the main thread belongs to Slint.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("omni-worker")
        .enable_time()
        .build()
        .context("failed to start the async runtime")?;

    let window = AppWindow::new().context("failed to create the main window")?;

    let fs: Arc<dyn FileSystem> = Arc::new(LocalFs::new());
    let config = AppConfig {
        left: PaneConfig {
            fs: Arc::clone(&fs),
            path: left,
        },
        right: PaneConfig { fs, path: right },
        sort: SortSpec::default(),
    };
    let (app, updates, core) = app::spawn(runtime.handle(), config);

    let controller = Controller::attach(&window, app, Keymap::commander_defaults());
    let update_loop = slint::spawn_local(controller.run_updates(updates))
        .context("failed to start the UI update loop")?;

    window.run().context("event loop failed")?;

    // Orderly shutdown: stop feeding the UI, stop the core, and give
    // background work a moment. Blocking reads stuck on an unresponsive
    // mount are abandoned rather than awaited.
    update_loop.abort();
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

/// Directories for the two panes: from the command line, otherwise the
/// current directory (left) and the home directory (right).
fn start_directories() -> (PathBuf, PathBuf) {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let home = std::env::home_dir();
    let fallback = || {
        home.clone()
            .or_else(|| cwd.clone())
            .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR_STR))
    };
    let left = args
        .next()
        .map(|p| resolve(&p))
        .or_else(|| cwd.clone())
        .unwrap_or_else(fallback);
    let right = args.next().map(|p| resolve(&p)).unwrap_or_else(fallback);
    (left, right)
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
