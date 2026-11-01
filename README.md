# MD-Viewer

A native Markdown editor and viewer for macOS on Apple Silicon — written in
Rust with **egui/eframe** (wgpu → Metal), **winit** and **pulldown-cmark**.

Edit Markdown on the left, see a live preview on the right, and export the
document as a PDF. The editor and preview are self-contained — no Node, no
browser engine, no webview. PDF export uses the `weasyprint` CLI as its single,
mandatory conversion step (no fallback writer), so it has to be installed once;
the app shows an onboarding dialog with the exact command.

![Rust](https://img.shields.io/badge/Rust-2024-000?logo=rust)
![Platform](https://img.shields.io/badge/platform-macOS%20arm64-lightgrey)
![PDF export](https://img.shields.io/badge/PDF%20export-WeasyPrint-orange)

---

## Installation (release archive)

The published archive is `MD-Viewer-<version>-arm64.zip` and contains
`MD-Viewer.app`. It runs on Apple Silicon (arm64) Macs with macOS 11 or newer;
Intel Macs are not supported.

1. Download and unzip `MD-Viewer-<version>-arm64.zip`.
2. Drag `MD-Viewer.app` into **Applications**.
3. First launch: the app is not notarized, so macOS may block it.
   Right-click the app → **Open** → **Open**, or use
   **System Settings → Privacy & Security → Open Anyway**.
   If macOS reports the app as *damaged*, remove the quarantine flag:
   `xattr -dr com.apple.quarantine /Applications/MD-Viewer.app`.
4. PDF export only: install WeasyPrint once with
   `brew install weasyprint` (requires [Homebrew](https://brew.sh)).
   MD-Viewer shows an onboarding dialog at launch with the same command and a
   **Check again** button. The editor and preview work without WeasyPrint.

Verify the download against the published checksum:

```bash
shasum -a 256 -c MD-Viewer-<version>-arm64.zip.sha256
```

---

## Highlights

- **Complete Markdown** — CommonMark + GFM, tables, task lists, footnotes, callouts, code highlighting and typeset LaTeX math.
- **Obsidian & Pandoc** — wiki-links, `![[…]]` embeds, front matter, fenced divs, admonitions and `[TOC]`.
- **Live preview & split editor** — click any preview block to jump to its source line; find & replace and an outline sidebar keep long documents navigable.
- **Print-quality PDF** — A4, running footer, clickable links, images, math and Mermaid via WeasyPrint.
- **Native macOS UI** — dark and Paper themes, SF Pro/SF Mono, tabs, drag & drop and native dialogs.
- **Fast & self-contained** — Metal-accelerated rendering, virtualized large documents, no browser engine.
- AI assisted by **HAIKU 5.5**.

See [feature-list.md](feature-list.md) for a concise feature overview and the
technical characteristics.

---

## Requirements

| Requirement | Notes |
| --- | --- |
| macOS 11 (Big Sur) or newer, Apple Silicon | arm64 only. Intel Macs are not supported. |
| Rust toolchain (stable) | Install via [rustup](https://rustup.rs). `cargo` and `rustc` must be on `PATH`. |
| Xcode Command Line Tools | `xcode-select --install` (provides the linker and system frameworks). |
| WeasyPrint *(required for PDF export only)* | `brew install weasyprint` — the single PDF conversion step (A4 pagination, font fallback, link annotations). Missing or failing WeasyPrint is reported as an export error; there is no fallback writer. MD-Viewer shows an onboarding dialog with the install command when it is missing. |

No extra Homebrew packages are needed for the editor and preview. All Rust
dependencies are pure-Rust crates fetched by Cargo. Remote images are
downloaded with `/usr/bin/curl`, which ships with every macOS installation
(no extra install step).

Verify your toolchain:

```bash
rustc --version
cargo --version
```

---

## Quick Start

```bash
git clone <repository-url> MD-Viewer
cd MD-Viewer
cargo run --release
```

The published repository is deliberately minimal: the application sources, the
build script, `README.md`, `LICENSE`, `feature-list.md` and `start-up.md`.
Test documents, reference checkouts and build artifacts are not part of the
tree. The test fixtures are compiled into the test binary, so the suite is
self-contained.

The first build compiles the full wgpu/egui stack and can take a few minutes.
Subsequent builds are incremental.

For development (faster compile, debug assertions):

```bash
cargo run
```

---

## Startup document

MD-Viewer opens `start-up.md` when it launches. The file is loaded at runtime,
not compiled into the binary:

- **App bundle:** `MD-Viewer.app/Contents/Resources/start-up.md`
- **`cargo run`:** `start-up.md` in the current working directory

It is opened as an untitled document, so edits require **Save As…** and never
modify the shipped file. Relative images and links resolve against the file's
own folder. If the file is missing or unreadable, the app starts with an empty
document — deleting `start-up.md` is always safe.

---

## Building

### Debug build

```bash
cargo build
```

Binary: `target/debug/md_viewer`

### Release build (recommended)

```bash
cargo build --release
```

Binary: `target/release/md_viewer`

The release build targets generic Apple Silicon (arm64), so the resulting
binary runs on every M-series Mac. Intel Macs are not supported.

### Tests and lints

```bash
cargo test
cargo clippy --all-targets
```

All tests must pass and there should be zero warnings. The 115 tests cover the
parser (including HTML block conversion and the extended compatibility
corpus), the print-HTML renderer (preprocessors, sanitizer, template,
math/Mermaid image embedding, escape/base64 helpers), the WeasyPrint runner
(missing/failing binary handling and a real end-to-end export when WeasyPrint
is installed), the dark/Paper palettes, the tab state machine, encoding
detection, bidi reordering, link resolution, preview virtualization,
Pandoc/MkDocs/Obsidian extension parsing, front matter formats, media/SVG
handling, picture-viewer logic and magnification.

Two regression guards keep feature coverage complete:

- **Variant coverage** — a compiled-in fixture must exercise every `Block` and
  `Inline` variant the parser can produce. Adding a new construct without a
  fixture fails the test and names what is missing.
- **Headless render smoke** — a gallery document containing every construct is
  laid out with egui in both themes at four widths; the test asserts that the
  content of each feature actually appears in the draw list (not just that
  rendering did not panic).

All fixtures live in `src/test_fixtures.rs`, so the suite needs no external
Markdown files. For a slower end-to-end pass:
`cargo test full_documents_render_headlessly -- --ignored`.

---

## Creating a macOS App Bundle

`build_app.sh` builds the release binary, writes the `Info.plist`, embeds the
icon, signs the bundle and verifies the signature:

```bash
./build_app.sh            # MD-Viewer.app (ad-hoc signed)
./build_app.sh --package  # + dist/MD-Viewer-<version>-arm64.zip (+ .sha256)
./build_app.sh --install  # + replace /Applications/MD-Viewer.app
```

The version is read from `Cargo.toml`. The bundle identifier defaults to
`com.kauschke.mdviewer` and can be overridden with `BUNDLE_ID=...`.

Result:

```text
MD-Viewer.app/
└── Contents/
    ├── Info.plist
    ├── MacOS/
    │   └── MD-Viewer
    └── Resources/
        └── AppIcon.icns
```

### Release archive

`--package` writes `dist/MD-Viewer-<version>-arm64.zip` and a `.sha256`
checksum. The archive contains `MD-Viewer.app` and is the file users download.

Because the archive is ad-hoc signed unless you configure a Developer ID (see
below), the first launch on another Mac requires one of:

1. Right-click → **Open**, then confirm, **or**
2. `System Settings → Privacy & Security → Open Anyway`.

If macOS reports the app as “damaged” (e.g. after downloading a zipped build),
remove the quarantine attribute:

```bash
xattr -dr com.apple.quarantine /Applications/MD-Viewer.app
```

### Signing and notarization

With an Apple Developer account, sign with a **Developer ID Application**
certificate and notarize the archive so users do not see the Gatekeeper
warning:

```bash
# once: store notarization credentials in the keychain
xcrun notarytool store-credentials mdviewer-notary \
  --apple-id you@example.com --team-id TEAMID \
  --password <app-specific-password>

CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_PROFILE="mdviewer-notary" \
./build_app.sh --package
```

The script then signs with the hardened runtime, uploads the zip to Apple,
staples the ticket into the bundle and re-creates the archive. Verify with:

```bash
codesign --verify --deep --strict --verbose=2 MD-Viewer.app
spctl --assess --type execute --verbose=4 MD-Viewer.app   # must say "accepted"
```

### Install locally

```bash
./build_app.sh --install
open /Applications/MD-Viewer.app
```

`--install` replaces `/Applications/MD-Viewer.app` and removes the local build
copy. Use it instead of copying manually: two bundles with the same
`CFBundleIdentifier` make macOS show **two entries with the same name** in
Spotlight, Launchpad and the Apps view (one of them usually keeps a stale icon
until the LaunchServices cache is refreshed). The script unregisters the removed
copy and re-registers the installed one.

### Manual bundle (without the script)

```bash
cargo build --release
mkdir -p MD-Viewer.app/Contents/MacOS
cp target/release/md_viewer MD-Viewer.app/Contents/MacOS/MD-Viewer
# add Contents/Info.plist (see build_app.sh for a complete template)
codesign --force -s - MD-Viewer.app
```

### App icon

<p align="center"><img src="assets/AppIcon-1024.png" width="180" alt="MD-Viewer app icon"></p>

The published tree ships the two icon files the app and the bundle need:

| File | Purpose |
| --- | --- |
| `assets/AppIcon.icns` | Ready-to-use icon, picked up automatically by `build_app.sh` |
| `assets/AppIcon-1024.png` | 1024×1024 master (transparent padding, 824×824 icon body), embedded in the binary for the runtime Dock icon |

The design follows Apple's macOS icon conventions: a 1024×1024 canvas with the
icon body on the 824×824 content grid, a continuous-corner squircle, no text,
soft top-down lighting, a single light source, and enough contrast to stay
legible at 16×16. The palette is taken from the app itself — dark slate
(`#0C1015`–`#2B3644`) with the blue accent (`#0A84FF`) — and the motif is a
Markdown page with a folded corner carrying the Markdown `M↓` mark.

The 1024 px master is also embedded in the binary and passed to
`ViewportBuilder::with_icon`. This is required because eframe otherwise installs
the egui logo as the **runtime** app icon, which is what the Dock and minimized
window use; without it, Finder showed the MD-Viewer icon while the minimized
window showed the egui logo.

On macOS 26/27 the system can render app icons with the “Tinted Dark” icon
appearance, which makes every icon monochrome. That is a system-wide setting
(System Settings → Appearance → Icon appearance), not part of the artwork.

To regenerate the `.icns` after editing the master:

```bash
cd assets
rm -rf AppIcon.iconset && mkdir AppIcon.iconset
sips -z 16 16   AppIcon-1024.png --out AppIcon.iconset/icon_16x16.png
sips -z 32 32   AppIcon-1024.png --out AppIcon.iconset/icon_16x16@2x.png
sips -z 32 32   AppIcon-1024.png --out AppIcon.iconset/icon_32x32.png
sips -z 64 64   AppIcon-1024.png --out AppIcon.iconset/icon_32x32@2x.png
sips -z 128 128 AppIcon-1024.png --out AppIcon.iconset/icon_128x128.png
sips -z 256 256 AppIcon-1024.png --out AppIcon.iconset/icon_128x128@2x.png
sips -z 256 256 AppIcon-1024.png --out AppIcon.iconset/icon_256x256.png
sips -z 512 512 AppIcon-1024.png --out AppIcon.iconset/icon_256x256@2x.png
sips -z 512 512 AppIcon-1024.png --out AppIcon.iconset/icon_512x512.png
sips -z 1024 1024 AppIcon-1024.png --out AppIcon.iconset/icon_512x512@2x.png
iconutil -c icns AppIcon.iconset -o AppIcon.icns
```

---

## Usage

| Action | How |
| --- | --- |
| Open a file | Toolbar **Open**, ⌘O, drag & drop, or double-click / **Open With** in Finder — opens in a new tab |
| Switch tabs | Click a tab, or re-open a file to jump to its existing tab (the tab strip appears with 2+ documents) |
| Close tab | Tab **×** button, middle-click, or ⌘W (prompts to save when dirty; closing down to one tab hides the strip) |
| Follow a local link | Click `[text](other.md)` or `[[Note]]` — opens in a tab (fragments are not scrolled to yet) |
| Open from Finder | Works for cold starts and running instances (macOS Apple Event integration) |
| Save | Toolbar **Save**, ⌘S |
| Save as | ⌘⇧S |
| Export PDF | Toolbar **Export PDF**, ⌘E. Without WeasyPrint the button is greyed out and opens the setup dialog; the rest of the app works normally |
| Print | Toolbar **Print**, ⌘P — exports a temporary PDF and shows the native macOS print dialog |
| Find & replace | Toolbar **Find**, ⌘F; ⌘G / ⇧⌘G next/previous, Esc closes |
| Outline | Toolbar **Outline**, ⌘⇧O — click a heading to jump to that section |
| Toggle dark / Paper theme | Toolbar sun/moon button at the right (sun = dark active, moon = Paper active) |
| Switch view | Toolbar **Preview / Edit**, ⌘⇧P — Edit shows raw text left and live preview right |
| Zoom | Toolbar **A−/A+**, ⌘+/⌘−; the percentage readout between them shows the current magnification and resets to 100% when clicked (also ⌘0) |
| Inspect an image | Click an image in the preview; scroll to zoom, drag to pan, **Esc** (or **Close**) returns to the document |
| Cut / copy / paste | ⌘X / ⌘C / ⌘V (Ctrl also works) or right-click in the editor |
| Select all | ⌘A or right-click → **Select All** |

Supported file extensions: `.md`, `.markdown`, `.mdown`, `.txt`.

---

## Project Structure

```text
MD-Viewer/
├── Cargo.toml / Cargo.lock
├── build_app.sh              # .app bundle, signing, zip packaging
├── README.md
├── LICENSE                   # MIT
├── feature-list.md           # concise feature overview
├── start-up.md               # document shown on launch (runtime-loaded)
├── assets/
│   ├── AppIcon.icns          # bundle icon
│   └── AppIcon-1024.png      # master, also embedded for the runtime icon
└── src/
    ├── main.rs               # entry point, window + wgpu setup
    ├── macos_open.rs         # Finder "Open With" Apple Event integration
    ├── macos_about.rs        # About panel credits (project page)
    ├── parser.rs             # Markdown → Document model
    ├── encoding.rs           # UTF-8/UTF-16/legacy file decoding
    ├── ui.rs                 # egui app: theme, panels, preview, highlighting
    ├── html_export.rs        # Markdown → sanitized print HTML
    ├── export.rs             # PDF export: mandatory WeasyPrint runner
    └── test_fixtures.rs      # compiled-in Markdown corpus for the tests
```

Test documents, reference checkouts and build artifacts are intentionally not
part of the published tree (see `.gitignore`).

---

## Architecture

```text
 ┌────────────┐   text    ┌────────────┐   Document   ┌───────────────────────┐
 │  ui::App   │ ────────► │ parser.rs  │ ───────────► │ ui.rs (preview)       │
 │ (editor)   │           │ pulldown-  │              └───────────────────────┘
 └────────────┘ ◄──────── │ cmark      │ ◄───────────
         ▲        doc      └────────────┘   re-parsed     ┌─► export.rs (PDF:
                               on every change           │   raw Markdown →
                                                         │   html_export.rs →
                                                         │   WeasyPrint CLI)
```

1. `App::text` is the single source of truth.
2. On every edit, `parser::parse` rebuilds `App::doc`.
3. The preview renders `App::doc`; the PDF export renders the raw Markdown
   through the ported `html_export.rs` pipeline, so both follow the same source.

The document model (`parser::Block` / `parser::Inline`) is deliberately
UI-independent. To add a new Markdown feature, see the extension guides in the
module docs of `parser.rs`.

### PDF export pipeline

```text
raw Markdown ──► html_export.rs ──► print-ready HTML ──► weasyprint CLI ──► PDF
                  (fence/inline-code protection,       (A4 @page rules,
                   math/emoji/wiki/callout              running footer,
                   preprocessors, image data URIs,      .markdown-body CSS)
                   pulldown-cmark, ammonia sanitize)
```

WeasyPrint is the only conversion step. The executable is detected at export
time (`PATH`, then `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`), so
GUI-launched instances find Homebrew installations. A missing binary or a
non-zero exit is returned to the UI as an error message (including the CLI's
stderr) — no PDF is written and there is no fallback writer.

---

## Dependencies

| Crate | Version | Purpose |
| --- | --- | --- |
| `eframe` | 0.37 | App shell, winit integration, wgpu backend |
| `egui_extras` | 0.37 | Local image loaders (plus in-tree background downloader for remote images) |
| `image` | 0.25 | PNG/JPEG/GIF/WebP decoding |
| `resvg` | 0.45 | SVG rasterizer (images and Mermaid diagrams) |
| `latex-rust` | 3.0.1 | Pure-Rust LaTeX math typesetting (preview) |
| `mermaid-svg` | 0.7 | Pure-Rust Mermaid parser/layout (preview) |
| `pulldown-cmark` | 0.13 | CommonMark + GFM parser (preview and print HTML) |
| `encoding_rs` | 0.8 | UTF-16/Windows-1252 decoding for opened files |
| `unicode-bidi` | 0.3 | Bidi levels for mixed LTR/RTL preview text |
| `toml` | 0.8 | TOML front matter (`+++` blocks) |
| `serde_json` | 1 | JSON front matter |
| `tl` | 0.7 | Small HTML parser for converting safe raw HTML blocks |
| `ammonia` | 4 | HTML sanitizer for the print pipeline (same policy as the reference implementation) |
| `regex` | 1 | Print preprocessors (math, emoji, wiki-links, callouts) |
| `rfd` | 0.16 | Native file dialogs |
| `winit` | 0.30 | Windowing/event loop |
| `env_logger`, `log` | 0.11 / 0.4 | Logging (`RUST_LOG=debug`) |
| `objc2` (macOS only) | 0.6 | Runtime hook for Finder open-document events |

PDF export uses the WeasyPrint CLI as its single conversion step (an external
program, not a Rust dependency); `src/export.rs` and `src/html_export.rs`
implement the pipeline and add no other runtime requirements.

---

## Troubleshooting

**PDF export fails with "WeasyPrint not found"**

WeasyPrint is mandatory for PDF export. Check that it is installed and
executable:

```bash
which weasyprint || ls /opt/homebrew/bin/weasyprint
weasyprint --version
```

If it is missing: `brew install weasyprint`. The app checks `PATH` plus the
Homebrew locations `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin` at
export time; GUI-launched instances therefore find Homebrew-installed
WeasyPrint without extra configuration.

**PDF export fails with a WeasyPrint error**

The status bar shows the CLI's exit status and stderr. Enable logging
(`RUST_LOG=info`, see below) for the full message. No PDF is written in that
case — by design there is no fallback writer, so a broken file can never be
produced silently.

**The window does not appear / GPU error**

Run with logging enabled:

```bash
RUST_LOG=info cargo run
```

Update macOS and Xcode Command Line Tools. wgpu selects Metal automatically on
macOS.

**`cargo` cannot find a linker**

Install the Command Line Tools:

```bash
xcode-select --install
```

**Remote images do not load**

Remote images are fetched with `/usr/bin/curl`, so they honour the system
proxy/VPN configuration. Make sure the Mac is online and that the image URL is
reachable from Terminal:

```bash
curl -I "https://img.shields.io/badge/Rust-2024-000?logo=rust"
```

On the first network access macOS may ask for local-network permission; choose
**Allow**. Failed images are shown with a framed placeholder explaining the
likely cause.

**Build is slow the first time**

The wgpu/egui dependency tree is large. Use `cargo build --release` once and
afterwards incremental builds are fast.

**App bundle cannot be opened**

See the Gatekeeper notes above (`Right-click → Open`,
`xattr -dr com.apple.quarantine`).

**Finder still shows "cannot open" dialogs**

macOS caches document-type associations. Re-register the bundle:

```bash
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
"$LSREG" -u "$PWD/MD-Viewer.app"
"$LSREG" -f "$PWD/MD-Viewer.app"
```

Also make sure the app is launched from its `.app` bundle (Finder open events
are not delivered to a bare `cargo run` binary).

**Debugging when launched by Finder**

Finder-launched apps have no visible stderr. Redirect the log to a file:

```bash
launchctl setenv MD_VIEWER_LOG_FILE /tmp/md-viewer.log
launchctl setenv RUST_LOG info
open -a "$PWD/MD-Viewer.app" /path/to/document.md
# inspect /tmp/md-viewer.log, then:
launchctl unsetenv MD_VIEWER_LOG_FILE
launchctl unsetenv RUST_LOG
```

---

## License

MIT — see [LICENSE](LICENSE).
