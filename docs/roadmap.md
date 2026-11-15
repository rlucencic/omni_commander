# Roadmap

## Milestone 1: functional foundation ✅

- [x] Cargo workspace (`omni-vfs`, `omni-core`, `omni-ui-slint`)
- [x] Slint main window with two independent file panes
- [x] Asynchronous, cancellable local directory enumeration with backpressure
- [x] Metadata display: size, modification time, permissions / attributes, link targets
- [x] Directory navigation (enter, parent, `..` row) with cursor restoration
- [x] Keyboard navigation through a toolkit-independent keymap
- [x] Active-pane switching (Tab, mouse)
- [x] Sorting by name / extension / size / date, directories first, natural order
- [x] Error reporting in the pane and through `tracing` logs
- [x] GPU rendering via FemtoVG on wgpu/Vulkan, with automatic software fallback
  (works around an invisible-cursor bug in NVIDIA's OpenGL driver)
- [x] Unit and integration tests for the core (57 tests), no GUI required

### Known limitations of Milestone 1

- Opening files (Enter on a file) only shows a notice; launching files with their
  associated applications needs platform integration (Milestone 2/3).
- No automatic refresh when the directory changes on disk; use Ctrl+R. (Planned:
  `notify`-based watching.)
- No free-space information in the status bar yet.
- Column widths are fixed; narrow panes hide the date and attribute columns.
- Name sorting is locale-independent (natural, case-insensitive), without full
  Unicode collation.
- With non-Latin keyboard layouts, Ctrl+letter shortcuts follow the layout's character,
  not the physical key.
- All metadata is read eagerly during enumeration. That is fast locally, but slow network
  mounts would benefit from lazy metadata.
- Sorting runs inside the app actor. 100 000 entries take tens of milliseconds in release
  builds; for millions of entries it should move to a blocking task.
- The renderer can only be chosen with `SLINT_BACKEND`; a renderer setting belongs in
  the Milestone 2 settings. Slint does not report which renderer it fell back to.
- Only Linux (X11) has been run. The code has `cfg` paths for Windows and macOS, but
  they have not been compiled for those targets yet.

## Milestone 2: navigation and usability (in progress)

Delivered in slices, each tested before the next:

1. Navigation ✅
   - [x] Back/forward history per pane, restoring the cursor (Alt+←/→, mouse buttons)
   - [x] Breadcrumbs with back/forward/up buttons
   - [x] Editable location (Ctrl+L): `~`, relative paths, file paths open their folder
2. Selection and filtering ✅
   - [x] Multi-selection: Insert, Space, Shift+arrows, Ctrl+A, Ctrl/Shift+click
   - [x] Selection summary in the status line
   - [x] Quick filter: type to filter, wildcards, `;` alternatives
3. Tabs
   - [ ] Per-pane tabs: Ctrl+T, Ctrl+W, Ctrl+Tab, tab bar
4. Settings
   - [ ] `omni-config` crate: TOML settings in the platform config directory
   - [ ] Restore folders/tabs and sort order on start
   - [ ] Configurable shortcuts
   - [ ] Light/dark/system theme, renderer choice, hidden-file toggle
   - [ ] Automatic refresh with `notify`

Known limitations of slice 2: changing the filter deselects entries it hides, and they
stay deselected when the filter is removed (deliberate: no invisible selection). There is
no invert-selection or select-by-pattern command yet.

Known limitations of slice 1: breadcrumbs are cut off on the left without an
ellipsis, and editing the text of a path whose name is not valid UTF-8 cannot reproduce
the original bytes (navigating via breadcrumbs or the list still works).

## Milestone 3: file operations

`omni-operations` crate: copy, move (copy + verify + delete across filesystems), rename,
delete and trash, create directory, conflict policies, progress, background queue,
cancellation.

## Milestone 4: advanced features

Miller columns, thumbnails, previews, archive browsing, search, batch rename,
directory comparison.

## Milestone 5: cross-platform release

Windows and macOS builds and installers, Linux DEB/AppImage, platform integration
tests, high-DPI and drag-and-drop verification, release automation.
