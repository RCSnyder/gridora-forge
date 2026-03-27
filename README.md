# Photo Matrix WASM

A simple Rust/WASM browser app that lets a user:

- choose a folder of local photos or pick multiple image files
- load large image batches with parallel browser-side processing
- reorder the photos with drag-and-drop or Up / Down controls
- rotate photos before export
- preview them in a 2×2 or 2×3 matrix
- customize PDF margins plus header and footer text
- open a print-ready layout and save it as PDF from the browser print dialog
- reach support or feature-request links directly from the UI

## Simple map

1. Pick local images.
2. Reorder them.
3. Rotate or annotate them.
4. Choose `1-up`, `2-up`, `2 × 2`, or `2 × 3`.
5. Print / Save PDF.

## What is in this starter

- **Rust + Leptos CSR** browser frontend
- **No backend**
- **Folder picker + multi-file picker**
- **Parallel image ingest** with bounded concurrency
- **PDF path** via a print-ready browser window
- **Per-photo rotation** and **PDF margin/header/footer controls**
- **One-command local dev** through Trunk

## Why this shape

This is the simplest understandable MVP that stays browser-first:

- no install for end users once hosted
- no server upload requirement
- private local file handling
- PDF-first report output
- easy repo to extend with drag-and-drop, image resizing, crop controls, or persistent project saves later

## Local development on Windows

### Prerequisites

Install Rust and the WASM target:

```powershell
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

### Run

```powershell
.\scripts\dev.ps1
```

That runs:

```powershell
trunk serve
```

### Production build

```powershell
.\scripts\build.ps1
```

That writes the static site into `dist/`.

## Host it later

You can host the `dist/` directory on any static hosting provider.

## Notes

- Folder picking relies on browser support for directory inputs. The app also supports standard multi-file selection as a fallback.
- The PDF flow opens a print-friendly tab and uses the browser print dialog. Choose **Save as PDF** there.
- The app compresses export images during ingest so PDFs stay materially smaller than the original camera files while keeping print quality usable.
- Support and feature-request links are available in the left pane.

## Suggested next upgrades

- saved project drafts in browser storage
- direct PDF byte generation instead of print dialog
