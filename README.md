# OmniCommander

A cross-platform, dual-pane file manager written in Rust with a [Slint](https://slint.dev)
user interface. It aims to combine the power-user file management of Total Commander and
Double Commander with modern navigation and presentation.

> **Status:** Milestone 1 (functional foundation). You can browse local directories in two
> independent panes. File operations are not implemented yet; see [docs/roadmap.md](docs/roadmap.md).

## Features (Milestone 1)

- Two independent panes, each listing a local directory
- Asynchronous, cancellable directory enumeration; the UI never waits on the disk
- Incremental display of slow or very large directories (tested with 100 000 entries)
- Columns: name, extension, size, modification time, Unix permissions / Windows attributes
- Directories first, then case-insensitive natural order (`file2` before `file10`);
  sort by name, extension, size or date
- Symbolic links (including broken ones), hidden files, and names that are not valid UTF-8
- Errors such as "permission denied" are reported in the pane, which stays usable
- Keyboard-driven navigation through a toolkit-independent keymap
- Follows the system light/dark preference; vector icons for crisp rendering at any DPI

## Keyboard

| Key | Action |
| --- | --- |
| ↑ ↓ PgUp PgDn Home End | Move the cursor |
| Enter / double-click | Open the directory under the cursor (`..` goes up) |
| Backspace, Ctrl+PgUp | Go to the parent directory |
| Tab, Shift+Tab | Switch the active pane |
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
   cargo run --release -p omni-ui-slint                  # current dir | home dir
   cargo run --release -p omni-ui-slint -- /etc /var/log # explicit left | right dirs
   ```

   The binary is `target/release/omnicommander`. Debug builds (`cargo run -p omni-ui-slint`)
   work too, but sorting very large directories is several times slower.

### Rendering

OmniCommander renders on the GPU with Slint's FemtoVG renderer on
[wgpu](https://wgpu.rs), which uses Vulkan on Linux. If no usable GPU API is available,
Slint falls back to its software renderer automatically. To pick a renderer explicitly,
set `SLINT_BACKEND`:

| `SLINT_BACKEND` | Renderer |
| --- | --- |
| *(unset)* or `winit-femtovg-wgpu` | FemtoVG on wgpu (Vulkan), the default |
| `winit-femtovg` | FemtoVG on OpenGL |
| `winit-software` | CPU rendering |

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

## License

Not chosen yet. Note that Slint is available under GPLv3, a royalty-free license for
desktop applications (with attribution requirements), or a commercial license; the choice
affects how OmniCommander can be distributed.
