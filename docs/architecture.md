# Architecture

## Crates

```text
omni-ui-slint  (binary: omnicommander)  Slint UI, glue, logging, startup
   │
   ├──▶ omni-core   application logic: app actor, pane state machine, listings,
   │       │        sorting, keymap, formatting. No GUI dependency.
   │       ▼
   └──▶ omni-vfs    filesystem abstraction (FileSystem trait, capabilities,
                    Entry model) and providers (LocalFs).
```

Dependencies point downwards only. `omni-core` and `omni-vfs` never depend on Slint, so
everything except rendering is tested without a window.

The workspace started with only the crates Milestone 1 needs. Further crates from the
original plan (`omni-operations`, `omni-config`, `omni-platform`, …) will be added when
their functionality is implemented, not as placeholders.

## Threading model

```text
 main thread (Slint event loop)              Tokio runtime (worker threads)
 ─────────────────────────────               ──────────────────────────────
 callbacks ──Command──▶ (unbounded) ───────▶ App actor (one task, owns all state)
                                                 │ start_load
                                                 ▼
                                             loader task ◀─DirEvent─ blocking pool:
                                                 │      (bounded 4)   std::fs::read_dir
                                                 ▼                    + metadata, 512/batch
                                             (bounded 16) back into the actor
 spawn_local task ◀──Update── (bounded 64) ◀──── actor
   applies updates to Slint models
```

- **The main thread belongs to Slint.** It never performs filesystem I/O and never blocks
  on the core. Callbacks only translate UI events into `Command`s.
- **The app actor** (`omni_core::app`) is a single Tokio task that owns both panes. All
  state changes happen there, so there is no shared mutable state and no locks.
- **Directory enumeration** runs on Tokio's blocking pool and streams batches through a
  bounded channel. When the consumer is slow, the producer waits (backpressure). Dropping
  the stream cancels the enumeration at the next batch.
- **Updates to the UI** go through a bounded channel. The UI side awaits it inside
  `slint::spawn_local`; Tokio's sync primitives are runtime-agnostic, so this needs
  neither `invoke_from_event_loop` closures nor thread-locals.
- Commands use an unbounded channel on purpose: they are user-generated (low rate), and
  the UI thread must never wait to send one.

## Key design decisions

### Pane state is a synchronous state machine

`PaneState` (`omni_core::pane`) performs no I/O. Its methods return `Changes`: "start this
enumeration", "publish the listing with the cursor on row N", "status changed". The actor
carries these out. This keeps the subtle logic testable with plain unit tests: stale
results, focus restoration, partial listings, and error recovery.

### Snapshots, tokens and stale results

- Every enumeration gets a `LoadToken`. Events carrying an outdated token are ignored, and
  the superseded loader task is aborted, which cancels the provider.
- Every published listing is an immutable `Listing` with a `SnapshotId`. Commands that
  refer to a row (`Open`, `CursorMoved`) carry the snapshot id, so a row index from an
  outdated listing is never applied to a newer one.
- `Listing` shares its entries (`Arc<Vec<Entry>>`) and natural-sort keys between all
  re-sorted versions. A re-sort only builds a new `Vec<u32>` order.

### Cursor ownership

The cursor row lives in the UI, so key repeat is instant without a round trip. The UI
reports moves (`CursorMoved`) and the core remembers the entry *name*. When a listing
changes (refresh, re-sort, going up), the core tells the UI where to put the cursor: on
the same entry if it still exists, otherwise on the same row.

### Navigation history

Each pane keeps two stacks of visited locations (back and forward). Every entry remembers
where the cursor was. Each load carries a `HistoryStep` (`Stay`, `Push`, `Back(n)`,
`Forward(n)`), applied only when the load shows its first listing:

- Failed navigations never enter the history.
- Pressing Back again while the previous step is still loading goes one step further
  (`Back(2)`); the skipped location moves to the forward stack.
- A history entry that can no longer be listed is dropped, so it cannot block the way back.
- Each direction keeps at most 100 entries.

Typed locations are resolved by the pure functions in `omni_core::location`: trimming,
quotes, `~`, relative paths, and lexical `..` handling like a shell's `cd`. The pane does
not check in advance whether the target is a file. If listing fails with "not a directory",
it shows the parent with the file under the cursor. Breadcrumbs come from the same module.

### Tabs

Each pane is a `Side` holding a list of tabs. Each tab is a full `PaneState` with its own
history, selection and filter, plus a stable `TabId` and its loader task.

- **Routing.** Directory events carry the `TabId`, not a position, so closing or
  reordering tabs can never send results to the wrong tab. Events of a closed tab are
  dropped, and dropping a `Tab` aborts its loader.
- **What is published.** Commands addressed to a pane act on its active tab, and only
  the active tab is published. Activating a tab publishes its complete state again
  (listing, cursor, selection, status). Background tabs keep loading silently; only
  their titles reach the tab bar (`Update::Tabs`, sent when it changes).
- **New tabs.** `Ctrl+T` duplicates the active tab: the copy shares the loaded listing,
  so it appears at once, and refreshes in the background.
- **Tab bar.** Shown only with two or more tabs. Tabs are placed with computed geometry
  (like the columns), so many tabs are clipped rather than widening the pane.

### Selection and quick filter

Both live in `PaneState`, so they are testable without a GUI and available to the file
operations of Milestone 3.

- **Bit set by entry index.** The selection (`omni_core::selection::Selection`) indexes the
  directory's entries. Entry indices are shared by every re-sorted or filtered `Listing`,
  so sorting and filtering never have to translate it.
- **Refresh and navigation.** A refresh of the same directory carries the selection over
  by name; a different directory starts empty.
- **Filtered views.** A filter produces a new `Listing` over the same entries and keys,
  with a subset of the order. `PaneState` keeps the unfiltered listing (`full`) next to
  the shown one, so changing the filter never re-reads or re-sorts.
- **Hidden means deselected.** Entries hidden by a filter are deselected, so an action on
  the selection can never touch files that are not visible.
- **Shift ranges.** A range has an anchor. The selection that existed when the range
  started is kept as its base, and each step selects base ∪ [anchor, cursor], so going
  back shrinks the range again. The UI tracks where a run of Shift movements started; the
  core does the selecting.
- **Sending changes to the UI.** `Update::Selection` names the changed rows; above 1024
  rows it says "all". The row model then notifies just those rows (not a model reset),
  so the scroll position is kept. Selecting all 100 000 entries of the test directory
  updates the UI in well under a second in a debug build.

### Rendering large listings

The Slint model (`RowsModel`) wraps the current `Arc<Listing>` and formats rows lazily
in `row_data`. `ListView` instantiates only visible rows, so a 100 000-entry directory
costs one `Arc` swap on the UI thread. Rows position their cells explicitly from one
`Columns` geometry struct instead of using layouts: no per-row layout solving, and the
visible columns can depend on the pane width without binding loops.

### Partial listings

A directory that takes longer than 400 ms shows partial results. Each partial listing
re-sorts everything received so far, so the next one is scheduled at
`max(1 s, 10 × cost of the last one)` after it finished. This caps the time spent on
partial listings at about 10% regardless of directory size or build profile.

### Sorting

Names are encoded once into order-preserving natural-sort keys, all stored in one
contiguous arena. Sorting moves 24-byte records whose primary and secondary integer keys
decide almost every comparison. For example, extensions are mapped to dense ranks. The
arena and the entries are touched only on ties. Directories always come first; the
final tie-break on raw name bytes makes the order total and deterministic. A differential
test checks the key encoding against a straightforward reference comparison.

### Keyboard shortcuts

Slint key events are converted to toolkit-independent `KeyChord`s (`omni-ui-slint/src/keys.rs`)
and resolved through `omni_core::keymap::Keymap`. Bindings are data, so making them
configurable (Milestone 2) is a matter of loading them from settings. Slint 1.18's
`KeyBinding` element was not used because its bindings are fixed in the markup.

## Rendering

Slint's FemtoVG renderer runs on wgpu (feature `renderer-femtovg-wgpu`). With that feature
enabled, Slint uses it as the default and falls back to the software renderer if wgpu
cannot create a device or surface. The OpenGL FemtoVG renderer stays compiled in but is
only used when selected with `SLINT_BACKEND=winit-femtovg`.

Why not OpenGL by default: on NVIDIA's proprietary X11 driver, rendering through GLX makes
the mouse cursor invisible over the window. It is not caused by the transparent (32-bit
ARGB) window that Slint creates, which the software renderer also uses without problems.
It is specific to NVIDIA's OpenGL implementation: the same code on Mesa is fine, and so is
wgpu/Vulkan on the same NVIDIA GPU.

Cost of the wgpu renderer: about 8 MB more binary size (wgpu, naga, ash). Vulkan is loaded
at runtime, so there are no extra build-time system dependencies.

## Filesystem abstraction

`omni_vfs::FileSystem`:

- `capabilities()` describes what the provider supports (symlinks, Unix permissions,
  Windows attributes, atomic rename, random access). Callers check capabilities instead of
  assuming local-disk behaviour.
- `read_dir(path) -> DirStream` streams `DirEvent`s (`Entries`, `Warning`, `Done`,
  `Failed`). A stream that ends without a terminal event reports `Interrupted`, so a
  partial listing is never mistaken for a complete one.

Names (`OsString`) and paths (`PathBuf`) keep their native representation. Display uses
lossy conversion, while navigation always uses the original bytes. `child_path` turns an
untrusted entry name into a path and rejects anything that is not a single plain component
(`..`, `a/b`, absolute paths, Windows prefixes).

Each pane holds an `Arc<dyn FileSystem>` together with its path. Remote providers will need
a richer path type than `PathBuf` (for example for `/`-separated remote paths on Windows).
That type will be introduced together with the first remote provider.

The local provider (`LocalFs`) uses `std::fs` on the blocking pool:

- `DirEntry::metadata` (no symlink following); links are additionally resolved to report
  their target kind and size. Broken links are marked as such.
- Entries whose metadata cannot be read (e.g. a directory with `r--` permissions) are
  still listed, and one summary warning is reported instead of one per entry.
- Hidden detection: dot-files on Unix, `UF_HIDDEN` on macOS, `FILE_ATTRIBUTE_HIDDEN` on
  Windows.

## Error handling

- Library crates use `thiserror` (`VfsError`); the binary uses `anyhow` for startup errors.
- Error messages lead with the reason and end with the path, so truncated messages in the
  narrow status line still say what went wrong.
- A failed navigation leaves the pane on its current listing and shows the error in the
  pane. If the very first load fails, the pane shows the failed location empty so the
  user can still go up from it.
- Warnings and errors are also logged through `tracing`.
