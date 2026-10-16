# Image Ranker

A local Windows desktop app for comparing, ranking and organizing images. Built with React, TypeScript, Tauri and Rust. Designed around practical collections of 100–500 images.

## What it does

- Import folders, preview images and filter by folder or filename.
- Select images with clicks, ranges or a drag rectangle. All / None / Invert apply to the current folder and search, replacing selections elsewhere.
- Rank with Adaptive, Winner stays or Full pairwise comparisons; pause, save, reopen and Undo.
- Reorder the Review grid with its grip handles and mark final picks individually or with **Star selected**.
- Move selected originals into a new subfolder for separate style ranking.
- Preview and approve original renames, ranked copies and final-pick exports, with conflict checks and file-operation history.

**Adaptive is the recommended starting mode.** Its progress bar measures placement and refinement stages. The number of images is not the number of decisions: Full pairwise with 500 images requires 124,750 comparisons.

## Status and limits

This is an early open-source release of a tool developed for personal use on Windows. There is no signed installer or supported macOS/Linux release. Build the executable from source using the instructions below. Existing local checks are useful evidence, but they do not establish compatibility with every PC, image format or storage device.

Managed profiles, saves and journals require a local fixed NTFS volume. File operations support qualified local fixed NTFS and exFAT volumes, with different recovery rules. **exFAT Undo requires the same app and profile to remain open.** Mixed NTFS/exFAT original-rename batches are refused; rank and rename each folder separately. Do not rely on network or removable storage for these write workflows. See [usage](docs/usage.md) and [recovery](docs/recovery.md).

The supported decoder handles qualified 8-bit JPEG, PNG, BMP, WebP, GIF first frames and TIFF first pages. HEIC/HEIF and RAW are unavailable. Original-file operations can change names and locations, so use disposable copies for experimentation and keep independent backups of valuable files.

## Build on Windows

Install [Node.js 24](https://nodejs.org/), [Rust](https://rustup.rs/), the Visual Studio C++ Build Tools with the Desktop development with C++ workload, and the Microsoft Edge WebView2 Runtime. The repository pins Rust 1.94.1 in `rust-toolchain.toml`. See [Tauri's Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows) for the native tools.

```powershell
git clone https://github.com/eriklgamedev/image-ranker.git
cd image-ranker
npm ci
npm run desktop
```

For a normal optimized executable without test instrumentation or an installer:

```powershell
npm run tauri -- build --no-bundle -- --locked --no-default-features
```

The executable is written to `src-tauri/target/release/image-ranker.exe`. Keep an existing profile backed up before testing a different build. The app identifier is `local.imageranker.desktop`; builds using this identifier share its default local profile.

`npm run dev` opens a browser demo. It does not provide native file operations or persistent desktop storage.

## Checks

```powershell
npm run check
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked
```

Windows CI runs those checks and a normal executable build. Tests that explicitly require a real exFAT volume are ignored by default and need an intentionally created disposable test root. Desktop automation also requires separate test drivers and an instrumented build; see [testing](docs/testing.md). Never point mutation tests at personal images.

## Contributing

Small, focused improvements and reproducible bug reports are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing file operations, saves or recovery. Please omit images, personal paths and session journals from public reports. See [SECURITY.md](SECURITY.md) for sensitive reports.

## License

Project-authored code and documentation are licensed under [MIT](LICENSE). Third-party components and retained upstream files keep their own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the bundled notices in `assets/`.
