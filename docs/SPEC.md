# SPEC — Gridora Forge

## What It Is

A browser-based tool for building photo reports. Drop in images, add titles and descriptions, arrange them in grid layouts, and export as Excel or PDF. No server, no uploads, no account — everything runs locally via WebAssembly.

## Problem Statement

Field professionals (construction PMs, insurance adjusters, property inspectors, engineers) routinely take dozens of photos at a job site. They then need to produce a structured document — typically called a **photo report**, **photo log**, or **inspection photo sheet** — where:

1. Each photo has a **user-written title** and **description/narrative** underneath it.
2. Photos are in a **deliberate order** (not camera roll order).
3. The output is a **portable document** (PDF or Excel) that can be emailed, printed, or attached to a formal report.

This is currently done by manually pasting images into Excel or Word, which is slow, error-prone, and tedious. The app eliminates that friction while keeping output format compatibility.

---

## Tech Stack

- **Rust + Leptos 0.8 CSR** compiled to WebAssembly (wasm32-unknown-unknown)
- **Trunk** for build/dev server
- **rust_xlsxwriter** (wasm feature) for Excel export
- **web-sys / js-sys** for browser APIs (Canvas, Drag, File, Blob, etc.)
- **heic2any** (CDN) for HEIC/HEIF → JPEG conversion
- No backend. No Node.js. No npm. Pure Rust → WASM.

---

## Primary Outcome (definition of "done")

A user can load site photos, write a title and description for each one, reorder them, and export a professional-looking PDF or Excel document — all in the browser, with no server, no uploads, no account.

---

## Users / Actors

| Actor                | Context                                                                                                                  |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| **Field inspector**  | Takes 10–80 photos per site visit. Needs to produce a report same-day or next-day. Works from a laptop or tablet.        |
| **Report recipient** | PM, adjuster, client, or regulator who receives the PDF/Excel. Never sees the app. Only cares about the output document. |

---

## Features (current state)

### Core — Implemented

| ID   | Feature                                                                |
| ---- | ---------------------------------------------------------------------- |
| F-01 | Load multiple image files (multi-select, folder, or drag-and-drop)     |
| F-02 | HEIC/HEIF support (auto-converts to JPEG)                              |
| F-03 | Editable title per photo (defaults to filename)                        |
| F-04 | Editable multi-line description per photo                              |
| F-05 | Drag-and-drop reorder (both left list and right preview)               |
| F-06 | Move up / move down / remove buttons per photo                         |
| F-07 | Four layout options: 1-up, 2-up, 2×2 grid, 2×3 grid                    |
| F-08 | Live paginated preview with page navigation                            |
| F-09 | Report metadata: title, site/address, author, date, notes              |
| F-10 | Export to Excel (.xlsx) with images, titles, descriptions, metadata    |
| F-11 | Export to PDF via print-ready HTML with base64-inlined images          |
| F-12 | Semantic export filenames based on report metadata                     |
| F-13 | Dark mode (system preference)                                          |
| F-14 | Virtual scrolling for large photo lists (160+ photos)                  |
| F-15 | Three-tier image pipeline: 128px thumbs, 600px previews, 2000px export |
| F-16 | Clear all with confirmation                                            |
| F-17 | Loading progress bar with per-file status                              |
| F-18 | Max 200 photo limit with feedback                                      |

### Future — Not Yet Built

| ID   | Feature                                                    |
| ---- | ---------------------------------------------------------- |
| F-20 | Persistence: save/restore work-in-progress (IndexedDB)     |
| F-21 | Auto-save on changes                                       |
| F-22 | Cover page with company logo                               |
| F-23 | Company branding: logo, header/footer on each page         |
| F-24 | Section dividers / grouping (e.g., "Exterior", "Interior") |
| F-25 | Image crop/rotate in-app                                   |
| F-26 | Bulk caption template (e.g., "Photo {n} — {filename}")     |
| F-27 | EXIF date/GPS extraction                                   |
| F-28 | Direct PDF byte generation (no print dialog)               |

---

## Non-Functional Requirements

| ID    | Requirement                                                                                | Rationale                                         |
| ----- | ------------------------------------------------------------------------------------------ | ------------------------------------------------- |
| NF-01 | **100% client-side** — no server, no uploads                                               | Privacy; no infra cost; works offline once loaded |
| NF-02 | Works in Chrome and Edge on Windows (primary); Safari/Firefox best-effort                  | Target audience is Windows-laptop field workers   |
| NF-03 | Handles 160+ photos at 5–30 MB each without lag                                            | Stress-tested with 12–48 MP images                |
| NF-04 | Export completes in < 10 seconds for 80 photos                                             | Must not feel broken                              |
| NF-05 | Page reload should not silently destroy 30 minutes of annotation work (once F-20 is built) | Major trust issue                                 |

---

## Data Model

```rust
ReportMeta {
    title: String,            // "123 Main St — Roof Inspection"
    site_address: String,     // "123 Main Street, Denver CO 80202"
    author: String,           // "Jane Smith, PE"
    date: String,             // "2026-03-21"
    notes: String,            // optional report-level narrative
}

PhotoItem {
    id: u64,
    title: String,            // user-written, defaults to filename
    description: String,      // user-written narrative, multi-line
    filename: String,         // original filename (for reference)
    mime: String,
    thumb_url: String,        // 128px JPEG data URL for left-pane list
    preview_url: String,      // 600px JPEG data URL for right-pane preview
    bytes: Arc<[u8]>,         // 2000px JPEG bytes for Excel/PDF export
}

GridLayout {
    OneUp,                    // 1 photo/page, full-width image + description
    TwoUp,                    // 2 photos/page, each with description
    TwoByTwo,                 // 4 photos/page, 2×2 grid
    TwoByThree,               // 6 photos/page, 2×3 grid
}
```

---

## Layout Behaviors

### 1-up (one photo per page)

- Full-width image (landscape or portrait, fit within page margins)
- Title below image (bold, larger font)
- Description below title (paragraph text, wraps)
- Best for: detailed narrative per photo, large images

### 2-up (two photos per page)

- Two images stacked vertically, each ~45% of page height
- Each has its own title + description
- Best for: moderate detail, common for insurance photo pages

### 2×2 grid (existing)

- 4 images in a 2×2 grid
- Title under each image (shorter, single-line encouraged)
- Description truncated or omitted in grid view (full text in export)
- Best for: high photo density, overview pages

### 2×3 grid (existing)

- 6 images in a 2×3 grid
- Title only (space is tight)
- Best for: maximum density, thumbnail-style appendix pages

---

## Export Formats

### Excel (.xlsx)

- One worksheet per page
- Image fit-to-cell
- Title in a merged cell below image
- Description in a merged cell below title (word-wrapped, auto-height)
- First worksheet optionally has report metadata as a header block
- **Why Excel matters:** Many construction/insurance workflows require Excel specifically because recipients paste sheets into larger workbooks, apply their own formatting, or have compliance templates. Do not remove Excel export.

### PDF (print-ready HTML → browser print dialog)

- Self-contained HTML with base64-inlined images
- Proper page breaks
- Report title + metadata on first page or as header
- Each photo with title + description
- Print CSS for letter-size pages
- **Future:** Direct PDF byte generation via `printpdf` or similar crate to avoid popup/print-dialog friction.

---

## Invariants (must always hold)

1. Photo order in the UI = photo order in exports. No silent reordering.
2. Every photo in the export has its user-written title and description (not just the filename).
3. Export never silently drops photos. If a photo can't be exported, surface an error.
4. No image data is sent to any server. All processing is local.
5. The app must not crash or hang on large batches (80+ photos). Degrade gracefully (e.g., thumbnail quality reduction for preview, lazy loading).

---

## Optimization Priorities (ordered)

1. **Caption/description UX** — this is the product. If it's painful to type titles and descriptions, nobody will use it.
2. **Export fidelity** — the output document must look professional and contain all user-entered data.
3. **Reordering speed** — must handle 40+ photos without frustration.
4. **Load speed** — large batches shouldn't freeze the UI.
5. **Persistence** — losing work is unacceptable once out of MVP.

---

## Explicit Non-Goals (for now)

- Server-side processing or storage
- User accounts or authentication
- Collaboration / multi-user editing
- Video support
- OCR or AI-generated descriptions
- Native mobile app
- Direct integration with cloud storage (Google Drive, Dropbox, etc.)

---

## Acceptable Failure Modes

| Scenario                                 | Acceptable behavior                                            |
| ---------------------------------------- | -------------------------------------------------------------- |
| Browser doesn't support directory picker | Falls back to multi-file picker (already handled)              |
| Image file is corrupted                  | Skip it, show warning in status bar                            |
| Export fails mid-generation              | Show error message, don't download partial file                |
| Browser blocks print popup               | Show clear instructions to allow popups                        |
| 80+ large photos overwhelm memory        | Show warning; suggest reducing photo resolution before loading |

---

## Architecture Notes

- **Single-file app**: All logic lives in `src/main.rs` (~1800 lines)
- **Image pipeline**: Each photo is decoded once via `createImageBitmap`, then rendered at 3 sizes (thumb/preview/export) using canvas `toDataURL`. No full-res images are stored in the DOM.
- **Virtual scrolling**: Left pane uses spacer divs + windowed `<For>` with `visible_range` memo. Only ~30 rows are in the DOM at any time.
- **Paginated preview**: Right pane renders one page at a time with nav buttons. Each page has at most 6 photo slots.
- **Reactive state**: `photos: RwSignal<Vec<PhotoItem>>` is the single source of truth. `photo_positions: Memo<HashMap<u64, usize>>` provides O(1) index lookups. `.with()` borrows are used in hot paths to avoid cloning.

---

## Success Metrics

- A user can go from "40 raw photos on disk" to "emailed PDF report with annotated photos" in under 15 minutes.
- The output document is indistinguishable in quality from one manually assembled in Excel/Word.
- Zero data leaves the browser.

## AI Tool Assistance

- VSCode Agent Mode with Claude Opus 4.6
- [BEE-OS Custom Chat-Mode/Agent](https://gist.github.com/RCSnyder/4ce8d44f5c77e1e00077393daeb391e6#file-gistfile1-txt)
