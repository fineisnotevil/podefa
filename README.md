<!--
SPDX-License-Identifier: CC0-1.0
SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>
-->

# [PROJECT_NAME]

A lightweight, cross-platform PDF viewer and editor designed for low resource consumption, instantaneous startup, and robust architectural separation.

## Status

**Early development**: The core workspace architecture, dependency graph, and initial scaffolding are established. Rendering pipelines and UI features are currently under active design and development.

## Goals and Non-Goals

### Goals
- **Minimal Resource Footprint**: Modest memory usage and negligible idle CPU consumption, suitable for low-end hardware.
- **Fast Startup**: Near-instant window display and document opening.
- **Cross-Platform**: First-class support for Windows 10+, Linux, and macOS; architected for potential future expansion to mobile (Android/iOS).
- **Clean Architectural Separation**: Strict crate boundaries isolating UI (`crates/app`), rendering logic (`crates/render`), backend engine implementations (`crates/engine-mupdf`), and domain primitives (`crates/core`).
- **Strict Copyleft & FOSS Compliance**: Strong adherence to open-source copyleft licenses and comprehensive REUSE specification compliance.

### Non-Goals
- **Full Office Suite / Word Processor Replacement**: Focused strictly on reading, annotating, and editing PDF structures; not a layout engine for arbitrary word-processing formats.
- **Electron / Heavy Web Engine Wrapping**: Explicitly eschews multi-hundred-megabyte runtimes in favor of native compiled code and lightweight declarative UI.
- **Proprietary DRM or Cloud Lockdown**: Does not implement proprietary digital rights management schemes or forced cloud services.

## Prerequisites & Build Instructions

Building `[PROJECT_NAME]` requires a Rust toolchain (pinned in `rust-toolchain.toml`) and native C/LLVM tooling required by `mupdf` (`bindgen` / `libclang`).

### Platform Prerequisites

#### Linux (Debian / Ubuntu)
```bash
sudo apt update
sudo apt install -y build-essential clang libclang-dev pkg-config libxkbcommon-dev libfontconfig1-dev
```

#### macOS (Homebrew)
```bash
brew install llvm
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"
```

#### Windows (MSVC)
1. Install **Visual Studio Build Tools** (with "Desktop development with C++").
2. Install **LLVM** (e.g. via winget: `winget install LLVM.LLVM`).
3. Ensure `clang` is in your `PATH` and `LIBCLANG_PATH` points to the LLVM `bin` or `lib` directory (for example, `C:\Program Files\LLVM\bin`).

### Building and Running

Clone the repository and build the workspace:

```bash
git clone [GITHUB_URL]
cd [PROJECT_NAME]

# Build all workspace crates
cargo build --workspace

# Run test suite
cargo test --workspace

# Launch the desktop application
cargo run -p app
```

## Repository Structure

- `crates/core`: Engine-agnostic domain traits (`PdfEngine`, `Command`) and geometry/buffer types (`Rect`, `Bitmap`). Free of UI or engine dependencies.
- `crates/engine-mupdf`: Implementation of `PdfEngine` wrapping MuPDF.
- `crates/render`: Viewport tile scheduler, LRU rasterization cache, and background render dispatching.
- `crates/app`: Declarative desktop frontend powered by Slint.

## Licensing

This project is licensed under the **GNU Affero General Public License v3.0 or later** ([AGPL-3.0-or-later](LICENSE)).
- The underlying PDF engine backend utilizes MuPDF, licensed under AGPL-3.0-or-later.
- The UI layer uses Slint under its GPLv3 license option.

The repository follows the [REUSE Specification](https://reuse.software/) with explicit SPDX headers in every file and license texts provided in [`LICENSES/`](LICENSES/).
