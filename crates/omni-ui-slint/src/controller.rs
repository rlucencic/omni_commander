//! Glue between the Slint window and the application core.
//!
//! Callbacks stay thin: they translate UI events into [`Command`]s. Updates
//! from the core are applied on the UI thread by a task running on Slint's
//! event loop. Everything here runs on the UI thread only.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use omni_core::app::{AppHandle, Command, PaneId, Update};
use omni_core::format::{TimeFormatter, format_count, format_totals};
use omni_core::keymap::{Action, Keymap, Modifiers};
use omni_core::location::breadcrumbs;
use omni_core::pane::{Level, PaneStatus};
use omni_core::{SortKey, SortSpec};
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
            c.on_key(
                &text,
                Modifiers {
                    ctrl: control,
                    alt,
                    shift,
                    meta,
                },
            )
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
            let target = c.panes[pane.index()]
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
            } => {
                let view = &self.panes[pane.index()];
                let new_location = view.location.borrow().as_ref() != Some(&location);
                let totals = listing.totals();
                view.rows.set_listing(listing);
                let crumbs = breadcrumbs(&location);
                let labels: Vec<SharedString> =
                    crumbs.iter().map(|c| c.label.as_str().into()).collect();
                *view.crumb_paths.borrow_mut() = crumbs.into_iter().map(|c| c.path).collect();
                self.edit_info(&window, pane, |info| {
                    info.location = location.display().to_string().into();
                    info.crumbs = ModelRc::new(VecModel::from(labels));
                    info.summary = format_totals(&totals).into();
                    let (column, descending) = sort_column(sort);
                    info.sort_column = column;
                    info.sort_descending = descending;
                });
                *view.location.borrow_mut() = Some(location);
                window.invoke_set_cursor(pane_index(pane), to_index(cursor), new_location);
            }
            Update::Status { pane, status } => {
                self.edit_info(&window, pane, |info| apply_status(info, &status));
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
        let Some(action) = self.keymap.lookup(chord) else {
            tracing::trace!(?chord, "unbound key");
            return false;
        };
        let Some(window) = self.window.upgrade() else {
            return false;
        };
        let pane = self.active.get();
        let index = pane_index(pane);
        match action {
            Action::CursorUp => window.invoke_move_cursor(index, -1),
            Action::CursorDown => window.invoke_move_cursor(index, 1),
            Action::CursorPageUp => window.invoke_page_cursor(index, -1),
            Action::CursorPageDown => window.invoke_page_cursor(index, 1),
            Action::CursorFirst => window.invoke_set_cursor(index, 0, false),
            Action::CursorLast => window.invoke_set_cursor(index, i32::MAX, false),
            Action::Open => self.open(pane, to_row(self.cursor(&window, pane))),
            Action::GoUp => self.app.send(Command::GoUp { pane }),
            Action::GoBack => self.app.send(Command::GoBack { pane }),
            Action::GoForward => self.app.send(Command::GoForward { pane }),
            Action::EditLocation => window.invoke_edit_location(index),
            Action::SwitchPane => self.app.send(Command::SwitchPane),
            Action::Refresh => self.app.send(Command::Refresh { pane }),
            Action::SortBy(key) => self.app.send(Command::SortBy { pane, key }),
        }
        true
    }

    fn open(&self, pane: PaneId, row: usize) {
        let snapshot = self.rows(pane).snapshot();
        self.app.send(Command::Open {
            pane,
            snapshot,
            row,
        });
    }

    fn rows(&self, pane: PaneId) -> &RowsModel {
        &self.panes[pane.index()].rows
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

fn apply_status(info: &mut PaneInfo, status: &PaneStatus) {
    info.can_go_back = status.history.can_go_back;
    info.can_go_forward = status.history.can_go_forward;
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
