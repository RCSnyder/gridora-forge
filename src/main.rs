use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures::stream::{self, StreamExt};
use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Blob, CanvasRenderingContext2d, HtmlCanvasElement, HtmlDocument, HtmlInputElement,
    HtmlTextAreaElement, ImageBitmap,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_photo_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// Holds the original source File blobs keyed by photo ID so we can
// defer the expensive export-JPEG compression to PDF-export time.
thread_local! {
    static SOURCE_FILES: RefCell<HashMap<u64, web_sys::File>> = RefCell::new(HashMap::new());

    // Cursor position shared between drag (mouse) and touch handlers,
    // consumed by the auto-scroll interval. Set from `dragover` and `touchmove`.
    static CURSOR_X: Cell<i32> = const { Cell::new(0) };
    static CURSOR_Y: Cell<i32> = const { Cell::new(0) };

    // Touch reorder state.
    // - TOUCH_HOLD: cancel-on-drop handle for the long-press timer.
    // - TOUCH_START: (x, y) of the initial touchstart for movement-threshold checks.
    // - TOUCH_DRAG_ACTIVE: true once the long-press confirms a drag, used by the
    //   global non-passive touchmove listener to call preventDefault().
    static TOUCH_HOLD: RefCell<Option<gloo_timers::callback::Timeout>> =
        const { RefCell::new(None) };
    static TOUCH_START: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };
    static TOUCH_DRAG_ACTIVE: Cell<bool> = const { Cell::new(false) };

    // Source element captured at touchstart so the long-press timer can clone
    // it into a floating ghost without a second elementFromPoint lookup.
    static TOUCH_SOURCE_EL: RefCell<Option<web_sys::HtmlElement>> =
        const { RefCell::new(None) };
    // The floating ghost element appended to <body> while a touch drag is
    // active. Removed in `touch_drag_cleanup`.
    static TOUCH_GHOST: RefCell<Option<web_sys::HtmlElement>> =
        const { RefCell::new(None) };
    // Offset from the finger to the top-left corner of the ghost, captured at
    // long-press confirm so the ghost stays anchored where the user grabbed it.
    static TOUCH_GHOST_OFFSET: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };

    // Sticky last-known-good drop indicator. The live `drop_indicator` signal
    // gets cleared whenever the finger drifts back over the source row (or
    // off any row), which would otherwise lose the user's intent at touchend.
    // We persist the most recent valid insertion target here so the lift
    // gesture always has a fallback. Reset by `touch_drag_cleanup`.
    static TOUCH_LAST_INDICATOR: Cell<Option<usize>> = const { Cell::new(None) };
}

// Long-press hold duration before a touch becomes a drag. Matches the iOS
// system convention closely enough that users won't notice.
const TOUCH_HOLD_MS: u32 = 250;
// If the finger moves more than this many CSS pixels before TOUCH_HOLD_MS
// elapses, we treat the gesture as a scroll instead of a drag.
const TOUCH_MOVE_THRESHOLD_PX: f64 = 8.0;

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
    /// Base64 data-URL of a user-uploaded logo for the cover page.
    logo_data_url: String,
}

#[derive(Clone, PartialEq)]
struct PdfSettings {
    margin_top_in: f64,
    margin_right_in: f64,
    margin_bottom_in: f64,
    margin_left_in: f64,
    header_template: String,
    footer_template: String,
}

impl Default for PdfSettings {
    fn default() -> Self {
        Self {
            margin_top_in: 0.25,
            margin_right_in: 0.25,
            margin_bottom_in: 0.25,
            margin_left_in: 0.25,
            header_template: String::new(),
            footer_template: String::new(),
        }
    }
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

    #[allow(dead_code)]
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
    rotation_quadrants: u8,
    /// Small data-URL thumbnail (~128 px) for the left-pane list.
    thumb_url: String,
    /// Medium data-URL preview (~600 px) for the right-pane page cards.
    preview_url: String,
}

impl PhotoItem {
    fn rotation_degrees(&self) -> u16 {
        (self.rotation_quadrants as u16 % 4) * 90
    }

    fn rotation_style(&self) -> String {
        let degrees = self.rotation_degrees();
        if degrees == 0 {
            String::new()
        } else {
            format!("transform: rotate({degrees}deg);")
        }
    }
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

fn rotate_photo(items: &mut [PhotoItem], photo_id: u64, delta: i8) {
    if let Some(item) = items.iter_mut().find(|photo| photo.id == photo_id) {
        let base = item.rotation_quadrants as i8;
        item.rotation_quadrants = (base + delta).rem_euclid(4) as u8;
    }
}

/// Compute the drop indicator position (insertion index) given a drag source
/// and the row index the cursor is currently over, using vertical-midpoint
/// logic. Mirrors the existing dragover handler in the photo list, extracted
/// so touch and mouse paths share identical math.
///
/// Returns `None` when the cursor is on the source row itself (no movement).
fn compute_drop_indicator(from: usize, target: usize, top: f64, height: f64, y: f64) -> Option<usize> {
    if target == from {
        return None;
    }
    let mid = top + height / 2.0;
    let raw = if y <= mid { target } else { target + 1 };
    // Snap out of the dead zone where the insertion point resolves to the
    // same position as the source after the remove/insert adjustment.
    let resolved = if raw == from || raw == from + 1 {
        if target < from { target } else { target + 1 }
    } else {
        raw
    };
    Some(resolved)
}

/// Walk up from the topmost element under (x, y) until we find one with a
/// `data-photo-idx` attribute. Returns the parsed index and the bounding rect
/// of that element so callers can run midpoint logic.
///
/// Skips the floating touch-ghost subtree defensively in case CSS
/// `pointer-events: none` is overridden somewhere.
///
/// Touch reorder reliability: on iOS Safari, `elementFromPoint` sometimes
/// still returns a child of a fixed-position overlay even with
/// `pointer-events: none`. We hide the ghost for the duration of the hit
/// test, then restore its display, which is the canonical workaround used
/// by every native-feel drag library.
fn target_index_at_point(x: f64, y: f64) -> Option<(usize, web_sys::DomRect)> {
    let doc = web_sys::window()?.document()?;

    // Hide ghost for the hit test, restoring its prior display value.
    let saved_display: Option<(web_sys::HtmlElement, String)> = TOUCH_GHOST.with(|g| {
        g.borrow().as_ref().map(|ghost| {
            let prev = ghost
                .style()
                .get_property_value("display")
                .unwrap_or_default();
            let _ = ghost.style().set_property("display", "none");
            (ghost.clone(), prev)
        })
    });

    let result = (|| -> Option<(usize, web_sys::DomRect)> {
        let mut current = doc.element_from_point(x as f32, y as f32);
        while let Some(el) = current {
            if el.closest(".touch-ghost").ok().flatten().is_some() {
                current = el.parent_element();
                continue;
            }
            if let Some(idx_str) = el.get_attribute("data-photo-idx") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    let rect = el.get_bounding_client_rect();
                    return Some((idx, rect));
                }
            }
            current = el.parent_element();
        }
        None
    })();

    if let Some((ghost, prev)) = saved_display {
        let _ = ghost.style().set_property("display", &prev);
    }

    // Fallback: if elementFromPoint missed (or was over a gap between rows),
    // scan every [data-photo-idx] element and pick the one whose vertical
    // band contains y. This guarantees we still resolve a target so the
    // drop indicator shows up on every meaningful move.
    if result.is_some() {
        return result;
    }
    let nodes = doc.query_selector_all("[data-photo-idx]").ok()?;
    let mut best: Option<(usize, web_sys::DomRect, f64)> = None;
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else { continue };
        let Ok(el) = node.dyn_into::<web_sys::Element>() else { continue };
        if el.closest(".touch-ghost").ok().flatten().is_some() {
            continue;
        }
        let Some(idx_str) = el.get_attribute("data-photo-idx") else { continue };
        let Ok(idx) = idx_str.parse::<usize>() else { continue };
        let rect = el.get_bounding_client_rect();
        // Only consider rows roughly horizontally aligned with the finger
        // so multi-column preview grids still work.
        if x < rect.left() - 4.0 || x > rect.right() + 4.0 {
            continue;
        }
        let cy = rect.top() + rect.height() / 2.0;
        let dist = (y - cy).abs();
        if best.as_ref().is_none_or(|b| dist < b.2) {
            best = Some((idx, rect, dist));
        }
    }
    best.map(|(idx, rect, _)| (idx, rect))
}

/// Resolve a final drop indicator from a cursor position, using the same
/// midpoint math as `compute_drop_indicator`. Used by touchend to recover
/// when the last touchmove left `drop_indicator` as `None` (e.g., the finger
/// was held still over the source row, or briefly outside any photo row).
fn resolve_drop_indicator_at_point(from: usize, x: f64, y: f64) -> Option<usize> {
    let (target, rect) = target_index_at_point(x, y)?;
    compute_drop_indicator(from, target, rect.top(), rect.height(), y)
}

/// True if the pointerdown target is an interactive element where pointer
/// input should pass through to the browser (text editing, button taps).
fn pointer_target_is_interactive(ev: &web_sys::PointerEvent) -> bool {
    let Some(target) = ev.target() else {
        return false;
    };
    let Ok(el) = target.dyn_into::<web_sys::Element>() else {
        return false;
    };
    // Walk up a few levels because the actual hit may be a child span/icon
    // inside a <button>; we still want to treat the outer interactive
    // ancestor as a pass-through.
    let mut current: Option<web_sys::Element> = Some(el);
    let mut depth = 0;
    while let Some(node) = current {
        let tag = node.tag_name();
        if matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "BUTTON" | "SELECT" | "A") {
            return true;
        }
        if depth >= 3 {
            break;
        }
        depth += 1;
        current = node.parent_element();
    }
    false
}

fn body_class_add(name: &str) {
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        let _ = body.class_list().add_1(name);
    }
}

fn body_class_remove(name: &str) {
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        let _ = body.class_list().remove_1(name);
    }
}

/// End/cancel any pending touch-drag state. Idempotent.
fn touch_drag_cleanup(
    drag_idx: RwSignal<Option<usize>>,
    drop_indicator: RwSignal<Option<usize>>,
) {
    TOUCH_HOLD.with(|h| h.borrow_mut().take()); // drops the timer = cancels it
    TOUCH_DRAG_ACTIVE.with(|a| a.set(false));
    drag_idx.set(None);
    drop_indicator.set(None);
    body_class_remove("touch-dragging");
    touch_ghost_detach();
    TOUCH_SOURCE_EL.with(|s| { s.borrow_mut().take(); });
    TOUCH_LAST_INDICATOR.with(|l| l.set(None));
}

/// Clone the source row element into a fixed-position ghost overlay anchored
/// to the user's finger. Called once at long-press confirm.
fn touch_ghost_attach(x: f64, y: f64) {
    let Some(window) = web_sys::window() else { return; };
    let Some(document) = window.document() else { return; };
    let Some(body) = document.body() else { return; };
    let Some(source_el) = TOUCH_SOURCE_EL.with(|s| s.borrow().clone()) else { return; };

    let rect = source_el.get_bounding_client_rect();
    let offset_x = x - rect.left();
    let offset_y = y - rect.top();
    TOUCH_GHOST_OFFSET.with(|o| o.set((offset_x, offset_y)));

    let Ok(node) = source_el.clone_node_with_deep(true) else { return; };
    let Ok(ghost) = node.dyn_into::<web_sys::HtmlElement>() else { return; };

    // Strip interactive children's IDs/names to keep the original DOM tree
    // unique. Cheaply done via removeAttribute on the ghost root.
    let _ = ghost.remove_attribute("id");
    let _ = ghost.remove_attribute("data-photo-idx");
    let _ = ghost.class_list().add_1("touch-ghost");

    let style = ghost.style();
    let _ = style.set_property("position", "fixed");
    let _ = style.set_property("left", "0");
    let _ = style.set_property("top", "0");
    let _ = style.set_property("width", &format!("{}px", rect.width()));
    let _ = style.set_property("height", &format!("{}px", rect.height()));
    let _ = style.set_property("pointer-events", "none");
    let _ = style.set_property("z-index", "9999");
    let _ = style.set_property("opacity", "0.92");
    let _ = style.set_property("box-shadow", "0 12px 32px rgba(0,0,0,0.35)");
    let _ = style.set_property("transform-origin", "0 0");
    let _ = style.set_property(
        "transform",
        &format!(
            "translate({}px, {}px) scale(1.04)",
            rect.left(),
            rect.top()
        ),
    );
    let _ = style.set_property("transition", "transform 90ms ease-out");

    if body.append_child(ghost.as_ref()).is_ok() {
        TOUCH_GHOST.with(|g| *g.borrow_mut() = Some(ghost));
        // Snap to finger position on next frame so the lift animation reads.
        touch_ghost_update(x, y);
    }
}

/// Update the ghost's transform to follow the finger.
fn touch_ghost_update(x: f64, y: f64) {
    TOUCH_GHOST.with(|g| {
        if let Some(ghost) = g.borrow().as_ref() {
            let (off_x, off_y) = TOUCH_GHOST_OFFSET.with(|o| o.get());
            let _ = ghost.style().set_property(
                "transform",
                &format!(
                    "translate({}px, {}px) scale(1.04)",
                    x - off_x,
                    y - off_y
                ),
            );
        }
    });
}

/// Remove the ghost from the DOM if present.
fn touch_ghost_detach() {
    TOUCH_GHOST.with(|g| {
        if let Some(ghost) = g.borrow_mut().take() {
            if let Some(parent) = ghost.parent_node() {
                let _ = parent.remove_child(ghost.as_ref());
            }
        }
    });
}

fn update_photo_title(items: &mut [PhotoItem], photo_id: u64, title: String) {
    if let Some(item) = items.iter_mut().find(|photo| photo.id == photo_id) {
        item.title = title;
    }
}

fn update_photo_description(items: &mut [PhotoItem], photo_id: u64, description: String) {
    if let Some(item) = items.iter_mut().find(|photo| photo.id == photo_id) {
        item.description = description;
    }
}

fn update_pdf_margin(settings: &mut PdfSettings, field: &str, value: f64) {
    let clamped = value.clamp(0.0, 2.0);
    match field {
        "top" => settings.margin_top_in = clamped,
        "right" => settings.margin_right_in = clamped,
        "bottom" => settings.margin_bottom_in = clamped,
        "left" => settings.margin_left_in = clamped,
        _ => {}
    }
}

fn apply_pdf_template(
    template: &str,
    meta: &ReportMeta,
    page: usize,
    total_pages: usize,
) -> String {
    let mut rendered = template.to_string();
    let page_str = page.to_string();
    let total_pages_str = total_pages.to_string();
    let replacements = [
        ("{title}", meta.title.as_str()),
        ("{site_address}", meta.site_address.as_str()),
        ("{author}", meta.author.as_str()),
        ("{date}", meta.date.as_str()),
        ("{notes}", meta.notes.as_str()),
        ("{page}", page_str.as_str()),
        ("{total_pages}", total_pages_str.as_str()),
    ];

    for (token, value) in replacements {
        rendered = rendered.replace(token, value);
    }

    html_escape(&rendered).replace('\n', "<br/>")
}

// ---------------------------------------------------------------------------
// Browser helpers
// ---------------------------------------------------------------------------

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
/// Tuned for PDF export quality without ballooning browser-generated PDFs.
const MAX_PARALLEL_IMAGE_TASKS: usize = 6;
const EXPORT_SIZE_STEPS: &[(u32, f64)] = &[(1600, 0.80), (1400, 0.74), (1200, 0.68), (1000, 0.62)];
const EXPORT_TARGET_MAX_BYTES: usize = 400_000;

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

fn decode_data_url_bytes(data_url: &str) -> Result<Vec<u8>, String> {
    let (_, b64) = data_url
        .split_once(',')
        .ok_or_else(|| "data URL missing payload".to_string())?;
    BASE64
        .decode(b64)
        .map_err(|_| "data URL base64 decode failed".to_string())
}

fn build_export_jpeg(bitmap: &ImageBitmap) -> Result<Vec<u8>, String> {
    let mut best: Option<Vec<u8>> = None;

    for (max_dim, quality) in EXPORT_SIZE_STEPS {
        let data_url = draw_scaled_data_url(bitmap, *max_dim, *quality)?;
        let bytes = decode_data_url_bytes(&data_url)?;
        if bytes.len() <= EXPORT_TARGET_MAX_BYTES {
            return Ok(bytes);
        }

        let should_replace = best
            .as_ref()
            .map(|current| bytes.len() < current.len())
            .unwrap_or(true);
        if should_replace {
            best = Some(bytes);
        }
    }

    best.ok_or_else(|| "failed to compress export image".to_string())
}

async fn build_photo_item(file: web_sys::File) -> Result<PhotoItem, String> {
    let source_file = if is_heic(&file) {
        convert_heic_to_jpeg(&file).await?
    } else {
        file
    };

    let filename = source_file.name();
    let bitmap = create_bitmap(&source_file).await?;
    let thumb_url = draw_scaled_data_url(&bitmap, 128, 0.7)?;
    let preview_url = draw_scaled_data_url(&bitmap, 600, 0.8)?;

    let id = next_photo_id();
    // Store the source file for deferred export-JPEG generation at PDF time.
    SOURCE_FILES.with(|m| m.borrow_mut().insert(id, source_file));

    Ok(PhotoItem {
        id,
        title: title_from_filename(&filename),
        description: String::new(),
        filename,
        mime: "image/jpeg".to_string(),
        rotation_quadrants: 0,
        thumb_url,
        preview_url,
    })
}

// ---------------------------------------------------------------------------
// PDF / print HTML export
// ---------------------------------------------------------------------------

fn build_print_html(
    photos: &[PhotoItem],
    export_bytes: &HashMap<u64, Vec<u8>>,
    layout: GridLayout,
    meta: &ReportMeta,
    settings: &PdfSettings,
    show_page_numbers: bool,
    include_cover: bool,
) -> String {
    let mut body_html = String::new();
    let photo_pages = split_pages(photos, layout);

    // -- cover / metadata block -----------------------------------------
    let has_meta = include_cover
        && (!meta.title.is_empty()
            || !meta.site_address.is_empty()
            || !meta.author.is_empty()
            || !meta.date.is_empty()
            || !meta.notes.is_empty()
            || !meta.logo_data_url.is_empty());
    let total_pages = photo_pages.len() + usize::from(has_meta);

    if has_meta {
        let cover_page_number = 1;
        let header_html = apply_pdf_template(
            &settings.header_template,
            meta,
            cover_page_number,
            total_pages,
        );
        let footer_html = apply_pdf_template(
            &settings.footer_template,
            meta,
            cover_page_number,
            total_pages,
        );
        body_html.push_str(r#"<section class="page cover-page">"#);
        if !header_html.trim().is_empty() {
            body_html.push_str(&format!(r#"<div class="page-header">{header_html}</div>"#));
        }
        body_html.push_str(r#"<div class="page-main cover-main">"#);
        if !meta.logo_data_url.is_empty() {
            body_html.push_str(&format!(
                r#"<div class="cover-logo"><img src="{}" alt="Logo" /></div>"#,
                meta.logo_data_url
            ));
        }
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
        body_html.push_str("</div>");
        if !footer_html.trim().is_empty() {
            body_html.push_str(&format!(r#"<div class="page-footer">{footer_html}</div>"#));
        }
        body_html.push_str("</section>");
    }

    // -- photo pages ----------------------------------------------------
    match layout {
        GridLayout::OneUp | GridLayout::TwoUp => {
            for (page_idx, page) in photo_pages.iter().enumerate() {
                let absolute_page = page_idx + 1 + usize::from(has_meta);
                let header_html =
                    apply_pdf_template(&settings.header_template, meta, absolute_page, total_pages);
                let footer_html =
                    apply_pdf_template(&settings.footer_template, meta, absolute_page, total_pages);
                body_html.push_str(r#"<section class="page">"#);
                if !header_html.trim().is_empty() {
                    body_html.push_str(&format!(r#"<div class="page-header">{header_html}</div>"#));
                }
                if show_page_numbers {
                    body_html.push_str(&format!(
                        r#"<div class="page-label">Page {}</div>"#,
                        absolute_page
                    ));
                }
                body_html.push_str(r#"<div class="page-main">"#);
                for photo in page.iter().flatten() {
                    let encoded = export_bytes.get(&photo.id).map(|b| BASE64.encode(b)).unwrap_or_default();
                    let rotation_style = photo.rotation_style();
                    body_html.push_str(&format!(
                        r#"<div class="photo-block"><div class="photo-media"><img src="data:{};base64,{}" alt="{}" style="{}" /></div><div class="caption"><strong>{}</strong>"#,
                        photo.mime,
                        encoded,
                        html_escape(&photo.title),
                        rotation_style,
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
                body_html.push_str("</div>"); // close .page-main
                if !footer_html.trim().is_empty() {
                    body_html.push_str(&format!(r#"<div class="page-footer">{footer_html}</div>"#));
                }
                body_html.push_str("</section>");
            }
        }
        GridLayout::TwoByTwo | GridLayout::TwoByThree => {
            for (page_idx, page) in photo_pages.iter().enumerate() {
                let absolute_page = page_idx + 1 + usize::from(has_meta);
                let header_html =
                    apply_pdf_template(&settings.header_template, meta, absolute_page, total_pages);
                let footer_html =
                    apply_pdf_template(&settings.footer_template, meta, absolute_page, total_pages);
                body_html.push_str(r#"<section class="page">"#);
                if !header_html.trim().is_empty() {
                    body_html.push_str(&format!(r#"<div class="page-header">{header_html}</div>"#));
                }
                if show_page_numbers {
                    body_html.push_str(&format!(
                        r#"<div class="page-label">Page {}</div>"#,
                        absolute_page,
                    ));
                }
                body_html.push_str(&format!(
                    r#"<div class="page-main"><div class="grid rows-{}">"#,
                    layout.rows()
                ));
                for slot in page {
                    match slot {
                        Some(photo) => {
                            let encoded = export_bytes.get(&photo.id).map(|b| BASE64.encode(b)).unwrap_or_default();
                            let rotation_style = photo.rotation_style();
                            body_html.push_str(&format!(
                                r#"<figure class="cell"><div class="cell-media"><img src="data:{};base64,{}" alt="{}" style="{}" /></div><figcaption><strong>{}</strong>"#,
                                photo.mime,
                                encoded,
                                html_escape(&photo.title),
                                rotation_style,
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
                body_html.push_str("</div></div>");
                if !footer_html.trim().is_empty() {
                    body_html.push_str(&format!(r#"<div class="page-footer">{footer_html}</div>"#));
                }
                body_html.push_str("</section>");
            }
        }
    }

    // Build a suggested filename for the PDF <title> tag.
    // Skip the (Date::now-dependent) export_filename call when we have a
    // meaningful title, which keeps `build_print_html` callable from host
    // unit tests that have no JS context.
    let title_esc = if meta.title.is_empty() {
        let pdf_filename = export_filename(meta, "pdf");
        html_escape(&pdf_filename)
    } else {
        html_escape(&meta.title)
    };

    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<title>{title} — Gridora Forge Report</title>
<style>
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
@page {{ size: letter; margin: 0; }}
html, body {{ height: 100%; margin: 0; font-family: Arial, sans-serif; color: #111; background: #fff; }}

/* Each .page is exactly one printed page (US Letter, 8.5in x 11in).
   Fixed physical units beat viewport-relative ones: mobile browsers report
   viewport height that fluctuates with the address bar, which corrupts
   page-break math.
   Margins are baked in as padding so the browser print dialog cannot override them. */
.page {{
  width: 8.5in;
  height: 11in;
  padding: {margin_top}in {margin_right}in {margin_bottom}in {margin_left}in;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  page-break-after: always;
  break-after: page;
}}
.page:last-child {{ page-break-after: auto; break-after: auto; }}

/* Optional header / footer chrome */
.page-header,
.page-footer {{
  flex: 0 0 auto;
  font-size: 10px;
  color: #666;
  line-height: 1.35;
}}
.page-header {{ padding-bottom: 0.12in; }}
.page-footer {{ padding-top: 0.12in; }}

/* Main content area fills remaining page height */
.page-main {{
  flex: 1 1 0;
  display: flex;
  flex-direction: column;
  gap: 0.12in;
  min-height: 0;
  overflow: hidden;
}}

/* ---------- Cover / title page ---------- */
.cover-main {{
  justify-content: center;
  align-items: center;
  text-align: center;
}}
.cover-logo {{ margin-bottom: 0.25in; text-align: center; }}
.cover-logo img {{ max-width: 3in; max-height: 1.5in; object-fit: contain; }}
.cover-title {{ font-size: 28px; margin-bottom: 0.3in; }}
.cover-detail {{ font-size: 14px; margin: 4px 0; }}
.cover-notes {{ margin-top: 0.3in; font-size: 13px; }}
.cover-notes p {{ margin: 6px 0 0 0; }}

/* ---------- Page number label ---------- */
.page-label {{ flex: 0 0 auto; color: #555; font-size: 11px; }}

/* ---------- Grid layouts (2×2, 2×3) ---------- */
.grid {{
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 0.15in;
  flex: 1 1 0;
  min-height: 0;
}}
.grid.rows-2 {{ grid-template-rows: repeat(2, 1fr); }}
.grid.rows-3 {{ grid-template-rows: repeat(3, 1fr); }}
.cell {{
  border: 1px solid #ccc;
  border-radius: 4px;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  min-height: 0;
}}
figure {{ margin: 0; display: flex; flex-direction: column; min-height: 0; flex: 1 1 0; }}
.cell-media {{
  flex: 1 1 0;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 6px;
  background: #f5f5f5;
}}
.cell-media img {{
  max-width: 100%;
  max-height: 100%;
  display: block;
  object-fit: contain;
  transform-origin: center center;
}}
.cell figcaption {{
  flex: 0 0 auto;
  padding: 3px 6px;
  font-size: 10px;
  color: #333;
  line-height: 1.3;
  word-break: break-all;
  overflow-wrap: anywhere;
}}
.cell figcaption .desc {{ font-weight: normal; color: #555; }}
.empty-cell {{
  align-items: center;
  justify-content: center;
  background: #fafafa;
  color: #999;
}}

/* ---------- Stacked layouts (1-up, 2-up) ---------- */
.photo-block {{
  border: 1px solid #ddd;
  border-radius: 4px;
  overflow: hidden;
  flex: 1 1 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
}}
.photo-media {{
  flex: 1 1 0;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 8px;
  background: #f5f5f5;
}}
.photo-media img {{
  max-width: 100%;
  max-height: 100%;
  display: block;
  object-fit: contain;
  transform-origin: center center;
}}
.caption {{
  flex: 0 0 auto;
  padding: 6px 8px;
  font-size: 12px;
  line-height: 1.4;
  word-break: break-all;
  overflow-wrap: anywhere;
}}
.caption p {{ margin: 4px 0 0 0; color: #444; }}

@media print {{
  html, body {{ height: 100%; }}
  body {{ print-color-adjust: exact; -webkit-print-color-adjust: exact; }}
  .page {{ page-break-inside: avoid; }}
}}
</style>
</head>
<body>
{body}
<script>
window.onload = function() {{ window.print(); }};
</script>
</body>
</html>"#,
        title = title_esc,
    margin_top = settings.margin_top_in,
    margin_right = settings.margin_right_in,
    margin_bottom = settings.margin_bottom_in,
    margin_left = settings.margin_left_in,
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
    // Get today's date in YYYY-MM-DD for the default
    let today = {
        let d = js_sys::Date::new_0();
        let y = d.get_full_year();
        let m = d.get_month() + 1; // 0-indexed
        let day = d.get_date();
        format!("{y:04}-{m:02}-{day:02}")
    };
    let meta = RwSignal::new(ReportMeta {
        title: "Report Title".to_string(),
        site_address: "123 Site Address, 12345 ST".to_string(),
        author: "The Author".to_string(),
        date: today,
        notes: "Photos and descriptions of 123 Site Address, 12345 ST".to_string(),
        logo_data_url: String::new(),
    });
    let pdf_settings = RwSignal::new(PdfSettings::default());
    let include_page_numbers = RwSignal::new(false);
    let enable_cover_page = RwSignal::new(false);
    let status = RwSignal::new(String::new());
    let drag_idx = RwSignal::new(Option::<usize>::None);
    let drop_indicator = RwSignal::new(Option::<usize>::None);
    let photo_positions: Memo<HashMap<u64, usize>> = Memo::new(move |_| {
        photos.with(|items| items.iter().enumerate().map(|(i, p)| (p.id, i)).collect())
    });
    let clear_pending = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let drop_hover = RwSignal::new(false);
    let show_support_modal = RwSignal::new(false);
    let progress = RwSignal::new((0usize, 0usize)); // (current, total)
    let preview_page = RwSignal::new(0usize);
    let has_cover_page: Memo<bool> = Memo::new(move |_| {
        if !enable_cover_page.get() {
            return false;
        }
        let m = meta.get();
        !m.title.is_empty() || !m.site_address.is_empty() || !m.author.is_empty()
            || !m.date.is_empty() || !m.notes.is_empty() || !m.logo_data_url.is_empty()
    });
    let total_pages: Memo<usize> = Memo::new(move |_| {
        let count = photos.with(|v| v.len());
        let ps = layout.get().page_size();
        let cover = usize::from(has_cover_page.get());
        if count == 0 {
            1 + cover
        } else {
            count.div_ceil(ps) + cover
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
        status.set(format!(
            "Processing {} image(s) with {} parallel worker(s)…",
            raw_files.len(),
            raw_files.len().min(MAX_PARALLEL_IMAGE_TASKS)
        ));

        spawn_local(async move {
            let total = raw_files.len();
            let completed = Rc::new(Cell::new(0usize));
            let mut loaded = Vec::<(usize, PhotoItem)>::new();
            let mut skipped = 0usize;
            let mut work = stream::iter(raw_files.into_iter().enumerate().map(|(idx, file)| {
                let completed = completed.clone();
                async move {
                    let file_name = file.name();
                    let result = build_photo_item(file).await;
                    let done = completed.get() + 1;
                    completed.set(done);
                    (idx, file_name, done, result)
                }
            }))
            .buffer_unordered(MAX_PARALLEL_IMAGE_TASKS);

            while let Some((idx, file_name, done, result)) = work.next().await {
                progress.set((done, total));
                match result {
                    Ok(photo) => {
                        loaded.push((idx, photo));
                        status.set(format!("Processed {done}/{total}: {file_name}"));
                    }
                    Err(err) => {
                        skipped += 1;
                        web_sys::console::warn_1(
                            &format!("Image processing failed for {}: {}", file_name, err).into(),
                        );
                        status.set(format!("Skipped {done}/{total}: {file_name}"));
                    }
                }
            }

            loaded.sort_by_key(|(idx, _)| *idx);
            let loaded_count = loaded.len();
            photos.update(|items| items.extend(loaded.into_iter().map(|(_, photo)| photo)));
            if skipped == 0 {
                status.set(format!("Loaded {} image(s).", loaded_count));
            } else {
                status.set(format!(
                    "Loaded {} image(s); skipped {} file(s).",
                    loaded_count, skipped
                ));
            }
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

        // If an internal reorder drag is still active when the event bubbles
        // up here (e.g. dropped on an empty grid slot that has no drop
        // handler), bail out. Otherwise Chromium auto-attaches the dragged
        // <img> data to `dataTransfer.files`, and we'd "import" it as a new
        // photo, duplicating the existing one.
        if drag_idx.get().is_some() {
            drag_idx.set(None);
            drop_indicator.set(None);
            return;
        }

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
        let ly = layout.get();
        let settings = pdf_settings.get();
        let show_page_numbers = include_page_numbers.get();
        let include_cover = enable_cover_page.get();

        // The popup must be opened synchronously (same user-gesture tick)
        // to avoid browser popup-blockers.
        let Some(window) = web_sys::window() else {
            status.set("Could not access the browser window.".to_string());
            return;
        };
        let print_window = match window.open_with_url_and_target("about:blank", "_blank") {
            Ok(Some(w)) => w,
            _ => {
                status.set("Popup was blocked. Allow popups for this app and try again.".to_string());
                return;
            }
        };

        status.set("Preparing PDF\u{2026} compressing images".to_string());
        loading.set(true);

        spawn_local(async move {
            // Build export JPEG bytes for each photo from the stored source files.
            let mut export_bytes = HashMap::new();
            for photo in &current {
                let file_opt = SOURCE_FILES.with(|m| m.borrow().get(&photo.id).cloned());
                if let Some(file) = file_opt {
                    match create_bitmap(&file).await {
                        Ok(bitmap) => match build_export_jpeg(&bitmap) {
                            Ok(bytes) => { export_bytes.insert(photo.id, bytes); }
                            Err(e) => {
                                web_sys::console::warn_1(&format!("Export JPEG failed for {}: {}", photo.filename, e).into());
                            }
                        },
                        Err(e) => {
                            web_sys::console::warn_1(&format!("Bitmap decode failed for {}: {}", photo.filename, e).into());
                        }
                    }
                }
            }

            let html = build_print_html(&current, &export_bytes, ly, &m, &settings, show_page_numbers, include_cover);

            if let Some(doc) = print_window.document() {
                let html_doc: HtmlDocument = doc.unchecked_into();
                let _ = html_doc.open();
                let _ = html_doc.write(&js_sys::Array::of1(&html.into()));
                let _ = html_doc.close();
                status.set(
                    "Print dialog opened. Choose \u{201c}Save as PDF\u{201d} in the browser print dialog."
                        .to_string(),
                );
            } else {
                let _ = print_window.close();
                status.set("Could not open print document.".to_string());
            }
            loading.set(false);

            // Show support modal after export completes (unless permanently dismissed)
            let dominated = web_sys::window()
                .unwrap()
                .local_storage()
                .ok()
                .flatten()
                .and_then(|s| s.get_item("gridora_support_dismissed").ok().flatten())
                .unwrap_or_default()
                == "1";
            if !dominated {
                // Small delay so it appears after the print dialog opens
                gloo_timers::future::TimeoutFuture::new(600).await;
                show_support_modal.set(true);
            }
        });
    };

    // --- input refs -------------------------------------------------------
    let file_input_ref = NodeRef::<leptos::html::Input>::new();
    let folder_ref = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = folder_ref.get() {
            let _ = el.set_attribute("webkitdirectory", "");
        }
    });

    // --- keyboard shortcuts (one-time setup) ---------------------------
    // Ctrl+O = add files  (Ctrl+P intentionally NOT bound — conflicts with browser print)
    {
        use wasm_bindgen::closure::Closure;
        let window = web_sys::window().unwrap();
        let handler =
            Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
                let ctrl = ev.ctrl_key() || ev.meta_key();
                if !ctrl {
                    return;
                }
                if ev.key() == "o" || ev.key() == "O" {
                    ev.prevent_default();
                    if let Some(el) = file_input_ref.get() {
                        el.click();
                    }
                }
            });
        window
            .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
            .ok();
        handler.forget(); // Intentional: lives for the lifetime of the SPA

        // Global dragover: prevent no-drop cursor, track cursor position for auto-scroll
        let drag_handler =
            Closure::<dyn Fn(web_sys::DragEvent)>::new(move |ev: web_sys::DragEvent| {
                if drag_idx.get().is_some() {
                    ev.prevent_default();
                    CURSOR_X.with(|c| c.set(ev.client_x()));
                    CURSOR_Y.with(|c| c.set(ev.client_y()));
                    if let Some(dt) = ev.data_transfer() {
                        dt.set_drop_effect("move");
                    }
                }
            });
        window
            .add_event_listener_with_callback("dragover", drag_handler.as_ref().unchecked_ref())
            .ok();
        drag_handler.forget();

        // Pointer Events API handles touch/mouse/pen via a unified path. The
        // `touch-action` CSS on draggable rows tells the browser whether to
        // claim the gesture for scrolling, so no manual non-passive listener
        // is required to suppress page scroll during a confirmed drag.

        // Interval-based auto-scroll during drag (16ms ≈ 60fps)
        // Browsers suppress wheel events during native drag, so we scroll
        // based on cursor proximity to pane edges. The same interval also
        // covers touch reorder because touchmove updates CURSOR_X/Y.
        //
        // Mobile note: on narrow viewports the panes use `overflow-y: visible`
        // and the body is the scroll container. If we can't find a scrollable
        // pane ancestor, fall back to window scroll keyed off viewport edges.
        let scroll_cb = Closure::<dyn Fn()>::new(move || {
            // Active during either a mouse drag or a touch drag
            let touch_active = TOUCH_DRAG_ACTIVE.with(|a| a.get());
            if drag_idx.get().is_none() && !touch_active {
                return;
            }
            let x = CURSOR_X.with(|c| c.get()) as f64;
            let y = CURSOR_Y.with(|c| c.get()) as f64;
            let window = web_sys::window().unwrap();
            let doc = window.document().unwrap();
            // Try pane-scroll first (desktop / tablet wide layout).
            if let Some(el) = doc.element_from_point(x as f32, y as f32) {
                let mut current: Option<web_sys::Element> = Some(el);
                while let Some(node) = current {
                    let cls = node.class_name();
                    if cls.contains("left-pane") || cls.contains("right-pane") {
                        // Only treat the pane as the scroller if it's actually
                        // scrollable in this layout; otherwise fall through to
                        // window scroll below.
                        let html_ref: &web_sys::HtmlElement = node.unchecked_ref();
                        if html_ref.scroll_height() > html_ref.client_height() {
                            let rect = node.get_bounding_client_rect();
                            let edge = 80.0;
                            let dist_top = y - rect.top();
                            let dist_bottom = rect.bottom() - y;
                            let speed = if dist_top < edge {
                                -((edge - dist_top) / edge * 20.0)
                            } else if dist_bottom < edge {
                                (edge - dist_bottom) / edge * 20.0
                            } else {
                                0.0
                            };
                            if speed.abs() > 0.5 {
                                html_ref.set_scroll_top(html_ref.scroll_top() + speed as i32);
                            }
                            return;
                        }
                        break;
                    }
                    current = node.parent_element();
                }
            }
            // Fallback: window-level auto-scroll keyed off viewport edges.
            let viewport_h = window
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if viewport_h <= 0.0 {
                return;
            }
            let edge = 80.0;
            let dist_top = y;
            let dist_bottom = viewport_h - y;
            let speed = if dist_top < edge {
                -((edge - dist_top) / edge * 20.0)
            } else if dist_bottom < edge {
                (edge - dist_bottom) / edge * 20.0
            } else {
                0.0
            };
            if speed.abs() > 0.5 {
                window.scroll_by_with_x_and_y(0.0, speed);
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
                <span class="version-badge">"v1.0.1"</span>
                <span class="toolbar-hint">"Drag. Drop. Label. Export to PDF."</span>
                <div class="toolbar-group toolbar-left">
                    <label class="btn btn-primary">
                        "\u{1F4F7} Photos"
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
                    <button class="btn-export" on:click=export_pdf disabled=move || loading.get()>"\u{1F5A8} Export to PDF"</button>
                    <span class="toolbar-sep">"|"</span>
                    <button
                        class=move || if clear_pending.get() { "btn-danger btn-confirm" } else { "btn-danger" }
                        on:click=move |_| {
                            if clear_pending.get() {
                                SOURCE_FILES.with(|m| m.borrow_mut().clear());
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
                <div class="toolbar-right">
                    <a class="btn btn-secondary toolbar-link" href="https://github.com/RCSnyder/gridora-forge" target="_blank" rel="noreferrer">"\u{1F4BB} Source"</a>
                    <a class="btn btn-secondary toolbar-link" href="https://github.com/RCSnyder/gridora-forge/issues/new" target="_blank" rel="noreferrer">"\u{1F41B} Report a Problem / Request a Feature"</a>
                    <a class="btn btn-secondary toolbar-link" href="https://buymeacoffee.com/rcoopersnyder" target="_blank" rel="noreferrer">"\u{2615} Support"</a>
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
                } else if s.is_empty() {
                    "status-bar status-hidden"
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
                        <summary>"Title Page & Report Info"</summary>
                        <div class="meta-grid">
                            <label class="checkbox-field">
                                <input
                                    type="checkbox"
                                    prop:checked=move || enable_cover_page.get()
                                    on:change=move |ev| {
                                        let checked = event_target::<HtmlInputElement>(&ev).checked();
                                        enable_cover_page.set(checked);
                                    }
                                />
                                <span>"Include title page"</span>
                            </label>
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
                            <div class="meta-field full-width logo-field">
                                <span>"Logo"</span>
                                <div class="logo-row">
                                    {move || {
                                        let url = meta.get().logo_data_url.clone();
                                        if url.is_empty() {
                                            None
                                        } else {
                                            Some(view! { <img class="logo-preview" src=url alt="Logo" /> })
                                        }
                                    }}
                                    <label class="btn btn-secondary logo-btn">
                                        {move || if meta.get().logo_data_url.is_empty() { "Upload Logo" } else { "Replace" }}
                                        <input
                                            type="file"
                                            accept="image/*"
                                            on:change=move |ev| {
                                                let input = event_target::<HtmlInputElement>(&ev);
                                                if let Some(files) = input.files() {
                                                    if let Some(file) = files.get(0) {
                                                        let meta = meta;
                                                        spawn_local(async move {
                                                            if let Ok(bitmap) = create_bitmap(&file).await {
                                                                if let Ok(url) = draw_scaled_data_url(&bitmap, 400, 0.85) {
                                                                    meta.update(|m| m.logo_data_url = url);
                                                                }
                                                            }
                                                        });
                                                    }
                                                }
                                                // Reset so re-selecting same file fires change
                                                input.set_value("");
                                            }
                                        />
                                    </label>
                                    <Show when=move || !meta.get().logo_data_url.is_empty()>
                                        <button class="btn-danger btn-sm" on:click=move |_| {
                                            meta.update(|m| m.logo_data_url.clear());
                                        }>"Remove"</button>
                                    </Show>
                                </div>
                            </div>
                        </div>
                    </details>

                    <details class="meta-panel">
                        <summary>"PDF Export Settings"</summary>
                        <div class="meta-grid pdf-grid">
                            <label class="meta-field">
                                <span>"Top Margin (in)"</span>
                                <input
                                    type="number"
                                    min="0"
                                    max="2"
                                    step="0.05"
                                    prop:value=move || format!("{:.2}", pdf_settings.get().margin_top_in)
                                    on:input=move |ev| {
                                        let value = event_target::<HtmlInputElement>(&ev)
                                            .value()
                                            .parse::<f64>()
                                            .unwrap_or_default();
                                        pdf_settings.update(|settings| update_pdf_margin(settings, "top", value));
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Right Margin (in)"</span>
                                <input
                                    type="number"
                                    min="0"
                                    max="2"
                                    step="0.05"
                                    prop:value=move || format!("{:.2}", pdf_settings.get().margin_right_in)
                                    on:input=move |ev| {
                                        let value = event_target::<HtmlInputElement>(&ev)
                                            .value()
                                            .parse::<f64>()
                                            .unwrap_or_default();
                                        pdf_settings.update(|settings| update_pdf_margin(settings, "right", value));
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Bottom Margin (in)"</span>
                                <input
                                    type="number"
                                    min="0"
                                    max="2"
                                    step="0.05"
                                    prop:value=move || format!("{:.2}", pdf_settings.get().margin_bottom_in)
                                    on:input=move |ev| {
                                        let value = event_target::<HtmlInputElement>(&ev)
                                            .value()
                                            .parse::<f64>()
                                            .unwrap_or_default();
                                        pdf_settings.update(|settings| update_pdf_margin(settings, "bottom", value));
                                    }
                                />
                            </label>
                            <label class="meta-field">
                                <span>"Left Margin (in)"</span>
                                <input
                                    type="number"
                                    min="0"
                                    max="2"
                                    step="0.05"
                                    prop:value=move || format!("{:.2}", pdf_settings.get().margin_left_in)
                                    on:input=move |ev| {
                                        let value = event_target::<HtmlInputElement>(&ev)
                                            .value()
                                            .parse::<f64>()
                                            .unwrap_or_default();
                                        pdf_settings.update(|settings| update_pdf_margin(settings, "left", value));
                                    }
                                />
                            </label>
                            <label class="meta-field full-width">
                                <span>"Header"</span>
                                <textarea
                                    rows="2"
                                    placeholder="{title}"
                                    prop:value=move || pdf_settings.get().header_template.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlTextAreaElement>(&ev).value();
                                        pdf_settings.update(|settings| settings.header_template = v);
                                    }
                                ></textarea>
                            </label>
                            <label class="meta-field full-width">
                                <span>"Footer"</span>
                                <textarea
                                    rows="2"
                                    placeholder="Page {page} of {total_pages}"
                                    prop:value=move || pdf_settings.get().footer_template.clone()
                                    on:input=move |ev| {
                                        let v = event_target::<HtmlTextAreaElement>(&ev).value();
                                        pdf_settings.update(|settings| settings.footer_template = v);
                                    }
                                ></textarea>
                            </label>
                            <div class="token-hint full-width">
                                "Tokens: {title}, {site_address}, {author}, {date}, {notes}, {page}, {total_pages}"
                            </div>
                            <label class="meta-field full-width checkbox-field">
                                <input
                                    type="checkbox"
                                    prop:checked=move || include_page_numbers.get()
                                    on:change=move |ev| {
                                        let checked = event_target::<HtmlInputElement>(&ev).checked();
                                        include_page_numbers.set(checked);
                                    }
                                />
                                <span>"Include page numbers"</span>
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
                                <span class="drop-zone-title">"Free Construction Photo Report Generator"</span>
                                <span class="drop-zone-steps">"Drag. Drop. Label. Export to PDF."</span>
                                <span class="drop-zone-privacy">"No Installs \u{00B7} No Uploads \u{00B7} 100% Private in Browser"</span>
                                <span class="drop-zone-divider" />
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
                                    // Internal reorder — stop the event from bubbling
                                    // up to the workspace `on_drop_files` handler, which
                                    // would otherwise treat the dragged <img> data
                                    // (auto-attached by Chromium) as a new file upload
                                    // and duplicate the photo.
                                    ev.stop_propagation();
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
                                            SOURCE_FILES.with(|m| { m.borrow_mut().remove(&photo_id); });
                                            photos.update(|items| items.retain(|i| i.id != photo_id));
                                        }
                                    };

                                    // Disable drag when an input/textarea is focused
                                    let row_focused = RwSignal::new(false);

                                    view! {
                                        <div
                                            draggable=move || if row_focused.get() { "false" } else { "true" }
                                            attr:data-photo-idx=move || idx().to_string()
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
                                                        if let Some(ind) = compute_drop_indicator(
                                                            from,
                                                            index,
                                                            rect.top(),
                                                            rect.height(),
                                                            ev.client_y() as f64,
                                                        ) {
                                                            drop_indicator.set(Some(ind));
                                                        } else {
                                                            drop_indicator.set(None);
                                                        }
                                                    }
                                                }
                                            }
                                            on:drop=move |ev: web_sys::DragEvent| {
                                                ev.prevent_default();
                                                if let Some(from) = drag_idx.get() {
                                                    ev.stop_propagation();
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
                                            on:pointerdown=move |ev: web_sys::PointerEvent| {
                                                // Mouse path stays on HTML5 DnD (handled by
                                                // dragstart/dragover/drop above), so the desktop
                                                // experience is unchanged.
                                                if ev.pointer_type() == "mouse" { return; }
                                                if row_focused.get() || pointer_target_is_interactive(&ev) {
                                                    return;
                                                }
                                                if !ev.is_primary() { return; }
                                                let x = ev.client_x() as f64;
                                                let y = ev.client_y() as f64;
                                                TOUCH_START.with(|s| s.set((x, y)));
                                                CURSOR_X.with(|c| c.set(x as i32));
                                                CURSOR_Y.with(|c| c.set(y as i32));
                                                let pointer_id = ev.pointer_id();
                                                if let Some(ct) = ev.current_target() {
                                                    if let Ok(el) = ct.dyn_into::<web_sys::HtmlElement>() {
                                                        // Capture the pointer so every pointermove/up
                                                        // fires on this element regardless of where
                                                        // the finger goes — the W3C-blessed way to
                                                        // implement drag.
                                                        let _ = el.set_pointer_capture(pointer_id);
                                                        TOUCH_SOURCE_EL.with(|s| *s.borrow_mut() = Some(el));
                                                    }
                                                }
                                                let source_idx = idx();
                                                let timer = gloo_timers::callback::Timeout::new(
                                                    TOUCH_HOLD_MS,
                                                    move || {
                                                        TOUCH_DRAG_ACTIVE.with(|a| a.set(true));
                                                        drag_idx.set(Some(source_idx));
                                                        drop_indicator.set(None);
                                                        body_class_add("touch-dragging");
                                                        let (sx, sy) = TOUCH_START.with(|s| s.get());
                                                        touch_ghost_attach(sx, sy);
                                                    },
                                                );
                                                TOUCH_HOLD.with(|h| *h.borrow_mut() = Some(timer));
                                            }
                                            on:pointermove=move |ev: web_sys::PointerEvent| {
                                                if ev.pointer_type() == "mouse" { return; }
                                                let x = ev.client_x() as f64;
                                                let y = ev.client_y() as f64;
                                                CURSOR_X.with(|c| c.set(x as i32));
                                                CURSOR_Y.with(|c| c.set(y as i32));
                                                // Pre-confirm: cancel hold timer if user is scrolling
                                                if !TOUCH_DRAG_ACTIVE.with(|a| a.get()) {
                                                    let (sx, sy) = TOUCH_START.with(|s| s.get());
                                                    if (x - sx).abs() > TOUCH_MOVE_THRESHOLD_PX
                                                        || (y - sy).abs() > TOUCH_MOVE_THRESHOLD_PX
                                                    {
                                                        TOUCH_HOLD.with(|h| h.borrow_mut().take());
                                                        TOUCH_SOURCE_EL.with(|s| s.borrow_mut().take());
                                                    }
                                                    return;
                                                }
                                                // Confirmed drag: update ghost + drop target
                                                touch_ghost_update(x, y);
                                                if let Some(from) = drag_idx.get() {
                                                    if let Some((target_idx, rect)) = target_index_at_point(x, y) {
                                                        if let Some(ind) = compute_drop_indicator(
                                                            from, target_idx, rect.top(), rect.height(), y,
                                                        ) {
                                                            drop_indicator.set(Some(ind));
                                                            TOUCH_LAST_INDICATOR.with(|l| l.set(Some(ind)));
                                                        } else {
                                                            drop_indicator.set(None);
                                                        }
                                                    }
                                                }
                                            }
                                            on:pointerup=move |ev: web_sys::PointerEvent| {
                                                if ev.pointer_type() == "mouse" { return; }
                                                let was_dragging = TOUCH_DRAG_ACTIVE.with(|a| a.get());
                                                if was_dragging {
                                                    if let Some(from) = drag_idx.get() {
                                                        let target_pos = drop_indicator
                                                            .get()
                                                            .or_else(|| TOUCH_LAST_INDICATOR.with(|l| l.get()))
                                                            .or_else(|| {
                                                                let x = CURSOR_X.with(|c| c.get()) as f64;
                                                                let y = CURSOR_Y.with(|c| c.get()) as f64;
                                                                resolve_drop_indicator_at_point(from, x, y)
                                                            });
                                                        if let Some(target_pos) = target_pos {
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
                                                }
                                                touch_drag_cleanup(drag_idx, drop_indicator);
                                            }
                                            on:pointercancel=move |ev: web_sys::PointerEvent| {
                                                if ev.pointer_type() == "mouse" { return; }
                                                touch_drag_cleanup(drag_idx, drop_indicator);
                                            }
                                        >
                                            <div class="thumb">
                                                <img
                                                    src=photo.thumb_url.clone()
                                                    alt=photo.title.clone()
                                                    loading="lazy"
                                                    style=photo.rotation_style()
                                                />
                                            </div>
                                            <div class="row-body">
                                                <div class="row-top">
                                                    <span class="row-idx">{move || format!("#{}", idx() + 1)}</span>
                                                    <span class="row-file" title=photo.filename.clone()>{photo.filename.clone()}</span>
                                                    <div class="row-actions">
                                                        <button
                                                            on:click=move |_| {
                                                                photos.update(|items| rotate_photo(items, photo_id, -1));
                                                            }
                                                            aria-label="Rotate counterclockwise"
                                                            title="Rotate counterclockwise"
                                                        >"\u{21BA}"</button>
                                                        <button
                                                            on:click=move |_| {
                                                                photos.update(|items| rotate_photo(items, photo_id, 1));
                                                            }
                                                            aria-label="Rotate clockwise"
                                                            title="Rotate clockwise"
                                                        >"\u{21BB}"</button>
                                                        <button class="btn-move" on:click=move_up disabled=move || idx() == 0 aria-label="Move up" title="Move up">"\u{25B2}"</button>
                                                        <button class="btn-move" on:click=move_down disabled=is_last aria-label="Move down" title="Move down">"\u{25BC}"</button>
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
                                                        photos.update(|items| update_photo_title(items, photo_id, v));
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
                                                        photos.update(|items| update_photo_description(items, photo_id, v));
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
                                {move || {
                                    let p = clamped_page.get();
                                    let t = total_pages.get();
                                    if has_cover_page.get() && p == 0 {
                                        format!("Title Page \u{2014} 1 / {t}")
                                    } else {
                                        format!("Page {} / {t}", p + 1)
                                    }
                                }}
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
                        let has_cover = has_cover_page.get();
                        let page_idx = clamped_page.get();

                        // --- Cover page preview ---
                        if has_cover && page_idx == 0 {
                            let m = meta.get();
                            let logo = m.logo_data_url.clone();
                            return view! {
                                <div class="page-card cover-preview">
                                    <div class="page-label">"TITLE PAGE"</div>
                                    <div class="cover-preview-content">
                                        {if !logo.is_empty() {
                                            Some(view! { <img class="cover-preview-logo" src=logo alt="Logo" /> })
                                        } else {
                                            None
                                        }}
                                        {if !m.title.is_empty() {
                                            Some(view! { <h2 class="cover-preview-title">{m.title.clone()}</h2> })
                                        } else {
                                            None
                                        }}
                                        {if !m.site_address.is_empty() {
                                            Some(view! { <p class="cover-preview-detail"><strong>"Site / Address: "</strong>{m.site_address.clone()}</p> })
                                        } else {
                                            None
                                        }}
                                        {if !m.author.is_empty() {
                                            Some(view! { <p class="cover-preview-detail"><strong>"Prepared By: "</strong>{m.author.clone()}</p> })
                                        } else {
                                            None
                                        }}
                                        {if !m.date.is_empty() {
                                            Some(view! { <p class="cover-preview-detail"><strong>"Date: "</strong>{m.date.clone()}</p> })
                                        } else {
                                            None
                                        }}
                                        {if !m.notes.is_empty() {
                                            Some(view! { <div class="cover-preview-notes"><strong>"Notes: "</strong><p>{m.notes.clone()}</p></div> })
                                        } else {
                                            None
                                        }}
                                    </div>
                                </div>
                            }.into_any();
                        }

                        // --- Photo page preview ---
                        let ly = layout.get();
                        let page_size = ly.page_size();
                        let num_cols = ly.cols();
                        let cols_class = if num_cols == 1 { "cols-1" } else { "cols-2" };
                        // Offset page index by cover page
                        let photo_page_idx = if has_cover { page_idx - 1 } else { page_idx };
                        // Borrow photos — only clone the single page we need
                        let (page, item_count) = photos.with(|items| {
                            let start = photo_page_idx * page_size;
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
                            let flat_idx = photo_page_idx * page_size + slot_idx;
                                    match slot {
                                        Some(photo) => {
                                            let title = photo.title.clone();
                                            let desc = photo.description.clone();
                                            let has_desc = !desc.is_empty();
                                            let rotation_style = photo.rotation_style();
                                            view! {
                                                <div
                                                    draggable="true"
                                                    attr:data-photo-idx=flat_idx.to_string()
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
                                                            ev.stop_propagation();
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
                                                    on:pointerdown=move |ev: web_sys::PointerEvent| {
                                                        if ev.pointer_type() == "mouse" { return; }
                                                        if pointer_target_is_interactive(&ev) { return; }
                                                        if !ev.is_primary() { return; }
                                                        let x = ev.client_x() as f64;
                                                        let y = ev.client_y() as f64;
                                                        TOUCH_START.with(|s| s.set((x, y)));
                                                        CURSOR_X.with(|c| c.set(x as i32));
                                                        CURSOR_Y.with(|c| c.set(y as i32));
                                                        let pointer_id = ev.pointer_id();
                                                        if let Some(ct) = ev.current_target() {
                                                            if let Ok(el) = ct.dyn_into::<web_sys::HtmlElement>() {
                                                                let _ = el.set_pointer_capture(pointer_id);
                                                                TOUCH_SOURCE_EL.with(|s| *s.borrow_mut() = Some(el));
                                                            }
                                                        }
                                                        let source_idx = flat_idx;
                                                        let timer = gloo_timers::callback::Timeout::new(
                                                            TOUCH_HOLD_MS,
                                                            move || {
                                                                TOUCH_DRAG_ACTIVE.with(|a| a.set(true));
                                                                drag_idx.set(Some(source_idx));
                                                                drop_indicator.set(None);
                                                                body_class_add("touch-dragging");
                                                                let (sx, sy) = TOUCH_START.with(|s| s.get());
                                                                touch_ghost_attach(sx, sy);
                                                            },
                                                        );
                                                        TOUCH_HOLD.with(|h| *h.borrow_mut() = Some(timer));
                                                    }
                                                    on:pointermove=move |ev: web_sys::PointerEvent| {
                                                        if ev.pointer_type() == "mouse" { return; }
                                                        let x = ev.client_x() as f64;
                                                        let y = ev.client_y() as f64;
                                                        CURSOR_X.with(|c| c.set(x as i32));
                                                        CURSOR_Y.with(|c| c.set(y as i32));
                                                        if !TOUCH_DRAG_ACTIVE.with(|a| a.get()) {
                                                            let (sx, sy) = TOUCH_START.with(|s| s.get());
                                                            if (x - sx).abs() > TOUCH_MOVE_THRESHOLD_PX
                                                                || (y - sy).abs() > TOUCH_MOVE_THRESHOLD_PX
                                                            {
                                                                TOUCH_HOLD.with(|h| h.borrow_mut().take());
                                                                TOUCH_SOURCE_EL.with(|s| s.borrow_mut().take());
                                                            }
                                                            return;
                                                        }
                                                        touch_ghost_update(x, y);
                                                        if let Some(from) = drag_idx.get() {
                                                            if let Some((target_idx, rect)) = target_index_at_point(x, y) {
                                                                let raw = if num_cols > 1 {
                                                                    let mid_x = rect.left() + rect.width() / 2.0;
                                                                    let mid_y = rect.top() + rect.height() / 2.0;
                                                                    let in_left = x <= mid_x;
                                                                    let in_top = y <= mid_y;
                                                                    if target_idx == from {
                                                                        target_idx
                                                                    } else if in_top || in_left {
                                                                        target_idx
                                                                    } else {
                                                                        target_idx + 1
                                                                    }
                                                                } else {
                                                                    let mid = rect.top() + rect.height() / 2.0;
                                                                    if y <= mid { target_idx } else { target_idx + 1 }
                                                                };
                                                                if target_idx == from {
                                                                    drop_indicator.set(None);
                                                                } else {
                                                                    let indicator = if raw == from || raw == from + 1 {
                                                                        if target_idx < from { target_idx } else { target_idx + 1 }
                                                                    } else {
                                                                        raw
                                                                    };
                                                                    drop_indicator.set(Some(indicator));
                                                                    TOUCH_LAST_INDICATOR.with(|l| l.set(Some(indicator)));
                                                                }
                                                            }
                                                        }
                                                    }
                                                    on:pointerup=move |ev: web_sys::PointerEvent| {
                                                        if ev.pointer_type() == "mouse" { return; }
                                                        let was_dragging = TOUCH_DRAG_ACTIVE.with(|a| a.get());
                                                        if was_dragging {
                                                            if let Some(from) = drag_idx.get() {
                                                                let target_pos = drop_indicator
                                                                    .get()
                                                                    .or_else(|| TOUCH_LAST_INDICATOR.with(|l| l.get()))
                                                                    .or_else(|| {
                                                                        let x = CURSOR_X.with(|c| c.get()) as f64;
                                                                        let y = CURSOR_Y.with(|c| c.get()) as f64;
                                                                        resolve_drop_indicator_at_point(from, x, y)
                                                                    });
                                                                if let Some(target_pos) = target_pos {
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
                                                        }
                                                        touch_drag_cleanup(drag_idx, drop_indicator);
                                                    }
                                                    on:pointercancel=move |ev: web_sys::PointerEvent| {
                                                        if ev.pointer_type() == "mouse" { return; }
                                                        touch_drag_cleanup(drag_idx, drop_indicator);
                                                    }
                                                >
                                                    <div class="slot-media">
                                                        <img
                                                            src=photo.preview_url
                                                            alt=title.clone()
                                                            loading="lazy"
                                                            style=rotation_style
                                                        />
                                                    </div>
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
                                }.into_any()
                    }}
                </div>
            </div>
        </div>

        // --- Support modal ---
        <Show when=move || show_support_modal.get()>
            <div class="modal-overlay" on:click=move |_| show_support_modal.set(false)>
                <div class="modal-card" on:click=move |ev: web_sys::MouseEvent| ev.stop_propagation()>
                    <h3 class="modal-title">"Gridora Forge just saved you time."</h3>
                    <p class="modal-body">"How much was that worth? If this tool is useful to you, consider buying me a coffee. It keeps development going."</p>
                    <div class="modal-actions">
                        <a class="btn btn-accent" href="https://buymeacoffee.com/rcoopersnyder" target="_blank" rel="noreferrer">"\u{2615} Buy Me a Coffee"</a>
                        <button class="btn btn-secondary" on:click=move |_| show_support_modal.set(false)>"Maybe Later"</button>
                        <button class="btn-text" on:click=move |_| {
                            show_support_modal.set(false);
                            if let Ok(Some(storage)) = web_sys::window()
                                .unwrap()
                                .local_storage()
                            {
                                let _ = storage.set_item("gridora_support_dismissed", "1");
                            }
                        }>"Don\u{2019}t show again"</button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}

// ---------------------------------------------------------------------------
// Tests
//
// These tests run on the host (`cargo test --target x86_64-pc-windows-msvc`
// or default host target) and exercise pure logic only. The wasm32 target has
// no `cfg(test)` harness, so anything here must avoid web_sys.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_photo(id: u64) -> PhotoItem {
        PhotoItem {
            id,
            title: format!("p{id}"),
            description: String::new(),
            filename: format!("p{id}.jpg"),
            mime: "image/jpeg".to_string(),
            rotation_quadrants: 0,
            thumb_url: String::new(),
            preview_url: String::new(),
        }
    }

    fn test_meta() -> ReportMeta {
        ReportMeta {
            title: "TestReport".to_string(),
            ..ReportMeta::default()
        }
    }

    // --- move_item invariants (covers both desktop drag and touch reorder
    //     since both code paths funnel through this function) -----------

    #[test]
    fn move_item_forward_preserves_other_order() {
        let mut v: Vec<PhotoItem> = (1..=5).map(make_photo).collect();
        move_item(&mut v, 0, 3);
        let ids: Vec<u64> = v.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![2, 3, 4, 1, 5]);
    }

    #[test]
    fn move_item_backward_preserves_other_order() {
        let mut v: Vec<PhotoItem> = (1..=5).map(make_photo).collect();
        move_item(&mut v, 4, 1);
        let ids: Vec<u64> = v.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![1, 5, 2, 3, 4]);
    }

    #[test]
    fn move_item_no_op_for_same_index() {
        let mut v: Vec<PhotoItem> = (1..=3).map(make_photo).collect();
        move_item(&mut v, 1, 1);
        let ids: Vec<u64> = v.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn move_item_out_of_bounds_is_noop() {
        let mut v: Vec<PhotoItem> = (1..=3).map(make_photo).collect();
        move_item(&mut v, 0, 10);
        let ids: Vec<u64> = v.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    // --- compute_drop_indicator: snap-out-of-deadzone semantics --------

    #[test]
    fn drop_indicator_above_midpoint_inserts_before_target() {
        // Source row 0 dragging onto row 3, finger near top → insert at 3
        let r = compute_drop_indicator(0, 3, 100.0, 60.0, 110.0);
        assert_eq!(r, Some(3));
    }

    #[test]
    fn drop_indicator_below_midpoint_inserts_after_target() {
        let r = compute_drop_indicator(0, 3, 100.0, 60.0, 150.0);
        assert_eq!(r, Some(4));
    }

    #[test]
    fn drop_indicator_on_source_returns_none() {
        let r = compute_drop_indicator(2, 2, 100.0, 60.0, 130.0);
        assert_eq!(r, None);
    }

    #[test]
    fn drop_indicator_avoids_dead_zone_above() {
        // Source 2, target 1, above midpoint → raw = 1 (= source - 1).
        // 1 == from would be false (from=2), 1 == from+1 = 3 false.
        // So the indicator should be 1 with no snap. Verify.
        let r = compute_drop_indicator(2, 1, 100.0, 60.0, 110.0);
        assert_eq!(r, Some(1));
    }

    #[test]
    fn drop_indicator_avoids_dead_zone_below() {
        // Source 2, target 2 → returns None (handled by source check).
        // Source 2, target 1, below midpoint → raw = 2 == from, snap.
        // target < from so result = target = 1.
        let r = compute_drop_indicator(2, 1, 100.0, 60.0, 150.0);
        assert_eq!(r, Some(1));
    }

    // --- Print HTML invariants (regression guards for mobile fix) -------

    #[test]
    fn print_html_uses_letter_dimensions_not_viewport() {
        let photos = vec![make_photo(1)];
        let mut export_bytes = HashMap::new();
        export_bytes.insert(1u64, vec![0xFFu8, 0xD8, 0xFF, 0xD9]); // tiny jpeg stub
        let meta = test_meta();
        let settings = PdfSettings::default();
        let html = build_print_html(
            &photos,
            &export_bytes,
            GridLayout::OneUp,
            &meta,
            &settings,
            false,
            false,
        );
        // Mobile fix: must use physical inch units, never the unreliable
        // mobile-viewport-relative `100vh`.
        assert!(
            !html.contains("100vh"),
            "print HTML still references 100vh; will misalign pages on mobile"
        );
        assert!(
            html.contains("11in"),
            "print HTML missing 11in page height"
        );
        assert!(
            html.contains("8.5in"),
            "print HTML missing 8.5in page width"
        );
        assert!(
            html.contains("@page"),
            "print HTML missing @page rule"
        );
    }

    #[test]
    fn print_html_includes_one_section_per_page() {
        // 5 photos at TwoByTwo (page size 4) = 2 photo pages
        let photos: Vec<PhotoItem> = (1..=5).map(make_photo).collect();
        let mut export_bytes = HashMap::new();
        for p in &photos {
            export_bytes.insert(p.id, vec![0xFFu8, 0xD8, 0xFF, 0xD9]);
        }
        let html = build_print_html(
            &photos,
            &export_bytes,
            GridLayout::TwoByTwo,
            &test_meta(),
            &PdfSettings::default(),
            false,
            false,
        );
        let section_count = html.matches(r#"<section class="page"#).count();
        assert_eq!(section_count, 2, "expected 2 photo pages for 5 photos at 2x2");
    }

    #[test]
    fn print_html_preserves_photo_order() {
        // Invariant: photo order in UI equals photo order in export
        let photos: Vec<PhotoItem> = (1..=4).map(make_photo).collect();
        let mut export_bytes = HashMap::new();
        for p in &photos {
            export_bytes.insert(p.id, vec![0xFFu8]);
        }
        let html = build_print_html(
            &photos,
            &export_bytes,
            GridLayout::OneUp,
            &test_meta(),
            &PdfSettings::default(),
            false,
            false,
        );
        let pos_p1 = html.find("alt=\"p1\"").expect("p1 missing");
        let pos_p2 = html.find("alt=\"p2\"").expect("p2 missing");
        let pos_p3 = html.find("alt=\"p3\"").expect("p3 missing");
        let pos_p4 = html.find("alt=\"p4\"").expect("p4 missing");
        assert!(pos_p1 < pos_p2);
        assert!(pos_p2 < pos_p3);
        assert!(pos_p3 < pos_p4);
    }
}
