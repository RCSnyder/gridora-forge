use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use gloo_file::futures::read_as_bytes;
use js_sys::{Array, Uint8Array};
use leptos::prelude::*;
use rust_xlsxwriter::{Format, Image, Workbook};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Blob, BlobPropertyBag, CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement,
    HtmlDocument, HtmlElement, HtmlInputElement, HtmlTextAreaElement, ImageBitmap, Url,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_photo_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Default)]
struct ReportMeta {
    title: String,
    site_address: String,
    author: String,
    date: String,
    notes: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GridLayout {
    OneUp,
    TwoUp,
    TwoByTwo,
    TwoByThree,
}

impl GridLayout {
    fn label(self) -> &'static str {
        match self {
            GridLayout::OneUp => "1\u{2011}up",
            GridLayout::TwoUp => "2\u{2011}up",
            GridLayout::TwoByTwo => "2 \u{00d7} 2",
            GridLayout::TwoByThree => "2 \u{00d7} 3",
        }
    }

    fn rows(self) -> usize {
        match self {
            GridLayout::OneUp => 1,
            GridLayout::TwoUp => 2,
            GridLayout::TwoByTwo => 2,
            GridLayout::TwoByThree => 3,
        }
    }

    fn cols(self) -> usize {
        match self {
            GridLayout::OneUp | GridLayout::TwoUp => 1,
            GridLayout::TwoByTwo | GridLayout::TwoByThree => 2,
        }
    }

    fn page_size(self) -> usize {
        self.rows() * self.cols()
    }

    fn value(self) -> &'static str {
        match self {
            GridLayout::OneUp => "1up",
            GridLayout::TwoUp => "2up",
            GridLayout::TwoByTwo => "2x2",
            GridLayout::TwoByThree => "2x3",
        }
    }
}

#[derive(Clone, PartialEq)]
struct PhotoItem {
    id: u64,
    title: String,
    description: String,
    filename: String,
    mime: String,
    /// Small data-URL thumbnail (~128 px) for the left-pane list.
    thumb_url: String,
    /// Medium data-URL preview (~600 px) for the right-pane page cards.
    preview_url: String,
    bytes: Arc<[u8]>,
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

fn split_pages(items: &[PhotoItem], layout: GridLayout) -> Vec<Vec<Option<PhotoItem>>> {
    let mut pages = Vec::new();
    let page_size = layout.page_size();
    let mut idx = 0;

    while idx < items.len() {
        let mut page = Vec::with_capacity(page_size);
        for _ in 0..page_size {
            if idx < items.len() {
                page.push(Some(items[idx].clone()));
                idx += 1;
            } else {
                page.push(None);
            }
        }
        pages.push(page);
    }

    if pages.is_empty() {
        pages.push(vec![None; page_size]);
    }

    pages
}

fn move_item(items: &mut Vec<PhotoItem>, from: usize, to: usize) {
    if from >= items.len() || to >= items.len() || from == to {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
}

/// Strip file extension for a nicer default title.
fn title_from_filename(name: &str) -> String {
    match name.rfind('.') {
        Some(pos) if pos > 0 => name[..pos].to_string(),
        _ => name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Browser helpers
// ---------------------------------------------------------------------------

fn trigger_download(filename: &str, bytes: &[u8], mime: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("No browser window")?;
    let document = window.document().ok_or("No document")?;
    let anchor = document
        .create_element("a")
        .map_err(|_| "Failed to create anchor")?
        .dyn_into::<HtmlAnchorElement>()
        .map_err(|_| "Failed to cast anchor")?;

    let array = Uint8Array::new_with_length(bytes.len() as u32);
    array.copy_from(bytes);

    let parts = Array::new();
    parts.push(&array);

    let opts = BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &opts)
        .map_err(|_| "Failed to create blob")?;
    let url = Url::create_object_url_with_blob(&blob).map_err(|_| "Failed to create object URL")?;

    anchor.set_href(&url);
    anchor.set_download(filename);
    let anchor_el: &HtmlElement = anchor.as_ref();
    anchor_el.style().set_property("display", "none").ok();

    if let Some(body) = document.body() {
        body.append_child(&anchor).ok();
        anchor.click();
        body.remove_child(&anchor).ok();
    } else {
        anchor.click();
    }

    // Don't revoke the object URL immediately — the browser needs a moment
    // to start the download after click(). Leaking a single blob URL per
    // export is harmless; it will be GC'd when the page is unloaded.
    Ok(())
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Build a semantic export filename from report metadata.
/// e.g. "Site_Report-123_Main_St-John_Doe-2026-03-21-a1b2c3.xlsx"
fn export_filename(meta: &ReportMeta, ext: &str) -> String {
    // Collect non-empty metadata parts
    let mut parts: Vec<String> = Vec::new();

    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string()
    };

    // Truncate a sanitized part to ~30 chars at a word boundary
    let truncate = |s: String, max: usize| -> String {
        if s.len() <= max {
            return s;
        }
        let truncated = &s[..max];
        match truncated.rfind('_') {
            Some(pos) if pos > max / 2 => truncated[..pos].to_string(),
            _ => truncated.to_string(),
        }
    };

    if !meta.title.is_empty() {
        parts.push(truncate(sanitize(&meta.title), 30));
    }
    if !meta.site_address.is_empty() {
        parts.push(truncate(sanitize(&meta.site_address), 30));
    }
    if !meta.author.is_empty() {
        parts.push(truncate(sanitize(&meta.author), 20));
    }
    if !meta.date.is_empty() {
        parts.push(sanitize(&meta.date));
    }

    // Fallback if nothing is filled in
    if parts.is_empty() {
        parts.push("gridora-report".to_string());
    }

    // 6-char hash for uniqueness (from all meta fields + current time)
    let hash_input = format!(
        "{}|{}|{}|{}|{}|{}",
        meta.title,
        meta.site_address,
        meta.author,
        meta.date,
        meta.notes,
        js_sys::Date::now() as u64
    );
    let mut hash: u64 = 5381;
    for b in hash_input.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    let hash_str = format!("{:06x}", hash & 0xFFFFFF);

    format!("{}-{}.{}", parts.join("-"), hash_str, ext)
}

// ---------------------------------------------------------------------------
// Image processing
// ---------------------------------------------------------------------------

/// Max pixel dimension for export-ready images.
/// 2000px covers 200+ DPI at 1-up layout on 8.5×11 paper.
const EXPORT_MAX_DIM: u32 = 2000;

/// Check if a file is HEIC/HEIF by MIME type or extension.
/// Browsers (except Safari) typically report an empty MIME for HEIC files.
fn is_heic(file: &web_sys::File) -> bool {
    let mime = file.type_().to_lowercase();
    if mime.contains("heic") || mime.contains("heif") {
        return true;
    }
    let name = file.name().to_lowercase();
    name.ends_with(".heic") || name.ends_with(".heif")
}

/// Convert a HEIC/HEIF file to JPEG using the heic2any JS library.
/// Returns a new web_sys::File with JPEG data, or Err if conversion fails.
async fn convert_heic_to_jpeg(file: &web_sys::File) -> Result<web_sys::File, String> {
    let window = web_sys::window().ok_or("no window")?;
    let heic2any: js_sys::Function = js_sys::Reflect::get(&window, &"heic2any".into())
        .map_err(|_| "heic2any not loaded")?
        .dyn_into()
        .map_err(|_| "heic2any is not a function")?;

    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"blob".into(), file.as_ref());
    let _ = js_sys::Reflect::set(&opts, &"toType".into(), &"image/jpeg".into());
    let _ = js_sys::Reflect::set(&opts, &"quality".into(), &JsValue::from(0.92));

    let promise: js_sys::Promise = heic2any
        .call1(&JsValue::NULL, &opts)
        .map_err(|_| "heic2any call failed")?
        .unchecked_into();

    let result = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| format!("heic2any conversion failed: {:?}", e))?;

    // heic2any returns a single Blob (or array of Blobs for multi-image HEIF)
    let jpeg_blob: Blob = if result.is_instance_of::<js_sys::Array>() {
        let arr: js_sys::Array = result.unchecked_into();
        arr.get(0).unchecked_into()
    } else {
        result.unchecked_into()
    };

    // Construct a new File from the Blob with a .jpg name
    let original_name = file.name();
    let new_name = if let Some(pos) = original_name.rfind('.') {
        format!("{}.jpg", &original_name[..pos])
    } else {
        format!("{}.jpg", original_name)
    };

    let parts = js_sys::Array::new();
    parts.push(&jpeg_blob);
    let file_opts = web_sys::FilePropertyBag::new();
    file_opts.set_type("image/jpeg");
    web_sys::File::new_with_blob_sequence_and_options(&parts, &new_name, &file_opts)
        .map_err(|_| "Failed to create JPEG file from HEIC conversion".to_string())
}

/// Approximate height in pixels of each photo-row for virtual scroll calculations.
const ROW_HEIGHT: f64 = 64.0;
/// Extra rows rendered above/below the visible viewport.
const OVERSCAN: usize = 10;

/// Decode a File/Blob into an `ImageBitmap` using the browser's built-in decoder.
async fn create_bitmap(file: &web_sys::File) -> Result<ImageBitmap, String> {
    let window = web_sys::window().ok_or("no window")?;
    let blob: &Blob = file.as_ref();
    let promise = window
        .create_image_bitmap_with_blob(blob)
        .map_err(|_| "createImageBitmap unavailable")?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| "image decode failed".to_string())
        .map(|v| v.unchecked_into())
}

/// Draw an `ImageBitmap` onto a temporary canvas scaled to `max_dim` and return
/// a JPEG data-URL at the given quality (0.0–1.0).
fn draw_scaled_data_url(
    bitmap: &ImageBitmap,
    max_dim: u32,
    quality: f64,
) -> Result<String, String> {
    let document = web_sys::window()
        .ok_or("no window")?
        .document()
        .ok_or("no document")?;

    let orig_w = bitmap.width() as f64;
    let orig_h = bitmap.height() as f64;
    let scale = (max_dim as f64 / orig_w.max(orig_h)).min(1.0);
    let w = (orig_w * scale).round() as u32;
    let h = (orig_h * scale).round() as u32;

    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|_| "create canvas")?
        .unchecked_into();
    canvas.set_width(w);
    canvas.set_height(h);

    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|_| "get context")?
        .ok_or("no 2d context")?
        .unchecked_into();

    ctx.draw_image_with_image_bitmap_and_dw_and_dh(bitmap, 0.0, 0.0, w as f64, h as f64)
        .map_err(|_| "drawImage failed")?;

    let to_data_url_fn: js_sys::Function = js_sys::Reflect::get(&canvas, &"toDataURL".into())
        .map_err(|_| "no toDataURL")?
        .unchecked_into();
    to_data_url_fn
        .call2(
            &canvas,
            &JsValue::from_str("image/jpeg"),
            &JsValue::from(quality),
        )
        .map_err(|_| "toDataURL call failed")?
        .as_string()
        .ok_or_else(|| "toDataURL returned non-string".to_string())
}

// ---------------------------------------------------------------------------
// Excel export
// ---------------------------------------------------------------------------

fn build_xlsx(
    photos: &[PhotoItem],
    layout: GridLayout,
    meta: &ReportMeta,
) -> Result<Vec<u8>, String> {
    let mut workbook = Workbook::new();

    // -- optional cover / metadata sheet --------------------------------
    let has_meta = !meta.title.is_empty()
        || !meta.site_address.is_empty()
        || !meta.author.is_empty()
        || !meta.date.is_empty()
        || !meta.notes.is_empty();

    if has_meta {
        let ws = workbook.add_worksheet();
        ws.set_name("Report Info").map_err(|e| e.to_string())?;
        ws.set_column_width_pixels(0, 160)
            .map_err(|e| e.to_string())?;
        ws.set_column_width_pixels(1, 500)
            .map_err(|e| e.to_string())?;

        let bold = Format::new().set_bold();
        let mut r: u32 = 0;
        let fields: &[(&str, &str)] = &[
            ("Report Title", &meta.title),
            ("Site / Address", &meta.site_address),
            ("Prepared By", &meta.author),
            ("Date", &meta.date),
            ("Notes", &meta.notes),
        ];
        for (label, value) in fields {
            if !value.is_empty() {
                ws.write_string_with_format(r, 0, *label, &bold)
                    .map_err(|e| e.to_string())?;
                ws.write_string(r, 1, *value).map_err(|e| e.to_string())?;
                r += 1;
            }
        }
    }

    // -- photo pages ----------------------------------------------------
    let cols = layout.cols();

    for (page_idx, chunk) in photos.chunks(layout.page_size()).enumerate() {
        let worksheet = workbook.add_worksheet();
        worksheet
            .set_name(format!("Page {}", page_idx + 1))
            .map_err(|e| e.to_string())?;

        let bold = Format::new().set_bold();
        let wrap = Format::new().set_text_wrap();

        match layout {
            GridLayout::OneUp | GridLayout::TwoUp => {
                // Single column: image row, title row, description row per photo
                worksheet
                    .set_column_width_pixels(0, 500)
                    .map_err(|e| e.to_string())?;

                let mut r: u32 = 0;
                for photo in chunk {
                    // Image row
                    worksheet
                        .set_row_height_pixels(r, 340)
                        .map_err(|e| e.to_string())?;
                    let image = Image::new_from_buffer(&photo.bytes).map_err(|e| e.to_string())?;
                    worksheet
                        .insert_image_fit_to_cell(r, 0, &image, false)
                        .map_err(|e| e.to_string())?;
                    r += 1;

                    // Title row
                    worksheet
                        .set_row_height_pixels(r, 24)
                        .map_err(|e| e.to_string())?;
                    worksheet
                        .write_string_with_format(r, 0, &photo.title, &bold)
                        .map_err(|e| e.to_string())?;
                    r += 1;

                    // Description row
                    if !photo.description.is_empty() {
                        let line_count = photo.description.lines().count().max(1) as u32;
                        worksheet
                            .set_row_height_pixels(r, 18 * line_count + 8)
                            .map_err(|e| e.to_string())?;
                        worksheet
                            .write_string_with_format(r, 0, &photo.description, &wrap)
                            .map_err(|e| e.to_string())?;
                    }
                    r += 1;

                    // Spacer
                    r += 1;
                }
            }
            GridLayout::TwoByTwo | GridLayout::TwoByThree => {
                for col in 0..cols {
                    worksheet
                        .set_column_width_pixels(col as u16, 280)
                        .map_err(|e| e.to_string())?;
                }

                // Each grid row: image row (180px), title row (22px), desc row (40px)
                for row in 0..layout.rows() {
                    let image_row = (row * 3) as u32;
                    let title_row = image_row + 1;
                    let desc_row = image_row + 2;
                    worksheet
                        .set_row_height_pixels(image_row, 180)
                        .map_err(|e| e.to_string())?;
                    worksheet
                        .set_row_height_pixels(title_row, 22)
                        .map_err(|e| e.to_string())?;
                    worksheet
                        .set_row_height_pixels(desc_row, 40)
                        .map_err(|e| e.to_string())?;
                }

                for (slot_idx, photo) in chunk.iter().enumerate() {
                    let row = slot_idx / cols;
                    let col = slot_idx % cols;
                    let image_row = (row * 3) as u32;
                    let title_row = image_row + 1;
                    let desc_row = image_row + 2;

                    let image = Image::new_from_buffer(&photo.bytes).map_err(|e| e.to_string())?;
                    worksheet
                        .insert_image_fit_to_cell(image_row, col as u16, &image, false)
                        .map_err(|e| e.to_string())?;
                    worksheet
                        .write_string_with_format(title_row, col as u16, &photo.title, &bold)
                        .map_err(|e| e.to_string())?;
                    if !photo.description.is_empty() {
                        worksheet
                            .write_string_with_format(
                                desc_row,
                                col as u16,
                                &photo.description,
                                &wrap,
                            )
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
        }
    }

    workbook.save_to_buffer().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// PDF / print HTML export
// ---------------------------------------------------------------------------

fn build_print_html(photos: &[PhotoItem], layout: GridLayout, meta: &ReportMeta) -> String {
    let mut body_html = String::new();

    // -- cover / metadata block -----------------------------------------
    let has_meta = !meta.title.is_empty()
        || !meta.site_address.is_empty()
        || !meta.author.is_empty()
        || !meta.date.is_empty()
        || !meta.notes.is_empty();

    if has_meta {
        body_html.push_str(r#"<section class="page cover-page">"#);
        if !meta.title.is_empty() {
            body_html.push_str(&format!(
                r#"<h1 class="cover-title">{}</h1>"#,
                html_escape(&meta.title)
            ));
        }
        let detail_fields: &[(&str, &str)] = &[
            ("Site / Address", &meta.site_address),
            ("Prepared By", &meta.author),
            ("Date", &meta.date),
        ];
        for (label, value) in detail_fields {
            if !value.is_empty() {
                body_html.push_str(&format!(
                    r#"<p class="cover-detail"><strong>{}:</strong> {}</p>"#,
                    label,
                    html_escape(value)
                ));
            }
        }
        if !meta.notes.is_empty() {
            body_html.push_str(&format!(
                r#"<div class="cover-notes"><strong>Notes:</strong><p>{}</p></div>"#,
                html_escape(&meta.notes).replace('\n', "<br/>")
            ));
        }
        body_html.push_str("</section>");
    }

    // -- photo pages ----------------------------------------------------
    match layout {
        GridLayout::OneUp | GridLayout::TwoUp => {
            for (page_idx, page) in split_pages(photos, layout).iter().enumerate() {
                body_html.push_str(&format!(
                    r#"<section class="page"><div class="page-label">Page {}</div>"#,
                    page_idx + 1
                ));
                for photo in page.iter().flatten() {
                    let encoded = BASE64.encode(&photo.bytes);
                    body_html.push_str(&format!(
                        r#"<div class="photo-block layout-{}"><img src="data:{};base64,{}" alt="{}" /><div class="caption"><strong>{}</strong>"#,
                        layout.value(),
                        photo.mime,
                        encoded,
                        html_escape(&photo.title),
                        html_escape(&photo.title),
                    ));
                    if !photo.description.is_empty() {
                        body_html.push_str(&format!(
                            "<p>{}</p>",
                            html_escape(&photo.description).replace('\n', "<br/>")
                        ));
                    }
                    body_html.push_str("</div></div>");
                }
                body_html.push_str("</section>");
            }
        }
        GridLayout::TwoByTwo | GridLayout::TwoByThree => {
            for (page_idx, page) in split_pages(photos, layout).iter().enumerate() {
                body_html.push_str(&format!(
                    r#"<section class="page"><div class="page-label">Page {}</div><div class="grid rows-{}">"#,
                    page_idx + 1,
                    layout.rows()
                ));
                for slot in page {
                    match slot {
                        Some(photo) => {
                            let encoded = BASE64.encode(&photo.bytes);
                            body_html.push_str(&format!(
                                r#"<figure class="cell"><img src="data:{};base64,{}" alt="{}" /><figcaption><strong>{}</strong>"#,
                                photo.mime,
                                encoded,
                                html_escape(&photo.title),
                                html_escape(&photo.title),
                            ));
                            if !photo.description.is_empty() {
                                body_html.push_str(&format!(
                                    r#"<br/><span class="desc">{}</span>"#,
                                    html_escape(&photo.description)
                                ));
                            }
                            body_html.push_str("</figcaption></figure>");
                        }
                        None => {
                            body_html.push_str(
                                r#"<div class="cell empty-cell"><span>Empty</span></div>"#,
                            );
                        }
                    }
                }
                body_html.push_str("</div></section>");
            }
        }
    }

    // Build a suggested filename for the PDF <title> tag
    let pdf_filename = export_filename(meta, "pdf");
    let title_esc = html_escape(if meta.title.is_empty() {
        &pdf_filename
    } else {
        &meta.title
    });

    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<title>{title} — Gridora Forge Report</title>
<style>
* {{ box-sizing: border-box; }}
body {{ margin: 0; font-family: Arial, sans-serif; color: #111; }}
.page {{
  width: 8.5in;
  min-height: 11in;
  padding: 0.5in;
  page-break-after: always;
}}
.page:last-child {{ page-break-after: auto; }}
.cover-page {{ display: flex; flex-direction: column; justify-content: center; }}
.cover-title {{ font-size: 28px; margin-bottom: 0.3in; }}
.cover-detail {{ font-size: 14px; margin: 4px 0; }}
.cover-notes {{ margin-top: 0.3in; font-size: 13px; }}
.cover-notes p {{ margin: 6px 0 0 0; }}
.page-label {{ margin-bottom: 0.15in; color: #555; font-size: 11px; }}
.grid {{
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 0.18in;
}}
.grid.rows-2 .cell {{ height: 4.5in; }}
.grid.rows-3 .cell {{ height: 2.9in; }}
.cell {{
  border: 1px solid #ccc;
  border-radius: 6px;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}}
figure {{ margin: 0; }}
.cell img {{
  width: 100%;
  flex: 1 1 0;
  object-fit: contain;
  display: block;
  min-height: 0;
  background: #f5f5f5;
}}
.cell figcaption {{
  padding: 4px 8px;
  font-size: 11px;
  color: #333;
  line-height: 1.3;
  word-break: break-all;
  overflow-wrap: anywhere;
  flex: 0 0 auto;
}}
.cell figcaption .desc {{ font-weight: normal; color: #555; }}
.empty-cell {{
  align-items: center;
  justify-content: center;
  background: #fafafa;
  color: #999;
}}
.photo-block {{
  margin-bottom: 0.25in;
  border: 1px solid #ddd;
  border-radius: 6px;
  overflow: hidden;
}}
.photo-block.layout-1up img {{
  width: 100%;
  max-height: 7in;
  object-fit: contain;
  display: block;
  background: #f5f5f5;
}}
.photo-block.layout-2up img {{
  width: 100%;
  max-height: 3.5in;
  object-fit: contain;
  display: block;
  background: #f5f5f5;
}}
.caption {{
  padding: 8px 10px;
  font-size: 12px;
  line-height: 1.4;
  word-break: break-all;
  overflow-wrap: anywhere;
}}
.caption p {{ margin: 4px 0 0 0; color: #444; word-break: break-all; overflow-wrap: anywhere; }}
@media print {{
  body {{ print-color-adjust: exact; -webkit-print-color-adjust: exact; }}
  .page {{ page-break-inside: avoid; }}
}}
</style>
</head>
<body>
{body}
<script>
// Wait for all base64 images to decode before opening print dialog
window.onload = function() {{ window.print(); }};
</script>
</body>
</html>"#,
        title = title_esc,
        body = body_html
    )
}

// ---------------------------------------------------------------------------
// App component
// ---------------------------------------------------------------------------

#[component]
fn App() -> impl IntoView {
    let photos = RwSignal::new(Vec::<PhotoItem>::new());
    let layout = RwSignal::new(GridLayout::TwoByTwo);
    let meta = RwSignal::new(ReportMeta::default());
    let status = RwSignal::new(String::from(
        "Ready \u{2014} drop images here or click \u{201c}Add Photos\u{201d} to start.",
    ));
    let drag_idx = RwSignal::new(Option::<usize>::None);
    let drop_indicator = RwSignal::new(Option::<usize>::None);
    let photo_positions: Memo<HashMap<u64, usize>> = Memo::new(move |_| {
        photos.with(|items| items.iter().enumerate().map(|(i, p)| (p.id, i)).collect())
    });
    let clear_pending = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let drop_hover = RwSignal::new(false);
    let progress = RwSignal::new((0usize, 0usize)); // (current, total)
    let preview_page = RwSignal::new(0usize);
    let total_pages: Memo<usize> = Memo::new(move |_| {
        let count = photos.with(|v| v.len());
        let ps = layout.get().page_size();
        if count == 0 {
            1
        } else {
            count.div_ceil(ps)
        }
    });
    // Auto-clamped page index: always valid even when photos/layout shrink
    let clamped_page: Memo<usize> =
        Memo::new(move |_| preview_page.get().min(total_pages.get().saturating_sub(1)));

    // Virtual scroll state for the left-pane photo list
    let list_scroll_top = RwSignal::new(0.0f64);
    let list_client_height = RwSignal::new(800.0f64);
    let left_pane_ref = NodeRef::<leptos::html::Div>::new();
    let visible_range: Memo<(usize, usize)> = Memo::new(move |_| {
        let total = photos.with(|v| v.len());
        if total == 0 {
            return (0, 0);
        }
        let scroll = list_scroll_top.get();
        let height = list_client_height.get();
        // Subtract ~80px for meta panel + section bar above the photo list
        let effective_scroll = (scroll - 80.0).max(0.0);
        let start = ((effective_scroll / ROW_HEIGHT) as usize).saturating_sub(OVERSCAN);
        let visible_count = ((height / ROW_HEIGHT).ceil() as usize) + OVERSCAN * 2 + 2;
        let end = (start + visible_count).min(total);
        (start, end)
    });

    // --- shared file-processing logic ----------------------------------
    let process_files = move |raw_files: Vec<web_sys::File>| {
        if raw_files.is_empty() {
            status.set("No image files found.".to_string());
            return;
        }
        let max_total: usize = 200;
        let current_count = photos.with(|v| v.len());
        let available = max_total.saturating_sub(current_count);
        if available == 0 {
            status.set(format!("Maximum of {} photos reached.", max_total));
            return;
        }
        let original_count = raw_files.len();
        let raw_files: Vec<_> = raw_files.into_iter().take(available).collect();
        if raw_files.len() < original_count {
            status.set(format!(
                "Loading {} of {} image(s) (max {})\u{2026}",
                raw_files.len(),
                original_count,
                max_total
            ));
        } else {
            status.set(format!("Loading {} image(s)\u{2026}", raw_files.len()));
        }
        loading.set(true);
        progress.set((0, raw_files.len()));

        spawn_local(async move {
            let mut loaded = Vec::new();
            let total = raw_files.len();

            for (idx, file) in raw_files.into_iter().enumerate() {
                let fname = file.name();
                progress.set((idx + 1, total));
                status.set(format!("Processing {}/{}: {}", idx + 1, total, &fname));

                // Convert HEIC/HEIF to JPEG before processing
                let file = if is_heic(&file) {
                    status.set(format!("Converting HEIC {}/{}: {}", idx + 1, total, &fname));
                    match convert_heic_to_jpeg(&file).await {
                        Ok(converted) => converted,
                        Err(e) => {
                            web_sys::console::warn_1(
                                &format!("HEIC conversion failed for {}: {}", fname, e).into(),
                            );
                            continue;
                        }
                    }
                } else {
                    file
                };

                // Decode the image once; derive thumbnail, preview, and export
                // bytes from the same ImageBitmap to avoid repeated full-res decodes.
                let bitmap = match create_bitmap(&file).await {
                    Ok(b) => b,
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("Image decode failed for {}: {}", fname, e).into(),
                        );
                        continue;
                    }
                };

                let thumb_url = draw_scaled_data_url(&bitmap, 128, 0.7).unwrap_or_default();
                let preview_url = draw_scaled_data_url(&bitmap, 600, 0.8).unwrap_or_default();

                let export_url = draw_scaled_data_url(&bitmap, EXPORT_MAX_DIM, 0.85);
                let (bytes, mime) = if let Ok(ref data_url) = export_url {
                    if let Some((_, b64)) = data_url.split_once(',') {
                        match BASE64.decode(b64) {
                            Ok(b) => (b, "image/jpeg".to_string()),
                            Err(_) => {
                                match read_as_bytes(&gloo_file::File::from(file.clone())).await {
                                    Ok(raw) => (raw, file.type_()),
                                    Err(_) => continue,
                                }
                            }
                        }
                    } else {
                        match read_as_bytes(&gloo_file::File::from(file.clone())).await {
                            Ok(raw) => (raw, file.type_()),
                            Err(_) => continue,
                        }
                    }
                } else {
                    match read_as_bytes(&gloo_file::File::from(file.clone())).await {
                        Ok(raw) => (raw, file.type_()),
                        Err(_) => continue,
                    }
                };

                loaded.push(PhotoItem {
                    id: next_photo_id(),
                    title: title_from_filename(&fname),
                    description: String::new(),
                    filename: fname,
                    mime,
                    thumb_url,
                    preview_url,
                    bytes: bytes.into(),
                });
            }

            let loaded_count = loaded.len();
            photos.update(|items| items.extend(loaded));
            status.set(format!("Loaded {} image(s).", loaded_count));
            loading.set(false);
            progress.set((0, 0));
        });
    };

    // --- file input handler --------------------------------------------
    let on_files = move |ev: web_sys::Event| {
        let input = event_target::<HtmlInputElement>(&ev);
        let Some(list) = input.files() else {
            return;
        };

        let mut raw_files = Vec::new();
        for idx in 0..list.length() {
            if let Some(file) = list.get(idx) {
                if file.type_().starts_with("image/") || is_heic(&file) {
                    raw_files.push(file);
                }
            }
        }

        process_files(raw_files);
    };

    // --- drag-and-drop file adding ------------------------------------
    let on_drop_files = move |ev: web_sys::DragEvent| {
        ev.prevent_default();
        drop_hover.set(false);

        if let Some(dt) = ev.data_transfer() {
            if let Some(list) = dt.files() {
                let mut raw_files = Vec::new();
                for idx in 0..list.length() {
                    if let Some(file) = list.get(idx) {
                        if file.type_().starts_with("image/") || is_heic(&file) {
                            raw_files.push(file);
                        }
                    }
                }
                process_files(raw_files);
            }
        }
    };

    // --- export handlers -----------------------------------------------
    let export_xlsx = move |_| {
        if loading.get() {
            return;
        }
        let current = photos.get();
        if current.is_empty() {
            status.set("Add photos before exporting.".to_string());
            return;
        }

        loading.set(true);
        status.set("Generating Excel\u{2026}".to_string());

        // Run in spawn_local so the status message has a chance to paint,
        // and so any WASM panic gets surfaced via console_error_panic_hook
        // instead of silently aborting the synchronous closure.
        let layout_val = layout.get();
        let m = meta.get();
        let status_signal = status;
        spawn_local(async move {
            let result = build_xlsx(&current, layout_val, &m);
            match result {
                Ok(bytes) => {
                    web_sys::console::log_1(
                        &format!("xlsx generated: {} bytes", bytes.len()).into(),
                    );
                    let fname = export_filename(&m, "xlsx");
                    if let Err(err) = trigger_download(
                        &fname,
                        &bytes,
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    ) {
                        status_signal.set(format!("Excel export failed: {err}"));
                    } else {
                        status_signal.set("Excel file downloaded.".to_string());
                    }
                }
                Err(err) => {
                    web_sys::console::error_1(&format!("xlsx build error: {err}").into());
                    status_signal.set(format!("Excel export failed: {err}"));
                }
            }
            loading.set(false);
        });
    };

    let export_pdf = move |_| {
        if loading.get() {
            return;
        }
        let current = photos.get();
        if current.is_empty() {
            status.set("Add photos before exporting.".to_string());
            return;
        }

        let m = meta.get();
        let html = build_print_html(&current, layout.get(), &m);
        let Some(window) = web_sys::window() else {
            status.set("Could not access the browser window.".to_string());
            return;
        };

        match window.open_with_url_and_target("about:blank", "_blank") {
            Ok(Some(print_window)) => {
                if let Some(doc) = print_window.document() {
                    let html_doc: HtmlDocument = doc.unchecked_into();
                    let _ = html_doc.open();
                    let _ = html_doc.write(&js_sys::Array::of1(&html.into()));
                    let _ = html_doc.close();
                    // Don't call print_window.print() here — the <script> in
                    // the written HTML will call window.print() after onload,
                    // ensuring all base64 images have been decoded.
                    status.set(
                        "Print dialog opened. Choose \u{201c}Save as PDF\u{201d} in the browser print dialog."
                            .to_string(),
                    );
                } else {
                    status.set("Could not open print document.".to_string());
                }
            }
            _ => status
                .set("Popup was blocked. Allow popups for this app and try again.".to_string()),
        }
    };

    // --- folder input ref (for webkitdirectory) -------------------------
    let folder_ref = NodeRef::<leptos::html::Input>::new();
    let file_input_ref = NodeRef::<leptos::html::Input>::new();
    let xlsx_btn_ref = NodeRef::<leptos::html::Button>::new();
    let pdf_btn_ref = NodeRef::<leptos::html::Button>::new();
    Effect::new(move |_| {
        if let Some(el) = folder_ref.get() {
            let _ = el.set_attribute("webkitdirectory", "");
        }
    });

    // --- keyboard shortcuts (one-time setup) ---------------------------
    // Ctrl+O = add files, Ctrl+E = export xlsx, Ctrl+P = print/pdf
    {
        use wasm_bindgen::closure::Closure;
        let window = web_sys::window().unwrap();
        let handler =
            Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
                let ctrl = ev.ctrl_key() || ev.meta_key();
                if !ctrl {
                    return;
                }
                match ev.key().as_str() {
                    "o" | "O" => {
                        ev.prevent_default();
                        if let Some(el) = file_input_ref.get() {
                            el.click();
                        }
                    }
                    "e" | "E" => {
                        ev.prevent_default();
                        if let Some(el) = xlsx_btn_ref.get() {
                            el.click();
                        }
                    }
                    "p" | "P" => {
                        ev.prevent_default();
                        if let Some(el) = pdf_btn_ref.get() {
                            el.click();
                        }
                    }
                    _ => {}
                }
            });
        window
            .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
            .ok();
        handler.forget(); // Intentional: lives for the lifetime of the SPA

        // Global dragover: prevent no-drop cursor, track cursor position for auto-scroll
        let cursor_y = Rc::new(Cell::new(0i32));
        let cursor_x = Rc::new(Cell::new(0i32));
        let cursor_y2 = cursor_y.clone();
        let cursor_x2 = cursor_x.clone();
        let drag_handler =
            Closure::<dyn Fn(web_sys::DragEvent)>::new(move |ev: web_sys::DragEvent| {
                if drag_idx.get().is_some() {
                    ev.prevent_default();
                    cursor_y2.set(ev.client_y());
                    cursor_x2.set(ev.client_x());
                    if let Some(dt) = ev.data_transfer() {
                        dt.set_drop_effect("move");
                    }
                }
            });
        window
            .add_event_listener_with_callback("dragover", drag_handler.as_ref().unchecked_ref())
            .ok();
        drag_handler.forget();

        // Interval-based auto-scroll during drag (16ms ≈ 60fps)
        // Browsers suppress wheel events during native drag, so we scroll
        // based on cursor proximity to pane edges.
        let scroll_cb = Closure::<dyn Fn()>::new(move || {
            if drag_idx.get().is_none() {
                return;
            }
            let y = cursor_y.get() as f64;
            let x = cursor_x.get() as f32;
            let doc = web_sys::window().unwrap().document().unwrap();
            // Find the pane under the cursor
            if let Some(el) = doc.element_from_point(x, cursor_y.get() as f32) {
                let mut current: Option<web_sys::Element> = Some(el);
                while let Some(node) = current {
                    let cls = node.class_name();
                    if cls.contains("left-pane") || cls.contains("right-pane") {
                        let rect = node.get_bounding_client_rect();
                        let edge = 80.0;
                        let dist_top = y - rect.top();
                        let dist_bottom = rect.bottom() - y;
                        let speed = if dist_top < edge {
                            // Scroll up: faster closer to edge
                            -((edge - dist_top) / edge * 20.0)
                        } else if dist_bottom < edge {
                            // Scroll down
                            (edge - dist_bottom) / edge * 20.0
                        } else {
                            0.0
                        };
                        if speed.abs() > 0.5 {
                            let html: web_sys::HtmlElement = node.unchecked_into();
                            html.set_scroll_top(html.scroll_top() + speed as i32);
                        }
                        return;
                    }
                    current = node.parent_element();
                }
            }
        });
        window
            .set_interval_with_callback_and_timeout_and_arguments_0(
                scroll_cb.as_ref().unchecked_ref(),
                16,
            )
            .ok();
        scroll_cb.forget();
    }

    // --- view ----------------------------------------------------------
    view! {
        <div class="app">
            <header class="toolbar">
                <span class="brand">"Gridora Forge"</span>
                <div class="toolbar-group">
                    <label class="btn btn-primary">
                        "\u{1F4F7} Add Photos"
                        <input type="file" multiple accept="image/*,.heic,.heif" node_ref=file_input_ref on:change=on_files />
                    </label>
                    <label class="btn btn-secondary">
                        "\u{1F4C1} Folder"
                        <input type="file" multiple accept="image/*,.heic,.heif" node_ref=folder_ref on:change=on_files />
                    </label>
                    <span class="toolbar-sep">"|"</span>
                    <div class="layout-picker">
                        <button
                            class=move || if layout.get() == GridLayout::OneUp { "layout-btn active" } else { "layout-btn" }
                            on:click=move |_| layout.set(GridLayout::OneUp)
                            title="1-up layout"
                        >
                            <svg viewBox="0 0 20 20" width="16" height="16">
                                <rect x="3" y="2" width="14" height="16" rx="1" fill="currentColor" opacity="0.7" />
                            </svg>
                        </button>
                        <button
                            class=move || if layout.get() == GridLayout::TwoUp { "layout-btn active" } else { "layout-btn" }
                            on:click=move |_| layout.set(GridLayout::TwoUp)
                            title="2-up layout"
                        >
                            <svg viewBox="0 0 20 20" width="16" height="16">
                                <rect x="3" y="2" width="14" height="7" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="3" y="11" width="14" height="7" rx="1" fill="currentColor" opacity="0.7" />
                            </svg>
                        </button>
                        <button
                            class=move || if layout.get() == GridLayout::TwoByTwo { "layout-btn active" } else { "layout-btn" }
                            on:click=move |_| layout.set(GridLayout::TwoByTwo)
                            title="2\u{00d7}2 grid"
                        >
                            <svg viewBox="0 0 20 20" width="16" height="16">
                                <rect x="2" y="2" width="7" height="7" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="11" y="2" width="7" height="7" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="2" y="11" width="7" height="7" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="11" y="11" width="7" height="7" rx="1" fill="currentColor" opacity="0.7" />
                            </svg>
                        </button>
                        <button
                            class=move || if layout.get() == GridLayout::TwoByThree { "layout-btn active" } else { "layout-btn" }
                            on:click=move |_| layout.set(GridLayout::TwoByThree)
                            title="2\u{00d7}3 grid"
                        >
                            <svg viewBox="0 0 20 20" width="16" height="16">
                                <rect x="2" y="1" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="11" y="1" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="2" y="7.5" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="11" y="7.5" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="2" y="14" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                                <rect x="11" y="14" width="7" height="5" rx="1" fill="currentColor" opacity="0.7" />
                            </svg>
                        </button>
                    </div>
                    <span class="toolbar-sep">"|"</span>
                    <button class="btn-export" node_ref=xlsx_btn_ref on:click=export_xlsx disabled=move || loading.get()>"\u{1F4CA} Excel"</button>
                    <button class="btn-export" node_ref=pdf_btn_ref on:click=export_pdf disabled=move || loading.get()>"\u{1F5A8} PDF"</button>
                    <button
                        class=move || if clear_pending.get() { "btn-danger btn-confirm" } else { "btn-danger" }
                        on:click=move |_| {
                            if clear_pending.get() {
                                photos.set(Vec::new());
                                status.set("Cleared all photos.".to_string());
                                clear_pending.set(false);
                            } else {
                                clear_pending.set(true);
                                status.set("Click again to confirm clear.".to_string());
                                // Auto-reset after 3 seconds
                                let cp = clear_pending;
                                let st = status;
                                spawn_local(async move {
                                    gloo_timers::future::TimeoutFuture::new(3_000).await;
                                    if cp.get() {
                                        cp.set(false);
                                        st.set("Clear cancelled.".to_string());
                                    }
                                });
                            }
                        }
                    >{move || if clear_pending.get() { "Confirm?" } else { "Clear" }}</button>
                </div>
            </header>
            <div class=move || {
                let s = status.get();
                if s.contains("failed") || s.contains("blocked") || s.contains("error") {
                    "status-bar status-error"
                } else if s.contains('\u{2026}') || s.contains("Processing") {
                    "status-bar status-progress"
                } else if s.contains("downloaded") || s.contains("Loaded") || s.contains("Cleared") {
                    "status-bar status-success"
                } else {
                    "status-bar"
                }
            }>{move || status.get()}</div>

            <div class="workspace"
                on:dragover=move |ev: web_sys::DragEvent| {
                    ev.prevent_default();
                    // Only show file-drop overlay for external drags,
                    // not when reordering photos internally
                    if drag_idx.get().is_none() {
                        drop_hover.set(true);
                    }
                }
                on:dragleave=move |ev: web_sys::DragEvent| {
                    // Only reset when leaving the workspace itself
                    let related = ev.related_target();
                    if related.is_none() {
                        drop_hover.set(false);
                    }
                }
                on:drop=on_drop_files
            >
                // ── Drop overlay ──
                <Show when=move || drop_hover.get()>
                    <div class="drop-overlay"
                        on:dragleave=move |_| drop_hover.set(false)
                    >
                        <div class="drop-overlay-inner">
                            <span class="drop-icon">"\u{1F4F7}"</span>
                            <span class="drop-text">"Drop images to add"</span>
                        </div>
                    </div>
                </Show>

                // ── Left pane: meta + photo list ──
                <div class="left-pane"
                    node_ref=left_pane_ref
                    on:scroll=move |_| {
                        if let Some(el) = left_pane_ref.get() {
                            let html_el: &web_sys::HtmlElement = &el;
                            list_scroll_top.set(html_el.scroll_top() as f64);
                            list_client_height.set(html_el.client_height() as f64);
                        }
                    }
                >
                    <details class="meta-panel">
                        <summary>"Report Info"</summary>
                        <div class="meta-grid">
                            <label class="meta-field">
                                <span>"Title"</span>
                                <input
                                    type="text"
                                    placeholder="Report title"
                                    prop:value=move || meta.get().title.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlInputElement>(&ev).value();
                                        meta.update(|m| m.title = v);
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Site / Address"</span>
                                <input
                                    type="text"
                                    placeholder="Site address"
                                    prop:value=move || meta.get().site_address.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlInputElement>(&ev).value();
                                        meta.update(|m| m.site_address = v);
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Prepared By"</span>
                                <input
                                    type="text"
                                    placeholder="Author"
                                    prop:value=move || meta.get().author.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlInputElement>(&ev).value();
                                        meta.update(|m| m.author = v);
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Date"</span>
                                <input
                                    type="date"
                                    prop:value=move || meta.get().date.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlInputElement>(&ev).value();
                                        meta.update(|m| m.date = v);
                                    }
                                />
                            </label>
                            <label class="meta-field full-width">
                                <span>"Notes"</span>
                                <textarea
                                    rows="2"
                                    placeholder="Report-level notes"
                                    prop:value=move || meta.get().notes.clone()
                                    on:input=move |ev| {
                                        let el = event_target::<HtmlTextAreaElement>(&ev);
                                        let style = web_sys::HtmlElement::from(el.clone()).style();
                                        let _ = style.set_property("height", "auto");
                                        let _ = style.set_property("height", &format!("{}px", el.scroll_height()));
                                        let v = el.value();
                                        meta.update(|m| m.notes = v);
                                    }
                                ></textarea>
                            </label>
                        </div>
                    </details>

                    <div class="section-bar">
                        <span class="section-label">"Photos"</span>
                        <span class="section-count">{move || format!("({})", photos.with(|v| v.len()))}</span>
                    </div>

                    <Show
                        when=move || !photos.with(|v| v.is_empty())
                        fallback=move || view! {
                            <label class="empty-drop-zone">
                                <span class="drop-zone-icon">"\u{1F4F7}"</span>
                                <span class="drop-zone-text">"Drop images here, or click to browse"</span>
                                <input type="file" multiple accept="image/*,.heic,.heif" on:change=on_files />
                            </label>
                        }
                    >
                        <div
                            class=move || if drag_idx.get().is_some() { "photo-list reordering" } else { "photo-list" }
                            on:dragover=move |ev: web_sys::DragEvent| {
                                if drag_idx.get().is_some() {
                                    ev.prevent_default();
                                }
                            }
                            on:drop=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                if let Some(from) = drag_idx.get() {
                                    if let Some(target_pos) = drop_indicator.get() {
                                        let to = if from < target_pos {
                                            target_pos - 1
                                        } else {
                                            target_pos
                                        };
                                        if from != to {
                                            photos.update(|items| move_item(items, from, to));
                                        }
                                    }
                                }
                                drag_idx.set(None);
                                drop_indicator.set(None);
                            }
                        >
                            // Top spacer for virtual scroll
                            <div style=move || {
                                let (start, _) = visible_range.get();
                                format!("height: {}px;", start as f64 * ROW_HEIGHT)
                            } />
                            <For
                                each=move || {
                                    let (start, end) = visible_range.get();
                                    photos.with(|items| {
                                        items.iter()
                                            .enumerate()
                                            .skip(start)
                                            .take(end.saturating_sub(start))
                                            .map(|(i, p)| (i, p.clone()))
                                            .collect::<Vec<_>>()
                                    }).into_iter()
                                }
                                key=|(_index, photo)| photo.id
                                children=move |(_initial_index, photo)| {
                                    let photo_id = photo.id;
                                    // Reactive index via memo — O(1) lookup, no Vec clone
                                    let idx = move || {
                                        photo_positions.with(|m| m.get(&photo_id).copied().unwrap_or(0))
                                    };
                                    let is_last = move || {
                                        let len = photos.with(|v| v.len());
                                        len == 0 || idx() + 1 >= len
                                    };

                                    let move_up = {
                                        let photos = photos;
                                        move |_| {
                                            let i = idx();
                                            photos.update(|items| {
                                                if i > 0 {
                                                    move_item(items, i, i - 1);
                                                }
                                            });
                                        }
                                    };

                                    let move_down = {
                                        let photos = photos;
                                        move |_| {
                                            let i = idx();
                                            photos.update(|items| {
                                                if i + 1 < items.len() {
                                                    move_item(items, i, i + 1);
                                                }
                                            });
                                        }
                                    };

                                    let remove = {
                                        let photos = photos;
                                        move |_| {
                                            photos.update(|items| items.retain(|i| i.id != photo_id));
                                        }
                                    };

                                    // Disable drag when an input/textarea is focused
                                    let row_focused = RwSignal::new(false);

                                    view! {
                                        <div
                                            draggable=move || if row_focused.get() { "false" } else { "true" }
                                            class=move || {
                                                let index = idx();
                                                let mut c = String::from("photo-row");
                                                if drag_idx.get() == Some(index) {
                                                    c.push_str(" dragging");
                                                }
                                                if drag_idx.get() != Some(index) {
                                                    if drop_indicator.get() == Some(index) {
                                                        c.push_str(" drag-above");
                                                    }
                                                    let len = photos.with(|v| v.len());
                                                    if index + 1 == len && drop_indicator.get() == Some(len) {
                                                        c.push_str(" drag-below");
                                                    }
                                                }
                                                c
                                            }
                                            on:dragstart=move |_| {
                                                drag_idx.set(Some(idx()));
                                                drop_indicator.set(None);
                                            }
                                            on:dragover=move |ev: web_sys::DragEvent| {
                                                ev.prevent_default();
                                                if let Some(from) = drag_idx.get() {
                                                    let index = idx();
                                                    if index == from {
                                                        drop_indicator.set(None);
                                                    } else if let Some(target) = ev.current_target() {
                                                        let el: web_sys::Element = target.unchecked_into();
                                                        let rect = el.get_bounding_client_rect();
                                                        let mid = rect.top() + rect.height() / 2.0;
                                                        let raw = if (ev.client_y() as f64) <= mid { index } else { index + 1 };
                                                        // Snap out of dead zone: positions `from` and `from+1` both
                                                        // resolve to no-op after the remove/insert adjustment.
                                                        let indicator = if raw == from || raw == from + 1 {
                                                            if index < from { index } else { index + 1 }
                                                        } else {
                                                            raw
                                                        };
                                                        drop_indicator.set(Some(indicator));
                                                    }
                                                }
                                            }
                                            on:drop=move |ev: web_sys::DragEvent| {
                                                ev.prevent_default();
                                                if let Some(from) = drag_idx.get() {
                                                    if let Some(target_pos) = drop_indicator.get() {
                                                        let to = if from < target_pos {
                                                            target_pos - 1
                                                        } else {
                                                            target_pos
                                                        };
                                                        if from != to {
                                                            photos.update(|items| move_item(items, from, to));
                                                        }
                                                    }
                                                }
                                                drag_idx.set(None);
                                                drop_indicator.set(None);
                                            }
                                            on:dragend=move |_| {
                                                drag_idx.set(None);
                                                drop_indicator.set(None);
                                            }
                                        >
                                            <div class="thumb">
                                                <img src=photo.thumb_url.clone() alt=photo.title.clone() loading="lazy" />
                                            </div>
                                            <div class="row-body">
                                                <div class="row-top">
                                                    <span class="row-idx">{move || format!("#{}", idx() + 1)}</span>
                                                    <div class="row-actions">
                                                        <button on:click=move_up disabled=move || idx() == 0 aria-label="Move up">"\u{25B2}"</button>
                                                        <button on:click=move_down disabled=is_last aria-label="Move down">"\u{25BC}"</button>
                                                        <button class="btn-danger" on:click=remove aria-label="Remove">"\u{2715}"</button>
                                                    </div>
                                                </div>
                                                <input
                                                    class="title-input"
                                                    type="text"
                                                    placeholder="Title"
                                                    prop:value=photo.title.clone()
                                                    on:focus=move |_| row_focused.set(true)
                                                    on:blur=move |_| row_focused.set(false)
                                                    on:input=move |ev| {
                                                        let v = event_target::<HtmlInputElement>(&ev).value();
                                                        photos.update(|items| {
                                                            if let Some(item) = items.iter_mut().find(|i| i.id == photo_id) {
                                                                item.title = v;
                                                            }
                                                        });
                                                    }
                                                />
                                                <textarea
                                                    class="desc-input"
                                                    rows="1"
                                                    placeholder="Description"
                                                    prop:value=photo.description.clone()
                                                    on:focus=move |_| row_focused.set(true)
                                                    on:blur=move |_| row_focused.set(false)
                                                    on:input=move |ev| {
                                                        let el = event_target::<HtmlTextAreaElement>(&ev);
                                                        let style = web_sys::HtmlElement::from(el.clone()).style();
                                                        let _ = style.set_property("height", "auto");
                                                        let _ = style.set_property("height", &format!("{}px", el.scroll_height()));
                                                        let v = el.value();
                                                        photos.update(|items| {
                                                            if let Some(item) = items.iter_mut().find(|i| i.id == photo_id) {
                                                                item.description = v;
                                                            }
                                                        });
                                                    }
                                                ></textarea>
                                            </div>
                                        </div>
                                    }
                                }
                            />
                            // Bottom spacer for virtual scroll
                            <div style=move || {
                                let total = photos.with(|v| v.len());
                                let (_, end) = visible_range.get();
                                format!("height: {}px;", total.saturating_sub(end) as f64 * ROW_HEIGHT)
                            } />
                        </div>
                    </Show>
                </div>

                // ── Right pane: preview ──
                <div class="right-pane"
                >
                    <div class="section-bar">
                        <span class="section-label">{move || format!("Preview \u{2014} {}", layout.get().label())}</span>
                        <div class="page-nav">
                            <button class="page-nav-btn"
                                on:click=move |_| {
                                    let p = clamped_page.get();
                                    if p > 0 {
                                        preview_page.set(p - 1);
                                    }
                                }
                                disabled=move || clamped_page.get() == 0
                            >"\u{25C0}"</button>
                            <span class="page-nav-label">
                                {move || format!("Page {} / {}", clamped_page.get() + 1, total_pages.get())}
                            </span>
                            <button class="page-nav-btn"
                                on:click=move |_| {
                                    let p = clamped_page.get();
                                    let max = total_pages.get();
                                    if p + 1 < max {
                                        preview_page.set(p + 1);
                                    }
                                }
                                disabled=move || {
                                    #[allow(clippy::nonminimal_bool)]
                                    { !(clamped_page.get() + 1 < total_pages.get()) }
                                }
                            >"\u{25B6}"</button>
                        </div>
                    </div>
                    {move || {
                        let ly = layout.get();
                        let page_size = ly.page_size();
                        let num_cols = ly.cols();
                        let cols_class = if num_cols == 1 { "cols-1" } else { "cols-2" };
                        let page_idx = clamped_page.get();
                        // Borrow photos — only clone the single page we need
                        let (page, item_count) = photos.with(|items| {
                            let start = page_idx * page_size;
                            let mut pg = Vec::with_capacity(page_size);
                            for s in 0..page_size {
                                if start + s < items.len() {
                                    pg.push(Some(items[start + s].clone()));
                                } else {
                                    pg.push(None);
                                }
                            }
                            (pg, items.len())
                        });

                        let slots = page.into_iter().enumerate().map(|(slot_idx, slot)| {
                            let flat_idx = page_idx * page_size + slot_idx;
                                    match slot {
                                        Some(photo) => {
                                            let title = photo.title.clone();
                                            let desc = photo.description.clone();
                                            let has_desc = !desc.is_empty();
                                            view! {
                                                <div
                                                    draggable="true"
                                                    class=move || {
                                                        let mut c = String::from("slot filled");
                                                        if drag_idx.get() == Some(flat_idx) {
                                                            c.push_str(" dragging");
                                                        }
                                                        if drag_idx.get() != Some(flat_idx) {
                                                            if drop_indicator.get() == Some(flat_idx) {
                                                                c.push_str(" drag-above");
                                                            }
                                                            if flat_idx + 1 == item_count && drop_indicator.get() == Some(item_count) {
                                                                c.push_str(" drag-below");
                                                            }
                                                        }
                                                        c
                                                    }
                                                    on:dragstart=move |_| {
                                                        drag_idx.set(Some(flat_idx));
                                                        drop_indicator.set(None);
                                                    }
                                                    on:dragover=move |ev: web_sys::DragEvent| {
                                                        ev.prevent_default();
                                                        if let Some(from) = drag_idx.get() {
                                                            if flat_idx == from {
                                                                drop_indicator.set(None);
                                                            } else if let Some(target) = ev.current_target() {
                                                                let el: web_sys::Element = target.unchecked_into();
                                                                let rect = el.get_bounding_client_rect();
                                                                // For multi-column grids, detect
                                                                // horizontal position too
                                                                let raw = if num_cols > 1 {
                                                                    let mid_x = rect.left() + rect.width() / 2.0;
                                                                    let mid_y = rect.top() + rect.height() / 2.0;
                                                                    let in_left = (ev.client_x() as f64) <= mid_x;
                                                                    let in_top = (ev.client_y() as f64) <= mid_y;
                                                                    if in_top || in_left { flat_idx } else { flat_idx + 1 }
                                                                } else {
                                                                    let mid = rect.top() + rect.height() / 2.0;
                                                                    if (ev.client_y() as f64) <= mid { flat_idx } else { flat_idx + 1 }
                                                                };
                                                                let indicator = if raw == from || raw == from + 1 {
                                                                    if flat_idx < from { flat_idx } else { flat_idx + 1 }
                                                                } else {
                                                                    raw
                                                                };
                                                                drop_indicator.set(Some(indicator));
                                                            }
                                                        }
                                                    }
                                                    on:drop=move |ev: web_sys::DragEvent| {
                                                        ev.prevent_default();
                                                        if let Some(from) = drag_idx.get() {
                                                            if let Some(target_pos) = drop_indicator.get() {
                                                                let to = if from < target_pos {
                                                                    target_pos - 1
                                                                } else {
                                                                    target_pos
                                                                };
                                                                if from != to {
                                                                    photos.update(|items| move_item(items, from, to));
                                                                }
                                                            }
                                                        }
                                                        drag_idx.set(None);
                                                        drop_indicator.set(None);
                                                    }
                                                    on:dragend=move |_| {
                                                        drag_idx.set(None);
                                                        drop_indicator.set(None);
                                                    }
                                                >
                                                    <img src=photo.preview_url alt=title.clone() loading="lazy" />
                                                    <div class="slot-info">
                                                        <strong class="slot-title">{title}</strong>
                                                        {if has_desc {
                                                            Some(view! { <span class="slot-desc">{desc}</span> })
                                                        } else {
                                                            None
                                                        }}
                                                    </div>
                                                </div>
                                            }.into_any()
                                        }
                                        None => view! {
                                            <div class="slot empty">"Empty"</div>
                                        }.into_any(),
                                    }
                                }).collect_view();

                                view! {
                                    <div class="page-card">
                                        <div class="page-label">{format!("Page {}", page_idx + 1)}</div>
                                        <div class=format!("matrix {}", cols_class)>
                                            {slots}
                                        </div>
                                    </div>
                                }
                    }}
                </div>
            </div>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}
