# CLAUDE.md

Working notes for AI-assisted development of OmniCommander: what the project is, how to
work in it, what was learned the hard way, where it stands, and what comes next.
User-facing documentation is in `README.md` and `docs/`.

## Project

A cross-platform dual-pane file manager in Rust with a Slint UI, in the spirit of Total
Commander / Double Commander with modern navigation. Linux (Zorin OS 18.1, X11, NVIDIA
RTX 3060 Laptop + Radeon iGPU) is the development platform; Windows and macOS are
first-class targets but have not been built yet.

## Commands

```sh
cargo run -p omni-ui-slint [-- LEFT RIGHT]       # debug run (deps are opt-level 2)
cargo build --release -p omni-ui-slint           # target/release/omnicommander
cargo test --workspace                           # ~115 tests, no GUI needed
cargo clippy --workspace --all-targets           # must stay warning-free
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo run --release -p omni-core --example bench_listing
cargo run --release -p omni-vfs --example enumerate -- DIR
OMNI_CONFIG_DIR=/tmp/omni-test cargo run -p omni-ui-slint   # scratch settings/session
OMNI_LOG=info,omni_core=debug ...                # loads, failures, timings
```

## Layout

| Crate | Responsibility |
| --- | --- |
| `crates/omni-vfs` | `FileSystem` trait, `Capabilities`, `Entry`, `DirStream`, `LocalFs` (blocking pool, 512-entry batches, bounded channel, drop = cancel), `child_path` (rejects `..`, `a/b`) |
| `crates/omni-core` | No GUI. `app.rs`: the actor (commands in, updates out, tabs per side). `pane.rs`: the `PaneState` state machine (loads, history, selection, filter, hidden files). `listing.rs`: immutable snapshots. `sort.rs`: natural-sort key arena. `selection.rs`, `filter.rs`, `location.rs`, `keymap.rs`, `format.rs` |
| `crates/omni-config` | `settings.toml` (user-owned, never rewritten) and `session.toml` (app-written, atomic) formats; lenient per-section parsing with warnings |
| `crates/omni-ui-slint` | Binary `omnicommander`. `main.rs` startup/shutdown, `startup.rs` config ↔ core mapping, `controller.rs` UI glue, `rows.rs` lazy Slint model, `keys.rs` key translation, `ui/*.slint` |
| `tools/ui-sandbox` | `xdrive.py` (drive a nested X server: keys, clicks, typing, screenshots, close), `cursor.py` |

## Rules of the architecture

- The UI thread never touches the filesystem. Callbacks translate events into `Command`s.
- All state lives in the core actor. No `Arc<Mutex<…>>`; communicate by messages.
- `PaneState` does no I/O. It returns `Changes`, which the actor carries out. Put new
  pane logic there and unit-test it.
- Rows are referenced by `(SnapshotId, row)`, enumerations by `LoadToken`, tabs by
  `TabId`. Stale ones are ignored, never applied to the wrong thing.
- Selection is a bit set by *entry index*, which is stable across sorting and filtering.
  Hidden-by-filter means deselected.
- Only the active tab is published; activating a tab republishes its whole state.
- Keyboard shortcuts are data (`Keymap`, `Action::ALL` names); never hard-code keys in
  `.slint` files.
- `unsafe` is denied workspace-wide. Don't create crates or modules ahead of need.

## Hard-won lessons (read before touching these areas)

- **NVIDIA + OpenGL hides the mouse cursor** over the window (proprietary driver 580,
  X11). It is not caused by the transparent window: the software renderer and Mesa's
  OpenGL are fine. Hence the default renderer is FemtoVG on **wgpu/Vulkan**
  (`renderer-femtovg-wgpu`), with software fallback. Don't switch the default back to
  OpenGL.
- **Slint layouts propagate size constraints.** A `Flickable` without an explicit
  `width` forwards its content's preferred width; columns, tabs and breadcrumbs that
  depend on the pane width cause binding loops or widen the pane. Use explicit geometry
  (`x`/`width` computed from a struct like `Columns`) for anything inside the list area,
  and give Flickables explicit `width`/`height`.
- **Slint name clashes:** `Rectangle` already has `color`, and every element has `row`
  (grid layouts). Use other property names.
- `TextInput`/`LineEdit` `key-pressed` runs *before* the input handles the key. Keys it
  does not use bubble to ancestors, including the global shortcut `FocusScope`, so
  wrap editors in a `FocusScope` that accepts everything. To put the caret at the end
  after setting text: `select-all()` and then `clear-selection()`.
- `changed <property> => { … }` handlers exist in Slint 1.18; `Palette.color-scheme =
  ColorScheme.unknown` follows the system theme again.
- Tokio's sync channels are runtime-agnostic: the UI awaits core updates inside
  `slint::spawn_local`. Tokio *timers* are not: create `tokio::time::timeout` inside the
  runtime (`block_on(async { timeout(..).await })`), or it panics.
- **Messages:** arriving in a different folder clears the pane message; a load of the
  same folder only replaces it when it has warnings of its own (startup warnings
  arrive while the first load is still running).
- **Integration tests on the actor race** with background loads (duplicated tabs
  refresh, background tabs load). Wait for a specific state (`listing_at`,
  `expect_tabs`), not for "the next update".
- **UI verification without touching the desktop:** run the app in `Xephyr :5`, give
  it focus with `xdrive.py focus:OmniCommander`, and drive it. Xephyr may not show the
  mouse cursor; that is Xephyr, not the app. `close:OmniCommander` exits normally, so
  the session is saved. Use `OMNI_CONFIG_DIR` so the user's config is untouched. Final
  checks happen on the real desktop by the user.

## How we work

One slice at a time, then:

1. `cargo fmt`, clippy, tests and rustdoc are clean.
2. Verify in the Xephyr sandbox with screenshots.
3. Update `README.md`, `docs/`, and this file.
4. Build release and start it on the user's desktop (`DISPLAY=:1`) for them to test.
5. Commit on the milestone branch after the user confirms. Commit messages: a summary
   line `Milestone N, slice M: …`, a body of bullet points, and a `Co-Authored-By`
   trailer.

## Status — 2026-11-15

Done today:

- **Milestone 1 (foundation):**
  - workspace; local VFS with streaming, cancellable enumeration;
  - core actor with snapshot ids and load tokens; two virtualized panes; keyboard
    navigation; sorting with precomputed natural-sort keys (100k names in ~30 ms release);
  - adaptive throttle for partial listings;
  - wgpu renderer (NVIDIA cursor bug); benchmarks.
- **Milestone 2, slices 1–4 (on branch `milestone-2`):**
  1. back/forward history restoring the cursor, breadcrumbs, editable location (`~`,
     relative paths, file paths open their folder);
  2. multi-selection (Insert/Space/Shift ranges/Ctrl+A/Ctrl/Shift+click) and the quick
     filter (substring, wildcards, `;` alternatives);
  3. per-pane tabs (Ctrl+T/W/Tab, Ctrl+↑, middle-click, close buttons);
  4. `omni-config`: settings with configurable shortcuts, theme and renderer, session
     restore (tabs, sorts, active pane), hidden-files toggle (Ctrl+H), startup warnings
     shown in the pane.

## Next steps

1. **Milestone 2, slice 5: automatic refresh.**
   - Add `notify` and watch the folder of each *shown* tab (the active tab of each side;
     background tabs refresh on activation).
   - Debounce bursts (~200 ms) and reuse `PaneState::refresh()`, which keeps cursor,
     selection and filter.
   - Ignore events for superseded locations. Watch errors (inotify limits, network
     mounts) degrade to manual refresh with a one-time notice.
   - The watcher belongs in `omni-vfs`, as an optional provider capability (`Capabilities`
     gets a `watch` flag), not in the UI.
2. Close Milestone 2: merge `milestone-2` into `master` (or open a PR), and update the roadmap.
3. **Milestone 3: file operations.**
   - New crate `omni-operations`: a job queue on Tokio with progress events, cancellation,
     and conflict policies (ask/skip/overwrite/rename).
   - Copy and move use copy+verify+delete across filesystems. Also delete, rename and
     mkdir; trash through a platform adapter (`omni-platform`).
   - The operations act on `PaneState` selection (or the cursor entry); F5/F6/F7/F8 bindings;
     progress dialog and queue view in the UI.
   - Never overwrite without the chosen policy; test against temp dirs, including
     failure injection.
4. Opening files with the default application (Enter on a file): platform adapter
   (`xdg-open`/`gio` on Linux, `open` on macOS, `ShellExecute` on Windows) with no shell
   string building.
5. Housekeeping:
   - choose a license (Slint's licensing affects distribution);
   - add CI (GitHub Actions: fmt, clippy, test on Linux; build on Windows and macOS);
   - `rustup target add x86_64-pc-windows-msvc aarch64-apple-darwin` and
     `cargo check -p omni-vfs -p omni-core` to compile the `cfg(windows)`/macOS paths.

Known limitations are tracked per slice in `docs/roadmap.md`.
