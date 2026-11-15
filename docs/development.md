# Development

## Toolchain

- Rust stable, edition 2024, MSRV 1.92 (Slint's minimum). Developed with Rust 1.100.
- Slint 1.18 with the winit backend. Default renderer: FemtoVG on wgpu (Vulkan); the
  software renderer is the automatic fallback, and OpenGL FemtoVG is available on request.
  Force one with `SLINT_BACKEND` (`winit-femtovg-wgpu`, `winit-femtovg`, `winit-software`);
  see the README for why OpenGL is not the default.
- System packages: see the README.

## Everyday commands

```sh
cargo run -p omni-ui-slint [-- LEFT_DIR [RIGHT_DIR]]   # run (debug)
cargo run --release -p omni-ui-slint                    # run (release)
cargo test --workspace                                  # all tests, no GUI needed
cargo clippy --workspace --all-targets                  # must be warning-free
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

In debug builds, dependencies are compiled with `opt-level = 2` (see the root
`Cargo.toml`), which keeps Slint rendering smooth while our crates stay quick to rebuild.

## Logging

`OMNI_LOG` takes an `EnvFilter` directive. The default is
`info,wgpu_core=error,wgpu_hal=error,naga=error`: wgpu's own `info` and `warn` output is not
actionable for users. Setting `OMNI_LOG` replaces the default completely.

```sh
OMNI_LOG=debug cargo run -p omni-ui-slint              # everything, including winit
OMNI_LOG=info,omni_core=debug,omni_vfs=debug cargo run -p omni-ui-slint
```

`omni_vfs` logs enumeration timings at `debug`; `omni_core` logs loads, failures and slow
partial listings.

## Tests

| Where | What |
| --- | --- |
| `omni-vfs/src/*` | Local provider against real temporary directories: metadata, symlinks, non-UTF-8 names, permission errors, batching, cancellation, path-traversal rejection |
| `omni-core/src/*` | Sorting (including a differential test of the key encoding), listings, formatting, keymap, and the pane state machine (stale loads, focus restoration, partial listings, errors) |
| `omni-core/tests/app.rs` | The async app actor end to end, with `LocalFs` and with a scripted filesystem that controls the timing of enumerations |
| `omni-ui-slint/src/keys.rs` | Translation of Slint key events |

Some Unix permission tests skip themselves when run as root, because root ignores
permission bits.

## Benchmarks

Measured with simple programs (no extra dependencies). They report the minimum and
median of several runs:

```sh
cargo run --release -p omni-core --example bench_listing [ENTRIES]   # sort/format
cargo run --release -p omni-vfs --example enumerate -- DIR           # real enumeration
```

Results on the development machine (AMD Ryzen 7 5800H, release build, 100 000 entries; the
machine was heavily loaded during measurement, so these are upper bounds):

| Operation | Time |
| --- | --- |
| Build natural-sort keys | 19 ms |
| Sort by name / extension / size / date | 28 / 81 / 11 / 12 ms |
| Clone entries (cost of one partial listing) | 21 ms |
| Format the ~60 visible rows | 0.04 ms |
| Enumerate 100 000 empty files, warm / cold cache | 377 / 1460 ms (first batch after 2–15 ms) |

## Manual UI testing without touching your desktop

The UI can be exercised in a nested X server, so synthetic input cannot reach other
applications:

```sh
Xephyr :5 -screen 1280x800 -no-host-grab &
DISPLAY=:5 cargo run -p omni-ui-slint
```

Without a window manager, give the window keyboard focus explicitly (for example with
`XSetInputFocus` through XTest tooling) before sending keys.

Vulkan does not go through the nested server's OpenGL, so inside Xephyr the app still
renders on the host GPU. To test other setups, restrict the Vulkan loader:

```sh
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json ...   # Mesa CPU Vulkan (lavapipe)
VK_DRIVER_FILES=/nonexistent.json ...                      # no Vulkan: software fallback
```

## Conventions

- Keep Slint callbacks thin: translate to a `Command` and send it.
- No filesystem calls on the UI thread, ever.
- State lives in the app actor; communicate by messages, not shared locks.
- `unsafe` is denied workspace-wide. Platform code that needs it must be isolated in a
  dedicated module with a documented `#[allow]`.
- New providers implement `omni_vfs::FileSystem` and must report honest `Capabilities`.
- Don't add crates or modules before there is functionality for them.
