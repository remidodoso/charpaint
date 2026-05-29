//! DOM construction and startup initialisation — called once from start() in lib.rs.
//!
//! Builds the initial HTML structure (grid cells, palette buttons), pre-computes
//! startup data (sprite catalog), and writes the demo content to the canvas.
//! Nothing here is called after startup; all event-handler wiring lives in wiring.rs.

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, Document, Element, HtmlCanvasElement};

use crate::{App, COLS, ROWS};
use crate::asciiart;

// ── Grid ─────────────────────────────────────────────────────────────────────

/// Populate `#grid` with COLS×ROWS `<span class="cell">` elements.
/// Returns the flat element vec stored in App for direct render access.
pub(crate) fn build_grid(document: &Document) -> Vec<Element> {
    let container = document
        .get_element_by_id("grid")
        .expect("#grid must exist in HTML");

    let mut cell_els = Vec::with_capacity(COLS * ROWS);
    for r in 0..ROWS {
        for c in 0..COLS {
            let el = document
                .create_element("span")
                .expect("create_element failed");
            el.set_class_name("cell");
            // Store position as data attributes for hit-testing in mouse handlers.
            el.set_attribute("data-col", &c.to_string()).unwrap();
            el.set_attribute("data-row", &r.to_string()).unwrap();
            el.set_text_content(Some(" "));
            container.append_child(&el).unwrap();
            cell_els.push(el);
        }
    }
    cell_els
}

// ── Palette ───────────────────────────────────────────────────────────────────

/// One entry in the character palette strip.
struct PalEntry {
    label:            &'static str, // visible glyph in the palette button
    ch:               char,         // actual character value painted by this entry
    initially_active: bool,
}

/// Populate `#palette` with character picker entries.
/// Each entry gets a `data-char` attribute read by the click handler in wire_palette.
pub(crate) fn build_palette(document: &Document) {
    let container = document
        .get_element_by_id("palette")
        .expect("#palette must exist in HTML");

    // "char:" label on the left edge of the palette strip
    let lbl = document.create_element("span").unwrap();
    lbl.set_class_name("palette-label");
    lbl.set_text_content(Some("char:"));
    container.append_child(&lbl).unwrap();

    // None = visual separator between groups
    // TBD: add more character groups (box-drawing styles, user custom chars, etc.)
    let entries: &[Option<PalEntry>] = &[
        Some(PalEntry { label: "█", ch: '█', initially_active: false }),
        Some(PalEntry { label: "▓", ch: '▓', initially_active: false }),
        Some(PalEntry { label: "▒", ch: '▒', initially_active: false }),
        Some(PalEntry { label: "░", ch: '░', initially_active: false }),
        None,
        Some(PalEntry { label: "#", ch: '#', initially_active: false }),
        Some(PalEntry { label: "*", ch: '*', initially_active: true }),
        Some(PalEntry { label: "+", ch: '+', initially_active: false }),
        Some(PalEntry { label: ".", ch: '.', initially_active: false }),
        None,
        Some(PalEntry { label: "─", ch: '─', initially_active: false }),
        Some(PalEntry { label: "│", ch: '│', initially_active: false }),
        Some(PalEntry { label: "┼", ch: '┼', initially_active: false }),
        None,
        Some(PalEntry { label: "⣠", ch: ' ', initially_active: false }), // space = erase
    ];

    for entry in entries {
        match entry {
            None => {
                // Visual separator between palette groups
                let sep = document.create_element("div").unwrap();
                sep.set_class_name("pal-sep");
                container.append_child(&sep).unwrap();
            }
            Some(e) => {
                let el = document.create_element("div").unwrap();
                el.set_class_name(if e.initially_active { "pal-char active" } else { "pal-char" });
                el.set_text_content(Some(e.label));
                // data-char carries the actual character value to the click handler.
                el.set_attribute("data-char", &e.ch.to_string()).unwrap();
                container.append_child(&el).unwrap();
            }
        }
    }
}

// ── Sprite catalogs ───────────────────────────────────────────────────────────

/// Build the ASCII7 sprite catalog: printable ASCII 32–126 (95 chars).
pub(crate) fn build_ascii7_catalog(document: &Document) -> Vec<Vec<u8>> {
    build_catalog_for_codes(document, 32u32..=126u32)
}

/// Build the braille sprite catalog: Unicode braille U+2800–U+28FF (256 chars,
/// covering both 6-dot and 8-dot patterns).
pub(crate) fn build_braille_catalog(document: &Document) -> Vec<Vec<u8>> {
    build_catalog_for_codes(document, 0x2800u32..=0x28FFu32)
}

/// Shared canvas-rendering core. Renders each Unicode code point in `code_range`
/// as a white-stroke-on-black glyph at SPRITE_W × SPRITE_H px and returns the
/// flat grayscale bitmaps. An invalid code point pushes an all-black placeholder
/// so catalog indices stay aligned with the input range.
fn build_catalog_for_codes(
    document:   &Document,
    code_range: std::ops::RangeInclusive<u32>,
) -> Vec<Vec<u8>> {
    let canvas = match document
        .create_element("canvas").ok()
        .and_then(|el| el.dyn_into::<HtmlCanvasElement>().ok())
    {
        Some(c) => c,
        None    => return Vec::new(),
    };
    canvas.set_width(asciiart::SPRITE_W);
    canvas.set_height(asciiart::SPRITE_H);

    let ctx = match canvas
        .get_context("2d").ok().flatten()
        .and_then(|obj| obj.dyn_into::<CanvasRenderingContext2d>().ok())
    {
        Some(c) => c,
        None    => return Vec::new(),
    };

    let sw = asciiart::SPRITE_W as f64;
    let sh = asciiart::SPRITE_H as f64;
    // top-left origin keeps glyphs within the sprite bounds.
    ctx.set_font(&format!("{}px monospace", asciiart::SPRITE_H));
    ctx.set_text_align("left");
    ctx.set_text_baseline("top");

    let len = (code_range.end() - code_range.start() + 1) as usize;
    let mut catalog = Vec::with_capacity(len);
    let blank = vec![0u8; (asciiart::SPRITE_W * asciiart::SPRITE_H) as usize];

    for code in code_range {
        let ch = match char::from_u32(code) {
            Some(c) => c,
            None    => { catalog.push(blank.clone()); continue; }
        };
        ctx.set_fill_style_str("black");
        ctx.fill_rect(0.0, 0.0, sw, sh);
        ctx.set_fill_style_str("white");
        let _ = ctx.fill_text(&ch.to_string(), 0.0, 0.0);

        match ctx.get_image_data(0.0, 0.0, sw, sh) {
            Ok(pixels) => {
                // RGBA → grayscale: canvas is B&W so R = G = B; take R channel.
                let gray: Vec<u8> = pixels.data().0.chunks(4).map(|px| px[0]).collect();
                catalog.push(gray);
            }
            Err(_) => catalog.push(blank.clone()),
        }
    }

    catalog
}

// ── Demo content ──────────────────────────────────────────────────────────────

/// Draw a border and greeting to prove the Rust→DOM rendering pipeline works.
/// Lives here because it may grow into something more elaborate; move elsewhere
/// if it ever becomes significant logic.
pub(crate) fn draw_demo(app: &mut App) {
    // Top and bottom edges
    for c in 0..COLS {
        app.grid.set_committed(c, 0,        '─');
        app.grid.set_committed(c, ROWS - 1, '─');
    }
    // Left and right edges
    for r in 0..ROWS {
        app.grid.set_committed(0,        r, '│');
        app.grid.set_committed(COLS - 1, r, '│');
    }
    // Corners
    app.grid.set_committed(0,        0,        '┌');
    app.grid.set_committed(COLS - 1, 0,        '┐');
    app.grid.set_committed(0,        ROWS - 1, '└');
    app.grid.set_committed(COLS - 1, ROWS - 1, '┘');

    // Centered greeting
    let msg: Vec<char> = "✦  charpaint  ✦".chars().collect();
    let col0 = (COLS - msg.len()) / 2;
    for (i, &ch) in msg.iter().enumerate() {
        app.grid.set_committed(col0 + i, ROWS / 2, ch);
    }

    app.render_all();
}
