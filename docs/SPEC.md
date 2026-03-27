# SPEC - Gridora Forge

## What It Is

A browser-based tool for building photo reports. Drop in images, add titles and descriptions, arrange them in grid layouts, rotate images when needed, and export as PDF. No server, no uploads, no account; everything runs locally via WebAssembly.

## Problem Statement

Field professionals such as construction PMs, insurance adjusters, property inspectors, and engineers routinely take dozens of photos at a job site. They then need to produce a structured photo report where:

1. Each photo has a user-written title and description.
2. Photos stay in a deliberate order.
3. The output is a portable PDF that can be emailed, printed, or attached to a formal report.

This is often done manually in Word or Excel, which is slow and error-prone. Gridora Forge removes that friction while keeping the workflow entirely local to the browser.

## Tech Stack

- Rust + Leptos 0.8 CSR compiled to WebAssembly
- Trunk for build/dev server
- web-sys / js-sys for browser APIs
- heic2any for HEIC/HEIF to JPEG conversion
- No backend, no Node.js, no npm

## Primary Outcome

A user can load site photos, write a title and description for each one, reorder and rotate them, customize PDF framing, and export a professional-looking PDF document entirely in the browser.

## Users / Actors

| Actor            | Context                                                                       |
| ---------------- | ----------------------------------------------------------------------------- |
| Field inspector  | Takes 10 to 80 photos per site visit and needs a same-day or next-day report. |
| Report recipient | Receives the generated PDF and only cares about the document quality.         |

## Features (Current State)

### Core

| ID   | Feature                                                                     |
| ---- | --------------------------------------------------------------------------- |
| F-01 | Load multiple image files via multi-select, folder picker, or drag-and-drop |
| F-02 | HEIC/HEIF support with automatic JPEG conversion                            |
| F-03 | Editable title per photo                                                    |
| F-04 | Editable multi-line description per photo                                   |
| F-05 | Drag-and-drop reordering in both list and preview                           |
| F-06 | Move up, move down, and remove actions per photo                            |
| F-07 | Four layouts: 1-up, 2-up, 2x2, 2x3                                          |
| F-08 | Live paginated preview                                                      |
| F-09 | Report metadata: title, site/address, author, date, notes                   |
| F-10 | PDF export via print-ready HTML                                             |
| F-11 | Semantic export filenames based on metadata                                 |
| F-12 | Dark UI                                                                     |
| F-13 | Virtual scrolling for large photo lists                                     |
| F-14 | Parallel file ingest with bounded concurrency                               |
| F-15 | Three-tier image pipeline: thumbnail, preview, compressed export bytes      |
| F-16 | Per-photo 90-degree rotation before export                                  |
| F-17 | Custom PDF margins                                                          |
| F-18 | Custom PDF header/footer templates                                          |
| F-19 | Built-in support email and GitHub issue links                               |
| F-20 | Max 200 photo limit with feedback                                           |

### Future

| ID   | Feature                                         |
| ---- | ----------------------------------------------- |
| F-30 | Save/restore work in progress                   |
| F-31 | Auto-save                                       |
| F-32 | Cover-page branding/logo                        |
| F-33 | Section dividers / grouping                     |
| F-34 | Image crop                                      |
| F-35 | Bulk caption templates                          |
| F-36 | EXIF extraction                                 |
| F-37 | Direct PDF byte generation without print dialog |

## Non-Functional Requirements

| ID    | Requirement                                                             | Rationale                            |
| ----- | ----------------------------------------------------------------------- | ------------------------------------ |
| NF-01 | 100 percent client-side with no uploads                                 | Privacy and zero infrastructure cost |
| NF-02 | Primary support target is Chrome and Edge on Windows                    | Matches field-user environment       |
| NF-03 | Handles 160+ large photos without becoming unusable                     | Core production workflow             |
| NF-04 | Export should complete fast enough to feel interactive for typical jobs | User trust                           |
| NF-05 | Photo order in the UI must remain the order in the PDF                  | Output fidelity                      |

## Data Model

```rust
ReportMeta {
    title: String,
    site_address: String,
    author: String,
    date: String,
    notes: String,
}

PdfSettings {
    margin_top_in: f64,
    margin_right_in: f64,
    margin_bottom_in: f64,
    margin_left_in: f64,
    header_template: String,
    footer_template: String,
}

PhotoItem {
    id: u64,
    title: String,
    description: String,
    filename: String,
    mime: String,
    rotation_quadrants: u8,
    thumb_url: String,
    preview_url: String,
    bytes: Arc<[u8]>,
}

GridLayout {
    OneUp,
    TwoUp,
    TwoByTwo,
    TwoByThree,
}
```

## Export Format

### PDF

- Print-ready HTML opened in a new browser window
- Compressed inlined JPEG image payloads to control PDF size
- Proper page breaks for letter-size output
- Report metadata on the cover page
- User-editable header/footer text with template tokens
- User-editable page margins
- Photo title and description preserved in export

Supported template tokens:

- {title}
- {site_address}
- {author}
- {date}
- {notes}
- {page}
- {total_pages}

## Invariants

1. Photo order in the UI equals photo order in export.
2. Rotation shown in preview is the rotation used in export.
3. Export does not silently drop photos.
4. No image data leaves the browser.
5. Large batches degrade gracefully rather than freezing the app outright.

## Architecture Notes

- Single-file application centered in src/main.rs
- Each photo is decoded once using createImageBitmap
- Export bytes are compressed adaptively to keep generated PDFs smaller
- Upload processing runs in parallel but preserves original file order in the final photo list
- Left pane uses virtual scrolling to limit DOM size
- Right pane renders one preview page at a time

## Success Metrics

- A user can go from raw site photos to a shareable PDF report in under 15 minutes.
- The generated PDF is clear, ordered, and suitable for emailing or printing.
- Zero photo data leaves the browser.
