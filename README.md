# Gridora Forge

**Free Construction Photo Report Generator**
Drag. Drop. Label. Export to PDF.
No installs · No uploads · 100% private in browser.

**Live:** [www.gridora-forge.com](https://www.gridora-forge.com/)

## What it does

Gridora Forge turns jobsite photos into professional PDF photo reports in minutes. Built for construction workers, site inspectors, consultants, engineers, and anyone who needs to document a site with photos.

1. Drag and drop photos (or pick a folder)
2. Add titles and descriptions
3. Choose a layout
4. Export to PDF

Everything runs locally in your browser — no server, no accounts, no uploads.

## Features

- **Multiple layouts** — 1-up, 2-up, 2×2, 2×3 grid
- **Cover page** — title, site address, author, date, notes, company logo
- **Photo controls** — drag-and-drop reorder, rotate, title + description per photo
- **PDF settings** — configurable margins, header/footer templates with `{title}`, `{author}`, `{date}`, `{page}`, `{total_pages}` tokens, page numbers
- **HEIC/iPhone support** — handles HEIC/HEIF from iOS cameras
- **Deferred JPEG compression** — full-resolution originals until export, then compressed for smaller PDFs
- **Parallel processing** — bounded concurrency for fast batch imports
- **100% client-side** — Rust/WASM, no backend, photos never leave your device

## Tech stack

- **Rust + Leptos 0.8 CSR** compiled to WebAssembly
- **Trunk** for build/dev tooling
- **No backend** — static site hosted on GitHub Pages

## Local development

### Prerequisites

```powershell
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

### Run

```powershell
.\scripts\dev.ps1
```

Opens at `http://localhost:8080/`.

### Production build

```powershell
.\scripts\build.ps1
```

Outputs static site to `dist/`.

## Hosting

The `dist/` directory can be deployed to any static host. Currently served via GitHub Pages with a custom domain (`www.gridora-forge.com`).

## License

[MIT](LICENSE)
