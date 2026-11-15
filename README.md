# OmniCommander

A cross-platform, dual-pane file manager written in Rust with a [Slint](https://slint.dev)
user interface. It aims to combine the power-user file management of Total Commander and
Double Commander with modern navigation and presentation.

> **Status:** Milestone 2 (navigation and usability) is nearly complete; only automatic
> refresh is missing. You can browse local folders in two panes with tabs, history,
> breadcrumbs, multi-selection, a quick filter, and settings that persist between
> sessions. **File operations (copy, move, delete) are not implemented yet**; they are
> Milestone 3. See [docs/roadmap.md](docs/roadmap.md).

## Features

- Two independent panes, each with tabs; every tab has its own location, history,
  selection and filter
- Asynchronous, cancellable directory enumeration; the UI never waits on the disk
- Incremental display of slow or very large directories (tested with 100 000 entries)
- Columns: name, extension, size, modification time, Unix permissions / Windows attributes
- Directories first, then case-insensitive natural order (`file2` before `file10`);
  sort by name, extension, size or date
- Symbolic links (including broken ones), hidden files (toggle with Ctrl+H), and names
  that are not valid UTF-8
- Back/forward history that restores the cursor, clickable breadcrumbs, and an editable
  location: `~`, relative paths and `..` work, and a file path opens its folder with the
  file selected
- Multi-selection by keyboard and mouse, with a summary of what is selected
- Quick filter: start typing, or use wildcards and alternatives (`*.jpg;*.png`)
- Settings file with configurable shortcuts, theme (system/light/dark) and renderer;
  open tabs are restored on the next start
- Errors such as "permission denied" are reported in the pane, which stays usable
- GPU rendering (Vulkan via wgpu) with automatic software fallback; vector icons for
  crisp rendering at any DPI

## Keyboard

Default shortcuts. All of them can be changed in `settings.toml` (see
[Configuration](#configuration)).

| Key | Action |
| --- | --- |
| ↑ ↓ PgUp PgDn Home End | Move the cursor |
| Enter / double-click | Open the directory under the cursor (`..` goes up) |
| Backspace, Ctrl+PgUp | Go to the parent directory |
| Alt+← / Alt+→, mouse back/forward buttons | Back / forward in the tab's history |
| Ctrl+L, or click the empty part of the location bar | Edit the location (Enter go, Esc cancel) |
| Click a breadcrumb | Go to that folder |
| Insert | Select/deselect the entry under the cursor and move down |
| Space | Select/deselect the entry under the cursor |
| Shift+↑ ↓ PgUp PgDn Home End, Shift+click | Select a range |
| Ctrl+click | Select/deselect one entry |
| Ctrl+A / Ctrl+Shift+A | Select all shown entries / deselect all |
| Type any letter, or Ctrl+F | Quick filter (↑ ↓ move, Enter open, Tab back to the list, Esc clear) |
| Esc (in the list) | Clear the quick filter |
| Ctrl+H | Show / hide hidden files |
| Tab, Shift+Tab | Switch the active pane |
| Ctrl+T | New tab with the current folder |
| Ctrl+↑, middle-click a folder | Open the folder in a new tab (middle-click: in the background) |
| Ctrl+W, middle-click a tab, × on a tab | Close the tab (the last tab stays) |
| Ctrl+Tab / Ctrl+Shift+Tab, click a tab | Next / previous / chosen tab |
| Ctrl+R | Re-read the active pane |
| Ctrl+F3 / F4 / F5 / F6 | Sort by name / extension / date / size (again: reverse) |
| Click a column header | Sort by that column |

On macOS, Ctrl is the Command key.

## Building and running on Zorin OS / Ubuntu / Debian

1. Install Rust (stable, 1.92 or newer) with [rustup](https://rustup.rs).
2. Install the system libraries Slint needs to build:

   ```sh
   sudo apt install build-essential pkg-config libfontconfig-dev libxkbcommon-dev
   ```

   The development machine (Zorin OS 18.1, X11) also had the X11, Wayland, EGL and
   OpenGL development packages installed. If linking fails, add
   `libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libegl1-mesa-dev`.

3. Build and run:

   ```sh
   cargo run --release -p omni-ui-slint                  # restores the last session
   cargo run --release -p omni-ui-slint -- /etc /var/log # explicit left | right folders
   ```

   The binary is `target/release/omnicommander`. Debug builds (`cargo run -p omni-ui-slint`)
   work too, but sorting very large directories is several times slower.

## Configuration

OmniCommander keeps two files in its configuration directory: `~/.config/omnicommander`
on Linux, `~/Library/Application Support/OmniCommander` on macOS, and
`%APPDATA%\OmniCommander\config` on Windows. Set `OMNI_CONFIG_DIR` to use another
directory.

- **`settings.toml`**: your preferences. It is created on the first start with every option
  commented, and OmniCommander never rewrites it. Mistakes are reported in the left pane
  and in the log, and only the affected section falls back to defaults.

  ```toml
  [appearance]
  theme = "dark"          # "system", "light" or "dark"
  renderer = "auto"       # "auto", "gpu", "opengl" or "software"

  [panes]
  restore_session = true
  sort = "name"           # for panes not restored from the session

  [keys]                  # action name = shortcut(s); replaces the defaults
  refresh = ["Ctrl+R", "F2"]
  new_tab = []            # unbind
  ```

  The generated file lists every action name.

- **`session.toml`**: written on exit. It holds the open tabs, their sort order, the
  active pane and the hidden-files toggle. Folders given on the command line take
  precedence over it.

### Rendering

OmniCommander renders on the GPU with Slint's FemtoVG renderer on
[wgpu](https://wgpu.rs), which uses Vulkan on Linux. If no usable GPU API is available,
Slint falls back to its software renderer automatically. Pick a renderer with
`renderer` in `settings.toml`, or with `SLINT_BACKEND`, which takes precedence:

| `SLINT_BACKEND` | `renderer` | Renderer |
| --- | --- | --- |
| *(unset)* or `winit-femtovg-wgpu` | `auto` / `gpu` | FemtoVG on wgpu (Vulkan), the default |
| `winit-femtovg` | `opengl` | FemtoVG on OpenGL |
| `winit-software` | `software` | CPU rendering |

The OpenGL renderer is not the default. With NVIDIA's proprietary driver on X11 (seen with
driver 580.178, RTX 3060 Laptop GPU, Zorin OS 18.1), it makes the mouse cursor invisible
over the window. The same code is fine on Mesa's OpenGL, and the Vulkan path is fine on
NVIDIA.

### Logging

Logging goes to stderr. Set `OMNI_LOG` with
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax, for example `OMNI_LOG=debug` or `OMNI_LOG=info,omni_core=trace`.

## Documentation

- [docs/architecture.md](docs/architecture.md): crates, threading model, data flow, design decisions
- [docs/development.md](docs/development.md): building, testing, benchmarks, conventions
- [docs/roadmap.md](docs/roadmap.md): milestones and known limitations
- [CLAUDE.md](CLAUDE.md): working notes for AI-assisted development: current state and next steps

## License

Not chosen yet. Until a license is added, the default copyright rules apply: the code may
be read but not reused. Note that Slint is available under GPLv3, a royalty-free license for
desktop applications (with attribution requirements), or a commercial license; the choice
affects how OmniCommander can be distributed.
