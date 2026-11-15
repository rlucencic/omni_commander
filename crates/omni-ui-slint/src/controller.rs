//! Glue between the Slint window and the application core.
//!
//! Callbacks stay thin: they translate UI events into [`Command`]s. Updates
//! from the core are applied on the UI thread by a task running on Slint's
//! event loop. Everything here runs on the UI thread only.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use omni_core::app::{AppHandle, Command, PaneId, Update};
use omni_core::format::{TimeFormatter, format_count, format_size, format_totals};
use omni_core::keymap::{Action, Key, Keymap, Modifiers, Motion};
use omni_core::location::breadcrumbs;
use omni_core::pane::{Level, PaneStatus};
use omni_core::selection::SelectionSummary;
use omni_core::{Listing, SortKey, SortSpec};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tokio::sync::mpsc;

use crate::keys::chord_from_slint;
use crate::rows::RowsModel;
use crate::{AppWindow, MessageLevel, PaneInfo, SortColumn};

struct PaneView {
    rows: Rc<RowsModel>,
    location: RefCell<Option<PathBuf>>,
    /// Targets of the breadcrumbs currently shown, in display order.
    crumb_paths: RefCell<Vec<PathBuf>>,
    /// Row where the current run of Shift-selection started.
    anchor: Cell<Option<usize>>,
}

pub struct Controller {
    window: slint::Weak<AppWindow>,
    app: AppHandle,
    keymap: Keymap,
    panes: [PaneView; 2],
    /// Mirror of the core's active pane, used to route keyboard actions.
    active: Cell<PaneId>,
}

impl Controller {
    /// Creates the controller and connects it to `window`'s callbacks.
    pub fn attach(window: &AppWindow, app: AppHandle, keymap: Keymap) -> Rc<Self> {
        let time = TimeFormatter::system();
        let pane_view = || PaneView {
            rows: Rc::new(RowsModel::new(time.clone())),
            location: RefCell::new(None),
            crumb_paths: RefCell::new(Vec::new()),
            anchor: Cell::new(None),
        };
        let controller = Rc::new(Self {
            window: window.as_weak(),
            app,
            keymap,
            panes: [pane_view(), pane_view()],
            active: Cell::new(PaneId::Left),
        });

        window.set_left_rows(ModelRc::from(controller.panes[0].rows.clone()));
        window.set_right_rows(ModelRc::from(controller.panes[1].rows.clone()));

        let c = Rc::clone(&controller);
        window.on_key_pressed(move |text, control, alt, shift, meta| {
            let mods = Modifiers {
                ctrl: control,
                alt,
                shift,
                meta,
            };
            c.on_key(&text, mods)
        });
        let c = Rc::clone(&controller);
        window.on_cursor_changed(move |pane, row| {
            let pane = pane_id(pane);
            let snapshot = c.rows(pane).snapshot();
            c.app.send(Command::CursorMoved {
                pane,
                snapshot,
                row: to_row(row),
            });
        });
        let c = Rc::clone(&controller);
        window.on_row_activated(move |pane, row| c.open(pane_id(pane), to_row(row)));
        let c = Rc::clone(&controller);
        window.on_pane_clicked(move |pane| c.app.send(Command::Activate(pane_id(pane))));
        let c = Rc::clone(&controller);
        window.on_sort_requested(move |pane, column| {
            c.app.send(Command::SortBy {
                pane: pane_id(pane),
                key: sort_key(column),
            });
        });
        let c = Rc::clone(&controller);
        window.on_crumb_clicked(move |pane, index| {
            let pane = pane_id(pane);
            let target = c
                .view(pane)
                .crumb_paths
                .borrow()
                .get(to_row(index))
                .cloned();
            if let Some(path) = target {
                c.app.send(Command::Navigate { pane, path });
            }
        });
        let c = Rc::clone(&controller);
        window.on_location_entered(move |pane, text| {
            c.app.send(Command::NavigateToInput {
                pane: pane_id(pane),
                input: text.into(),
            });
        });
        let c = Rc::clone(&controller);
        window.on_go_back(move |pane| {
            c.app.send(Command::GoBack {
                pane: pane_id(pane),
            })
        });
        let c = Rc::clone(&controller);
        window.on_go_forward(move |pane| {
            c.app.send(Command::GoForward {
                pane: pane_id(pane),
            })
        });
        let c = Rc::clone(&controller);
        window.on_go_up(move |pane| {
            c.app.send(Command::GoUp {
                pane: pane_id(pane),
            })
        });
        let c = Rc::clone(&controller);
        window.on_row_pressed(move |pane, row, control, shift| {
            c.row_pressed(pane_id(pane), to_row(row), control, shift);
        });
        let c = Rc::clone(&controller);
        window.on_tab_selected(move |pane, index| {
            c.app.send(Command::SelectTab {
                pane: pane_id(pane),
                index: to_row(index),
            });
        });
        let c = Rc::clone(&controller);
        window.on_tab_closed(move |pane, index| {
            c.app.send(Command::CloseTab {
                pane: pane_id(pane),
                index: Some(to_row(index)),
            });
        });
        let c = Rc::clone(&controller);
        window.on_open_in_new_tab(move |pane, row| {
            let pane = pane_id(pane);
            c.app.send(Command::OpenInNewTab {
                pane,
                snapshot: c.rows(pane).snapshot(),
                row: to_row(row),
                background: true,
            });
        });
        let c = Rc::clone(&controller);
        window.on_filter_edited(move |pane, text| {
            c.app.send(Command::SetFilter {
                pane: pane_id(pane),
                text: text.into(),
            });
        });

        controller
    }

    /// Applies core updates until the core stops. Run with `slint::spawn_local`.
    pub async fn run_updates(self: Rc<Self>, mut updates: mpsc::Receiver<Update>) {
        // tokio's channels are runtime-agnostic, so awaiting them on Slint's
        // event loop is fine.
        while let Some(update) = updates.recv().await {
            self.apply(update);
        }
        tracing::debug!("update stream ended");
    }

    fn apply(&self, update: Update) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        match update {
            Update::Listing {
                pane,
                location,
                listing,
                sort,
                cursor,
                selection,
            } => {
                let view = self.view(pane);
                let new_location = view.location.borrow().as_ref() != Some(&location);
                let summary = listing_summary(&listing);
                view.rows.set_listing(listing, selection);
                view.anchor.set(None);
                let crumbs = breadcrumbs(&location);
                let labels: Vec<SharedString> =
                    crumbs.iter().map(|c| c.label.as_str().into()).collect();
                *view.crumb_paths.borrow_mut() = crumbs.into_iter().map(|c| c.path).collect();
                self.edit_info(&window, pane, |info| {
                    info.location = location.display().to_string().into();
                    info.crumbs = ModelRc::new(VecModel::from(labels));
                    info.summary = summary.into();
                    let (column, descending) = sort_column(sort);
                    info.sort_column = column;
                    info.sort_descending = descending;
                });
                *view.location.borrow_mut() = Some(location);
                window.invoke_set_cursor(pane_index(pane), to_index(cursor), new_location);
            }
            Update::Selection {
                pane,
                snapshot,
                selection,
                change,
            } => {
                self.rows(pane).set_selection(snapshot, selection, &change);
            }
            Update::Status { pane, status } => {
                self.edit_info(&window, pane, |info| apply_status(info, &status));
            }
            Update::Tabs {
                pane,
                titles,
                active,
            } => {
                let titles: Vec<SharedString> = titles.iter().map(|t| t.as_str().into()).collect();
                self.edit_info(&window, pane, |info| {
                    info.tabs = ModelRc::new(VecModel::from(titles));
                    info.active_tab = to_index(active);
                });
            }
            Update::ActivePane(pane) => {
                self.active.set(pane);
                window.set_active_pane(pane_index(pane));
            }
        }
    }

    fn on_key(&self, text: &str, mods: Modifiers) -> bool {
        let Some(chord) = chord_from_slint(text, mods) else {
            return false;
        };
        let Some(window) = self.window.upgrade() else {
            return false;
        };
        let pane = self.active.get();
        let Some(action) = self.keymap.lookup(chord) else {
            // Typing a character starts the quick filter.
            let plain = !(mods.ctrl || mods.alt || mods.meta);
            if plain && matches!(chord.key, Key::Char(_)) {
                window.invoke_start_filter(pane_index(pane), text.into());
                return true;
            }
            tracing::trace!(?chord, "unbound key");
            return false;
        };
        self.perform(&window, pane, action);
        true
    }

    fn perform(&self, window: &AppWindow, pane: PaneId, action: Action) {
        let index = pane_index(pane);
        let view = self.view(pane);
        match action {
            Action::Move(motion) => {
                view.anchor.set(None);
                move_cursor(window, index, motion);
            }
            Action::Select(motion) => {
                let anchor = view
                    .anchor
                    .get()
                    .unwrap_or_else(|| to_row(self.cursor(window, pane)));
                move_cursor(window, index, motion);
                view.anchor.set(Some(anchor));
                self.app.send(Command::SelectRange {
                    pane,
                    snapshot: view.rows.snapshot(),
                    anchor,
                    row: to_row(self.cursor(window, pane)),
                });
            }
            Action::ToggleSelection { advance } => {
                view.anchor.set(None);
                self.app.send(Command::ToggleSelection {
                    pane,
                    snapshot: view.rows.snapshot(),
                    row: to_row(self.cursor(window, pane)),
                });
                if advance {
                    window.invoke_move_cursor(index, 1);
                }
            }
            Action::SelectAll => {
                view.anchor.set(None);
                self.app.send(Command::SelectAll { pane });
            }
            Action::DeselectAll => {
                view.anchor.set(None);
                self.app.send(Command::DeselectAll { pane });
            }
            Action::Filter => window.invoke_start_filter(index, SharedString::new()),
            Action::ClearFilter => self.app.send(Command::SetFilter {
                pane,
                text: String::new(),
            }),
            Action::Open => self.open(pane, to_row(self.cursor(window, pane))),
            Action::GoUp => self.app.send(Command::GoUp { pane }),
            Action::GoBack => self.app.send(Command::GoBack { pane }),
            Action::GoForward => self.app.send(Command::GoForward { pane }),
            Action::EditLocation => window.invoke_edit_location(index),
            Action::SwitchPane => self.app.send(Command::SwitchPane),
            Action::NewTab => self.app.send(Command::NewTab { pane }),
            Action::OpenInNewTab => self.app.send(Command::OpenInNewTab {
                pane,
                snapshot: view.rows.snapshot(),
                row: to_row(self.cursor(window, pane)),
                background: false,
            }),
            Action::CloseTab => self.app.send(Command::CloseTab { pane, index: None }),
            Action::NextTab => self.app.send(Command::CycleTab {
                pane,
                forward: true,
            }),
            Action::PreviousTab => self.app.send(Command::CycleTab {
                pane,
                forward: false,
            }),
            Action::Refresh => self.app.send(Command::Refresh { pane }),
            Action::SortBy(key) => self.app.send(Command::SortBy { pane, key }),
        }
    }

    /// Left mouse button pressed on `row`. The UI moves the cursor there
    /// itself once the click completes.
    fn row_pressed(&self, pane: PaneId, row: usize, control: bool, shift: bool) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let view = self.view(pane);
        let snapshot = view.rows.snapshot();
        if control {
            view.anchor.set(None);
            self.app.send(Command::ToggleSelection {
                pane,
                snapshot,
                row,
            });
        } else if shift {
            let anchor = view
                .anchor
                .get()
                .unwrap_or_else(|| to_row(self.cursor(&window, pane)));
            view.anchor.set(Some(anchor));
            self.app.send(Command::SelectRange {
                pane,
                snapshot,
                anchor,
                row,
            });
        } else {
            view.anchor.set(None);
        }
    }

    fn open(&self, pane: PaneId, row: usize) {
        let snapshot = self.rows(pane).snapshot();
        self.app.send(Command::Open {
            pane,
            snapshot,
            row,
        });
    }

    fn view(&self, pane: PaneId) -> &PaneView {
        &self.panes[pane.index()]
    }

    fn rows(&self, pane: PaneId) -> &RowsModel {
        &self.view(pane).rows
    }

    fn cursor(&self, window: &AppWindow, pane: PaneId) -> i32 {
        match pane {
            PaneId::Left => window.get_left_cursor(),
            PaneId::Right => window.get_right_cursor(),
        }
    }

    fn edit_info(&self, window: &AppWindow, pane: PaneId, edit: impl FnOnce(&mut PaneInfo)) {
        let mut info = match pane {
            PaneId::Left => window.get_left_info(),
            PaneId::Right => window.get_right_info(),
        };
        edit(&mut info);
        match pane {
            PaneId::Left => window.set_left_info(info),
            PaneId::Right => window.set_right_info(info),
        }
    }
}

fn move_cursor(window: &AppWindow, pane: i32, motion: Motion) {
    match motion {
        Motion::Up => window.invoke_move_cursor(pane, -1),
        Motion::Down => window.invoke_move_cursor(pane, 1),
        Motion::PageUp => window.invoke_page_cursor(pane, -1),
        Motion::PageDown => window.invoke_page_cursor(pane, 1),
        Motion::First => window.invoke_set_cursor(pane, 0, false),
        Motion::Last => window.invoke_set_cursor(pane, i32::MAX, false),
    }
}

/// `3 folders, 12 files, 1.2 MiB`, plus how many entries a filter hides.
fn listing_summary(listing: &Listing) -> String {
    let mut summary = format_totals(&listing.totals());
    let (shown, total) = (listing.entry_count(), listing.entries().len());
    if shown < total {
        summary.push_str(&format!(
            "   (filter: {} of {} shown)",
            format_count(shown),
            format_count(total)
        ));
    }
    summary
}

/// `Selected: 2 folders, 3 files, 1.2 MiB`; empty when nothing is selected.
fn selection_summary(summary: &SelectionSummary) -> String {
    if summary.is_empty() {
        return String::new();
    }
    let plural = |n: usize, one: &str, many: &str| {
        format!("{} {}", format_count(n), if n == 1 { one } else { many })
    };
    let mut parts = Vec::new();
    if summary.dirs > 0 {
        parts.push(plural(summary.dirs, "folder", "folders"));
    }
    if summary.files > 0 {
        parts.push(plural(summary.files, "file", "files"));
        parts.push(format_size(summary.bytes));
    }
    format!("Selected: {}", parts.join(", "))
}

fn apply_status(info: &mut PaneInfo, status: &PaneStatus) {
    info.can_go_back = status.history.can_go_back;
    info.can_go_forward = status.history.can_go_forward;
    info.selection = selection_summary(&status.selection).into();
    info.filter = status.filter.as_str().into();
    info.loading = match &status.loading {
        Some(loading) => format!(
            "Loading… {} entries  ({})",
            format_count(loading.entries),
            loading.path.display()
        )
        .into(),
        None => Default::default(),
    };
    let (level, text) = match &status.message {
        Some(message) => {
            let level = match message.level {
                Level::Info => MessageLevel::Info,
                Level::Warning => MessageLevel::Warning,
                Level::Error => MessageLevel::Error,
            };
            (level, message.text.as_str().into())
        }
        None => (MessageLevel::None, Default::default()),
    };
    info.message_level = level;
    info.message = text;
}

fn pane_id(index: i32) -> PaneId {
    if index == 0 {
        PaneId::Left
    } else {
        PaneId::Right
    }
}

fn pane_index(pane: PaneId) -> i32 {
    match pane {
        PaneId::Left => 0,
        PaneId::Right => 1,
    }
}

/// Rows coming from Slint are `int`; negative values never denote a row.
fn to_row(row: i32) -> usize {
    usize::try_from(row).unwrap_or(0)
}

fn to_index(row: usize) -> i32 {
    i32::try_from(row).unwrap_or(i32::MAX)
}

fn sort_key(column: SortColumn) -> SortKey {
    match column {
        SortColumn::Name => SortKey::Name,
        SortColumn::Extension => SortKey::Extension,
        SortColumn::Size => SortKey::Size,
        SortColumn::Modified => SortKey::Modified,
    }
}

fn sort_column(sort: SortSpec) -> (SortColumn, bool) {
    let column = match sort.key {
        SortKey::Name => SortColumn::Name,
        SortKey::Extension => SortColumn::Extension,
        SortKey::Size => SortColumn::Size,
        SortKey::Modified => SortColumn::Modified,
    };
    (column, sort.descending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_summary_text() {
        assert_eq!(selection_summary(&SelectionSummary::default()), "");
        let summary = SelectionSummary {
            dirs: 1,
            files: 2,
            bytes: 2048,
        };
        assert_eq!(
            selection_summary(&summary),
            "Selected: 1 folder, 2 files, 2.0 KiB"
        );
        let dirs_only = SelectionSummary {
            dirs: 3,
            files: 0,
            bytes: 0,
        };
        assert_eq!(selection_summary(&dirs_only), "Selected: 3 folders");
    }
}
