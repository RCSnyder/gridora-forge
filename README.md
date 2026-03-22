# Photo Matrix WASM

A simple Rust/WASM browser app that lets a user:

- choose a folder of local photos or pick multiple image files
- reorder the photos with Up / Down controls
- preview them in a 2×2 or 2×3 matrix
- export a real `.xlsx` workbook
- open a print-ready layout and save it as PDF from the browser print dialog

## Simple map

1. Pick local images.
2. Reorder them.
3. Choose `2 × 2` or `2 × 3`.
4. Export to Excel or Print / Save PDF.

## What is in this starter

- **Rust + Leptos CSR** browser frontend
- **No backend**
- **Folder picker + multi-file picker**
- **Excel export** via `rust_xlsxwriter`
- **PDF path** via a print-ready browser window
- **One-command local dev** through Trunk

## Why this shape

This is the simplest understandable MVP that stays browser-first:

- no install for end users once hosted
- no server upload requirement
- private local file handling
- real workbook output
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
- This starter keeps the image pipeline simple. It uses original image bytes for export and browser object URLs for preview. That keeps the code smaller for your first final repo.
- A strong next iteration would add actual image resizing, drag-and-drop reordering, captions, and per-page margin controls.

## Suggested next upgrades

- drag-and-drop ordering
- actual image resize/crop pipeline in Rust
- page settings and margins
- saved projects in browser storage
- direct PDF byte generation instead of print dialog
