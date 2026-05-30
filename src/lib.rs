//! charpaint — a MacPaint-style character-cell painting app compiled to WebAssembly.
//!
//! All application logic lives here. The HTML/JS side is a minimal structural
//! shell; after `init()` is awaited in JS, this module owns the DOM and all
//! interactivity.

use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlInputElement, Window};

mod util;
use util::bresenham;

mod asciiart;
mod dom_setup;

mod wiring;
use wiring::{wire_grid_mouse, wire_toolbar, wire_palette, wire_undo_redo, wire_theme_toggle, wire_blend_mode, wire_line_tool, wire_pencil_tool, wire_fill_tool, wire_copy, wire_clear, wire_touch, wire_shift_toggle, wire_text_input, wire_help, wire_drag_drop, wire_outline_mode, wire_aa_mode, wire_aa_charset, wire_bg_visibility, wire_bg_move_tool, wire_load_image, wire_image_controls};

// Help strings generated from locales/help.en.yaml by build.rs.
include!(concat!(env!("OUT_DIR"), "/help_strings.rs"));

// ── Constants ────────────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

/// Maximum texture sharpening step. Steps 1–MAX each add 0.5× USM factor (so MAX=5 → up to 2.5×).
const MAX_TEXTURE: u32 = 5;
/// Maximum pop sharpening step. Same scale as MAX_TEXTURE.
const MAX_POP: u32 = 5;

/// The character placed in a cell when it is blank / erased.
const BLANK: char = ' ';

/// The block character used as the blinking text cursor.
const CURSOR_CHAR: char = '▄'; // LOWER HALF BLOCK U+2584

// ── BlendMode ────────────────────────────────────────────────────────────────

/// How the brush interacts with cells that already contain content.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum BlendMode {
    Overwrite,  // replace any cell unconditionally — classic paint behaviour
    Stamp,      // only write into blank cells; occupied cells are left untouched
    Combine,    // NIY — set-union of glyphs (TBD: exact visual definition)
    Difference, // NIY — set-difference / punch-out mode
}

impl BlendMode {
    /// Unicode icon for the mode, shown in the mode button.
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            BlendMode::Overwrite  => "▊",
            BlendMode::Stamp      => "⬚",
            BlendMode::Combine    => "┼",
            BlendMode::Difference => "∖",
        }
    }

    /// Human-readable name for button titles.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            BlendMode::Overwrite  => "Overwrite",
            BlendMode::Stamp      => "Stamp",
            BlendMode::Combine    => "Combine",
            BlendMode::Difference => "Difference",
        }
    }

    /// Advance to the next implemented blend mode.
    /// Combine and Difference are NIY — excluded from the cycle.
    pub(crate) fn cycle(&self) -> Self {
        match self {
            BlendMode::Overwrite => BlendMode::Stamp,
            _                    => BlendMode::Overwrite,
        }
    }
}

// ── LineMode ─────────────────────────────────────────────────────────────────

/// Whether the line tool stamps the brush character or chooses - | \ / based on direction.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LineMode {
    Character, // stamp brush_char along the Bresenham path — simple, any character
    Art,       // select - | \ / per step direction — classic ASCII-art geometry
}

impl LineMode {
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            LineMode::Character => "╲",
            LineMode::Art       => "📐",
        }
    }

    /// Advance to the next line mode.
    pub(crate) fn cycle(&self) -> Self {
        match self {
            LineMode::Character => LineMode::Art,
            LineMode::Art       => LineMode::Character,
        }
    }
}

// ── PencilMode ────────────────────────────────────────────────────────────────

/// Whether the pencil stamps the brush character or selects - | \ / per step direction.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PencilMode {
    Normal, // stamp brush_char — classic freehand paint
    Art,    // select - | \ / per step direction — smooth geometric strokes
}

impl PencilMode {
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            PencilMode::Normal => "✎",
            PencilMode::Art    => "~",
        }
    }

    /// Advance to the next pencil mode.
    pub(crate) fn cycle(&self) -> Self {
        match self {
            PencilMode::Normal => PencilMode::Art,
            PencilMode::Art    => PencilMode::Normal,
        }
    }
}

// ── FillMode ─────────────────────────────────────────────────────────────────

/// Whether the fill tool spreads through orthogonal neighbours only (4-adjacent)
/// or all 8 neighbours including diagonals (8-adjacent).
/// Flood4 is the default — it respects outline shapes drawn with the oval/rect tools.
/// Flood8 is useful for filling regions not fully enclosed by a solid border.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FillMode {
    Flood4, // up/down/left/right only — doesn't leak through diagonal gaps in outlines
    Flood8, // all 8 directions — fills corner-connected regions too
}

impl FillMode {
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            FillMode::Flood4 => "✣",
            FillMode::Flood8 => "❊",
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            FillMode::Flood4 => "Fill (4-adjacent)",
            FillMode::Flood8 => "Fill (8-adjacent)",
        }
    }

    pub(crate) fn cycle(&self) -> Self {
        match self {
            FillMode::Flood4 => FillMode::Flood8,
            FillMode::Flood8 => FillMode::Flood4,
        }
    }
}

// ── BgOutlineMode ─────────────────────────────────────────────────────────────

/// How the background image is rendered: as the processed luminance, or as an
/// edge map derived from it. The edge scale is set by NOTIONAL_CELL_PX so edges
/// correspond to character-cell-sized features regardless of the image resolution.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum BgOutlineMode {
    Original,      // processed luma — continuous-tone grayscale background
    WhiteOnBlack,  // Sobel edge map — bright edges on dark field
    BlackOnWhite,  // Sobel edge map inverted — dark edges on light field
}

impl BgOutlineMode {
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            BgOutlineMode::Original     => "▒",
            BgOutlineMode::WhiteOnBlack => "┼",
            BgOutlineMode::BlackOnWhite => "╬",
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            BgOutlineMode::Original     => "Original",
            BgOutlineMode::WhiteOnBlack => "White on black",
            BgOutlineMode::BlackOnWhite => "Black on white",
        }
    }

    /// Cycle: Original → WhiteOnBlack → BlackOnWhite → Original.
    pub(crate) fn cycle(&self) -> Self {
        match self {
            BgOutlineMode::Original     => BgOutlineMode::WhiteOnBlack,
            BgOutlineMode::WhiteOnBlack => BgOutlineMode::BlackOnWhite,
            BgOutlineMode::BlackOnWhite => BgOutlineMode::Original,
        }
    }
}

// ── AaCharset ────────────────────────────────────────────────────────────────

/// Which character set the AA brush draws from.
/// Each variant has its own sprite catalog built at startup.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AaCharset {
    Ascii7,  // printable ASCII 32–126 (95 chars) — default
    Braille, // Unicode braille 0x2800–0x28FF (256 chars, 6-dot + 8-dot combined)
}

impl AaCharset {
    /// Icon shown in the charset-mode button.
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            AaCharset::Ascii7  => "a",
            AaCharset::Braille => "⠵", // braille 'z' — evokes the Game of Life glider
        }
    }

    /// Cycle through available charsets.
    pub(crate) fn cycle(&self) -> Self {
        match self {
            AaCharset::Ascii7  => AaCharset::Braille,
            AaCharset::Braille => AaCharset::Ascii7,
        }
    }

    /// Map a catalog index back to the character it represents.
    pub(crate) fn char_from_idx(&self, idx: usize) -> char {
        match self {
            AaCharset::Ascii7  => (idx as u8 + 32) as char,
            // All code points in U+2800–U+28FF are valid Unicode; unwrap_or is defensive only.
            AaCharset::Braille => char::from_u32(0x2800 + idx as u32).unwrap_or('\u{2800}'),
        }
    }
}

// ── Axis ─────────────────────────────────────────────────────────────────────

/// Constraint axis for Shift-locked drawing. Once determined for a stroke,
/// never changes — releasing Shift mid-stroke does not un-constrain.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Axis { Horizontal, Vertical }

// ── Tool ─────────────────────────────────────────────────────────────────────

/// Every tool the toolbar can activate.
/// Variants marked NIY are declared so the data model is complete but their
/// drawing logic is not yet implemented — they will no-op gracefully.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Tool {
    Pencil,
    Eraser,
    Select,   // NIY
    Fill,
    Text,
    Line,
    Rect,
    RectFill,
    Oval,
    OvalFill,
    BgMove,   // pan/zoom the background image; no canvas painting
}

impl Tool {
    /// Map a Tool variant back to its `data-tool` HTML attribute value.
    /// Used when restoring the `.active` class to the previous tool's button.
    pub(crate) fn data_attr(&self) -> &'static str {
        match self {
            Tool::Pencil   => "pencil",
            Tool::Eraser   => "eraser",
            Tool::Select   => "select",
            Tool::Fill     => "fill",
            Tool::Text     => "text",
            Tool::Line     => "line",
            Tool::Rect     => "rect",
            Tool::RectFill => "rect-fill",
            Tool::Oval     => "oval",
            Tool::OvalFill => "oval-fill",
            Tool::BgMove   => "bg-move",
        }
    }

    /// Map the `data-tool` HTML attribute value to a Tool variant.
    pub(crate) fn from_data_attr(s: &str) -> Option<Self> {
        match s {
            "pencil"    => Some(Tool::Pencil),
            "eraser"    => Some(Tool::Eraser),
            "select"    => Some(Tool::Select),
            "fill"      => Some(Tool::Fill),
            "text"      => Some(Tool::Text),
            "line"      => Some(Tool::Line),
            "rect"      => Some(Tool::Rect),
            "rect-fill" => Some(Tool::RectFill),
            "oval"      => Some(Tool::Oval),
            "oval-fill" => Some(Tool::OvalFill),
            "bg-move"   => Some(Tool::BgMove),
            _           => None,
        }
    }
}

// ── Grid ─────────────────────────────────────────────────────────────────────

/// The character canvas: a committed backing store plus an ephemeral preview
/// layer for showing in-progress tool operations before mouseup.
struct Grid {
    /// Authoritative canvas state. Each undo snapshot is a clone of this vec.
    committed: Vec<char>,
    /// Per-cell override shown while a tool drag is in progress.
    /// `None` means "fall through to the committed value."
    /// Cleared on mouseup (commit) or ESC (abort).
    preview: Vec<Option<char>>,
}

impl Grid {
    fn new() -> Self {
        Grid {
            committed: vec![BLANK; COLS * ROWS],
            preview:   vec![None;  COLS * ROWS],
        }
    }

    /// Flat index into the committed/preview vecs.
    fn idx(col: usize, row: usize) -> usize {
        row * COLS + col
    }

    /// The character that should be displayed right now — preview takes priority.
    fn display_char(&self, col: usize, row: usize) -> char {
        self.preview[Self::idx(col, row)]
            .unwrap_or(self.committed[Self::idx(col, row)])
    }

    fn set_committed(&mut self, col: usize, row: usize, ch: char) {
        self.committed[Self::idx(col, row)] = ch;
    }

    fn set_preview(&mut self, col: usize, row: usize, ch: Option<char>) {
        self.preview[Self::idx(col, row)] = ch;
    }

    /// Move all preview cells into committed state; return the affected positions.
    /// Called on mouseup to finalise a tool operation.
    fn commit_preview(&mut self) -> Vec<(usize, usize)> {
        let mut dirty = Vec::new();
        for r in 0..ROWS {
            for c in 0..COLS {
                if let Some(ch) = self.preview[Self::idx(c, r)].take() {
                    self.committed[Self::idx(c, r)] = ch;
                    dirty.push((c, r));
                }
            }
        }
        dirty
    }

    /// Discard all preview cells without committing; return the affected positions.
    /// Called on ESC to cancel an in-progress operation.
    fn abort_preview(&mut self) -> Vec<(usize, usize)> {
        let mut dirty = Vec::new();
        for r in 0..ROWS {
            for c in 0..COLS {
                if self.preview[Self::idx(c, r)].take().is_some() {
                    dirty.push((c, r));
                }
            }
        }
        dirty
    }
}

// ── Application state ────────────────────────────────────────────────────────

pub(crate) struct App {
    grid: Grid,
    /// Flat list of DOM `<span class="cell">` elements, one per grid cell,
    /// indexed `[row * COLS + col]`. Stored here to avoid repeated getElementById
    /// lookups — rendering just writes directly into these elements.
    cell_els: Vec<Element>,

    pub(crate) tool:       Tool,
    pub(crate) brush_char: char, // character the pencil/fill paints with
    pub(crate) is_drawing: bool,
    /// Cell where the current stroke began. Used for Shift-axis projection and,
    /// eventually, for preview-based tools (line, rect, oval).
    pub(crate) draw_start: Option<(usize, usize)>,
    /// Axis locked by Shift-constrain. Determined on first significant movement
    /// under Shift; persists even if Shift is released before mouseup.
    pub(crate) locked_axis: Option<Axis>,
    /// Last cell painted during the current stroke, used by Bresenham interpolation
    /// to fill gaps when the mouse moves faster than mousemove events fire.
    /// Cleared on mouseup / mousedown start.
    pub(crate) last_painted_cell: Option<(usize, usize)>,

    // TBD: draw_start: Option<(usize, usize)> — needed by line/rect/oval tools
    //      to remember where the drag began so preview can be redrawn on each
    //      mousemove from start→current rather than just stamping each cell.

    /// Undo history — each entry is a full snapshot of `grid.committed`.
    /// Pushed on every mouseup commit. Cheap: 80×24×4 B ≈ 7.5 KB per entry.
    undo_stack: Vec<Vec<char>>,
    /// States available for redo. Populated by undo(); cleared by new commits.
    redo_stack: Vec<Vec<char>>,

    /// True until the user first touches the canvas. While true, the demo
    /// content drawn by draw_demo() is still showing. Cleared by
    /// clear_demo_if_active() which is called at the top of every mousedown
    /// and touchstart handler so the demo is never captured in undo history.
    pub(crate) demo_active: bool,
    /// True from first load (or future New Project) until the user either paints
    /// on the canvas or loads a background image. Used to auto-enter BgMove on
    /// the first image drop. TBD: reset to true by a future "New Project" action.
    pub(crate) is_new_project: bool,

    pub(crate) shift_locked: bool,       // true when the ⇧ toggle is active (axis constraint)
    pub(crate) dark_mode: bool,
    pub(crate) blend_mode:   BlendMode,   // how new paint interacts with existing cells
    pub(crate) line_mode:    LineMode,    // character-stamp vs art-geometry line drawing
    pub(crate) pencil_mode:  PencilMode,  // normal freehand vs art-geometry pencil
    pub(crate) fill_mode:    FillMode,    // 4-adjacent vs 8-adjacent flood fill

    /// Active selection bounding box (c0, r0, c1, r1), normalized so c0≤c1, r0≤r1.
    /// None means no selection. Cleared on tool switch or ESC; persists after mouseup.
    pub(crate) selection: Option<(usize, usize, usize, usize)>,

    /// Next cell position to type into. Some((col, row)) where col may == COLS
    /// (meaning the cursor is past the right edge; no visual cursor shown).
    /// None when no text session is active.
    pub(crate) text_cursor: Option<(usize, usize)>,
    /// Cell where the current text session began. Used by ESC to restart the
    /// cursor at the session origin after discarding all typed characters.
    pub(crate) text_origin: Option<(usize, usize)>,

    /// The off-screen hidden `<input>` focused to raise the mobile virtual
    /// keyboard when a text session is active. Blurred on session end.
    text_input_el: HtmlInputElement,

    /// Blob URL of the current background image, kept for revocation when a new
    /// image is dropped. None means no background image is active.
    pub(crate) bg_image_url: Option<String>,
    /// Displayed dimensions of the background image in CSS pixels. Set to cover+center
    /// on image drop, then updated interactively by the BgMove tool.
    pub(crate) bg_disp_w: f64,
    pub(crate) bg_disp_h: f64,
    /// Position of the image's top-left corner in CSS grid pixels. Negative values
    /// are valid — the image can be partially panned off the grid.
    pub(crate) bg_pos_x: f64,
    pub(crate) bg_pos_y: f64,
    /// Drag origin for the BgMove tool: (client_x, client_y, bg_pos_x, bg_pos_y)
    /// at the moment mousedown fired. None when not dragging.
    bg_drag_start: Option<(f64, f64, f64, f64)>,
    /// Background display width captured at pinch-start. Used in touchmove to
    /// compute the absolute scale ratio (pinch_start_bg_disp_w × ratio = new width).
    pub(crate) pinch_start_bg_disp_w: f64,
    /// Tool that was active before BgMove was entered. Restored on accept/cancel/exit.
    pub(crate) prev_tool: Option<Tool>,
    /// Background layout snapshot taken when BgMove was entered. Restored on ESC.
    bg_move_saved: Option<(f64, f64, f64, f64)>,

    /// Raw luminance after to_luminance + stretch_luminance, before contrast adjustment.
    /// Stored so rebuild_from_params can re-derive bg_luma without reloading the image.
    /// None until an image has been dropped and processed.
    pub(crate) bg_luma_raw: Option<Vec<u8>>,

    /// Contrast-adjusted luminance derived from bg_luma_raw.
    /// Recomputed by rebuild_from_params whenever contrast_index changes.
    /// None until first rebuild_from_params call after image load.
    pub(crate) bg_luma:        Option<Vec<u8>>,
    pub(crate) bg_luma_width:  u32,
    pub(crate) bg_luma_height: u32,  // current processed image height
    /// Dimensions of bg_luma_raw — the original downsample from the loaded image.
    /// Never changes after image load; used for zoom limits and raw-coordinate math.
    pub(crate) bg_luma_raw_width:  u32,
    pub(crate) bg_luma_raw_height: u32,
    /// Generation counter for the zoom debounce timer. Bumped on every zoom event;
    /// a pending timeout only reprocesses if its captured gen still matches.
    pub(crate) zoom_debounce_gen: u32,

    /// Texture sharpening step in [0, MAX_TEXTURE]; 0 = no sharpening.
    /// Applied to bg_luma_raw via unsharp mask to produce bg_luma before edge detection.
    pub(crate) texture_amount: u32,
    /// Pop (large-radius) sharpening step in [0, MAX_POP]; 0 = no sharpening.
    /// Applied after texture sharpening in rebuild_from_params.
    pub(crate) pop_amount: u32,

    /// Sobel + enhance edge map derived from bg_luma at the same dimensions.
    /// Used by compute_best_char: edge features (bright = present) are matched
    /// against inverted sprites (white strokes on black) via SSD, so character
    /// stroke directions align with edge directions in the image.
    pub(crate) bg_edges: Option<Vec<u8>>,

    /// True when the background image is deliberately hidden by the user.
    /// bg_image_css is preserved so toggling back re-shows without reprocessing.
    pub(crate) bg_hidden: bool,
    /// Reference to #bg-visibility-btn for enable/active class management.
    bg_eye_el: Element,

    /// Cached visible rectangle of the processed image behind the grid, in
    /// processed-image pixel coordinates: (x0, y0, width, height).
    /// Mirrors CSS background-size/position: center so cell-to-image mapping
    /// matches what is actually displayed. Recomputed by refresh_visible_rect()
    /// whenever the grid layout or background image changes. Avoids calling
    /// client_width/height (which forces a browser reflow) per painted cell.
    bg_visible_rect: (f64, f64, f64, f64),

    /// How the background image is rendered: original luma, white-on-black edges,
    /// or black-on-white edges. Cycled by #outline-mode-btn; persists across drops.
    pub(crate) bg_outline_mode: BgOutlineMode,

    /// True when the AA brush mode is active. When on, each cell stamped by any
    /// drawing tool receives the catalog character that best matches the image
    /// region under it, rather than the fixed brush_char from the palette.
    pub(crate) aa_mode: bool,
    /// Reference to #aa-mode-btn for enable/disable when the background changes.
    /// Stored here so process_bg_image and future clear-image code can update it.
    aa_btn_el: Element,
    /// Active AA charset — selects which catalog compute_best_char searches.
    pub(crate) aa_charset: AaCharset,
    /// Sprite catalog for printable ASCII 32–126.  Empty until built at startup.
    pub(crate) catalog_ascii7: Vec<Vec<u8>>,
    /// Sprite catalog for Unicode braille U+2800–U+28FF. Empty until built at startup.
    pub(crate) catalog_braille: Vec<Vec<u8>>,
    /// Reference to #aa-charset-btn for enable/disable in sync with aa_mode.
    aa_charset_btn_el: Element,

    /// True while the help-mode overlay is active.
    /// When on, pointer events are captured by #help-overlay and routed to
    /// the help popup instead of the canvas/toolbar.
    pub(crate) help_mode: bool,

    /// Reference to #image-controls strip — shown by Rust when BgMove is active.
    image_controls_el: Element,
    /// Reference to #image-controls-hide checkbox — unchecked each time the strip is shown
    /// so it always starts fresh when BgMove is re-entered.
    image_controls_hide_el: web_sys::HtmlInputElement,
    /// #bg-move-btn — held so enter/accept/cancel_bg_move can add/remove `.blinking`.
    bg_move_btn_el: Element,

    // ── Touch / pinch-zoom state ─────────────────────────────────────────────
    grid_el:                          Element,      // #grid element — target for font-size changes
    pub(crate) font_size:             f64,          // current grid font size in px (default 16)
    pub(crate) is_two_finger:         bool,         // true while two touches are on screen
    pub(crate) pinch_start_dist:      f64,          // finger separation when current pinch began
    pub(crate) pinch_start_font_size: f64,          // font_size at the start of current pinch
    pub(crate) pan_last_mid:          (f64, f64),   // last two-touch midpoint, for pan delta
}

impl App {
    fn new(cell_els: Vec<Element>, grid_el: Element, text_input_el: HtmlInputElement, aa_btn_el: Element, bg_eye_el: Element, aa_charset_btn_el: Element, image_controls_el: Element, image_controls_hide_el: web_sys::HtmlInputElement, bg_move_btn_el: Element) -> Self {
        App {
            grid: Grid::new(),
            cell_els,
            tool:       Tool::Pencil,
            brush_char: '*',
            is_drawing: false,
            draw_start:  None,
            locked_axis: None,
            last_painted_cell: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            demo_active: true,
            is_new_project: true,
            shift_locked: false,
            dark_mode:  true,
            blend_mode:   BlendMode::Overwrite,
            line_mode:    LineMode::Character,
            pencil_mode:  PencilMode::Normal,
            fill_mode:    FillMode::Flood4,
            selection: None,
            text_cursor: None,
            text_origin: None,
            text_input_el,
            grid_el,
            font_size:             16.0,
            is_two_finger:         false,
            pinch_start_dist:      0.0,
            pinch_start_font_size: 16.0,
            pan_last_mid:          (0.0, 0.0),
            bg_image_url:          None,
            bg_disp_w:             0.0,
            bg_disp_h:             0.0,
            bg_pos_x:              0.0,
            bg_pos_y:              0.0,
            bg_drag_start:         None,
            pinch_start_bg_disp_w: 0.0,
            prev_tool:             None,
            bg_move_saved:         None,
            bg_luma_raw:           None,
            bg_luma:               None,
            bg_luma_width:         0,
            bg_luma_height:        0,
            bg_luma_raw_width:     0,
            bg_luma_raw_height:    0,
            zoom_debounce_gen:     0,
            texture_amount:        0,
            pop_amount:            0,
            bg_edges:              None,
            bg_hidden:             false,
            bg_eye_el,
            bg_visible_rect:       (0.0, 0.0, 0.0, 0.0),
            bg_outline_mode:       BgOutlineMode::Original,
            aa_mode:               false,
            aa_btn_el,
            aa_charset:            AaCharset::Ascii7,
            catalog_ascii7:        Vec::new(),
            catalog_braille:       Vec::new(),
            aa_charset_btn_el,
            help_mode:             false,
            image_controls_el,
            image_controls_hide_el,
            bg_move_btn_el,
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Push the current display character for one cell to its DOM element,
    /// and toggle the `.preview` CSS class to show the in-progress tint.
    fn render_cell(&self, col: usize, row: usize) {
        let idx = Grid::idx(col, row);
        let el  = &self.cell_els[idx];
        let ch  = self.grid.display_char(col, row);

        // Space must not be HTML-collapsed; CSS `white-space: pre` on .cell handles this.
        el.set_text_content(Some(&ch.to_string()));

        // Preview tint: add class while cell has an uncommitted in-progress value.
        let cl = el.class_list();
        if self.grid.preview[idx].is_some() {
            cl.add_1("preview").unwrap();
        } else {
            cl.remove_1("preview").unwrap();
        }
    }

    /// Re-render the entire grid — used after undo/redo or a full state restore.
    fn render_all(&self) {
        for r in 0..ROWS {
            for c in 0..COLS {
                self.render_cell(c, r);
            }
        }
    }

    // ── Demo ─────────────────────────────────────────────────────────────────

    /// Wipe the intro demo content and re-render the now-blank canvas.
    /// Called at the top of every mousedown/touchstart handler, before any
    /// undo snapshot is pushed, so demo content never appears in undo history.
    /// Safe to call repeatedly — does nothing once demo_active is false.
    pub(crate) fn clear_demo_if_active(&mut self) {
        if !self.demo_active { return; }
        for cell in self.grid.committed.iter_mut() { *cell = BLANK; }
        self.render_all();
        self.demo_active = false;
        self.is_new_project = false; // painting before an image load ends new-project state
    }

    // ── Painting ─────────────────────────────────────────────────────────────

    /// Stamp a single character into the committed grid and update its DOM cell.
    fn paint_cell(&mut self, col: usize, row: usize, ch: char) {
        self.grid.set_committed(col, row, ch);
        self.render_cell(col, row);
    }

    /// Clear all preview cells and re-render them without aborting the stroke.
    /// Also resets `last_painted_cell` to `draw_start` so the very next Bresenham
    /// call redraws a clean constrained line from the stroke origin forward.
    fn clear_preview_for_snap(&mut self) {
        let dirty = self.grid.abort_preview();
        for (c, r) in dirty {
            self.render_cell(c, r);
        }
        self.last_painted_cell = self.draw_start; // Bresenham restarts from origin
    }

    /// Resolve the actual target cell for a paint step, applying Shift constraint.
    ///
    /// If the axis is already locked, project onto it regardless of current Shift state.
    /// If Shift is newly held, compare dx/dy from draw_start to determine the axis;
    /// on first lock, snap the existing preview retroactively to a clean straight line.
    /// Ties (dx == dy) defer locking until one axis pulls ahead.
    fn resolve_target(&mut self, col: usize, row: usize, shift_held: bool) -> (usize, usize) {
        let start = match self.draw_start {
            Some(s) => s,
            None    => return (col, row),
        };

        // Already locked — project regardless of whether Shift is still held.
        if let Some(axis) = self.locked_axis {
            return match axis {
                Axis::Horizontal => (col,     start.1),
                Axis::Vertical   => (start.0, row),
            };
        }

        // Not yet locked — try to determine axis if Shift is held.
        if shift_held {
            let dx = col.abs_diff(start.0);
            let dy = row.abs_diff(start.1);
            if dx > dy {
                self.locked_axis = Some(Axis::Horizontal);
                self.clear_preview_for_snap(); // retroactively snap preview to clean line
                return (col, start.1);
            } else if dy > dx {
                self.locked_axis = Some(Axis::Vertical);
                self.clear_preview_for_snap();
                return (start.0, row);
            }
            // Tie: keep evaluating on next move, continue freehand for now
        }

        (col, row)
    }

    /// Paint into the **preview layer** for the current tool and mouse position.
    /// On mouseup the preview is committed; on ESC it is discarded.
    ///
    /// Pencil/Eraser: incremental — Bresenham from last position to current.
    /// Line: redraws from draw_start to current on every call so the preview
    ///       always shows exactly the final line, not a smear of all positions.
    pub(crate) fn paint_stroke_to(&mut self, col: usize, row: usize, shift_held: bool) {
        match self.tool {
            Tool::Pencil => {
                let (col, row) = self.resolve_target(col, row, shift_held || self.shift_locked);
                let cells = match self.last_painted_cell {
                    Some((pc, pr)) => bresenham(pc, pr, col, row),
                    None           => vec![(col, row)],
                };

                match self.pencil_mode {
                    PencilMode::Normal => {
                        for (c, r) in cells {
                            if self.blend_mode == BlendMode::Stamp
                                && self.grid.committed[Grid::idx(c, r)] != BLANK
                            {
                                continue;
                            }
                            let ch = self.stamp_char(c, r);
                            self.grid.set_preview(c, r, Some(ch));
                            self.render_cell(c, r);
                        }
                    }
                    PencilMode::Art => {
                        // cells[0] == last_painted_cell when last_painted_cell is Some.
                        // We retroactively correct it with the now-known exit direction,
                        // then assign direction-based chars to the remaining new cells.
                        let prev_was_some = self.last_painted_cell.is_some();
                        for (i, &(c, r)) in cells.iter().enumerate() {
                            if i == 0 && prev_was_some {
                                // Retroactive fix: update last_painted_cell's char using
                                // the direction to the next cell (cells[1]).
                                if cells.len() > 1 {
                                    let (nc, nr) = cells[1];
                                    let ch = art_char((nc as i32) - (c as i32), (nr as i32) - (r as i32));
                                    self.grid.set_preview(c, r, Some(ch));
                                    self.render_cell(c, r);
                                }
                                // cells[0] = last_painted_cell: always skip normal processing.
                                continue;
                            }
                            if self.blend_mode == BlendMode::Stamp
                                && self.grid.committed[Grid::idx(c, r)] != BLANK
                            {
                                continue;
                            }
                            // Direction from previous cell in the batch.
                            let ch = if i == 0 {
                                // First-ever cell of the stroke — no direction yet.
                                '-'
                            } else {
                                let (pc, pr) = cells[i - 1];
                                art_char((c as i32) - (pc as i32), (r as i32) - (pr as i32))
                            };
                            self.grid.set_preview(c, r, Some(ch));
                            self.render_cell(c, r);
                        }
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Eraser => {
                let (col, row) = self.resolve_target(col, row, shift_held || self.shift_locked);
                let cells = match self.last_painted_cell {
                    Some((pc, pr)) => bresenham(pc, pr, col, row),
                    None           => vec![(col, row)],
                };
                for (c, r) in cells {
                    if self.blend_mode == BlendMode::Stamp
                        && self.grid.committed[Grid::idx(c, r)] != BLANK
                    {
                        continue;
                    }
                    let ch = self.stamp_char(c, r); // eraser always returns BLANK
                    self.grid.set_preview(c, r, Some(ch));
                    self.render_cell(c, r);
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Line => {
                // Clear the previous preview and redraw the whole line from
                // draw_start to the current cursor position each mousemove.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let cells = bresenham(sc, sr, col, row);
                    // Overall direction used to characterise the first cell in art mode.
                    let overall_dc = (col as i32) - (sc as i32);
                    let overall_dr = (row as i32) - (sr as i32);
                    for (i, &(c, r)) in cells.iter().enumerate() {
                        // AA mode overrides line mode — use image-matched char per cell.
                        let ch = if self.aa_mode && self.bg_edges.is_some() && !self.active_catalog().is_empty() {
                            self.compute_best_char(c, r)
                        } else {
                            match self.line_mode {
                                LineMode::Character => self.brush_char,
                                LineMode::Art => {
                                    if i == 0 {
                                        // Use the step to the next cell so the start character
                                        // matches the local direction, not the overall slope.
                                        // Falls back to overall direction for single-cell lines.
                                        if cells.len() > 1 {
                                            let (nc, nr) = cells[1];
                                            art_char((nc as i32) - (c as i32), (nr as i32) - (r as i32))
                                        } else {
                                            art_char(overall_dc, overall_dr)
                                        }
                                    } else {
                                        let (pc, pr) = cells[i - 1];
                                        art_char((c as i32) - (pc as i32), (r as i32) - (pr as i32))
                                    }
                                }
                            }
                        };
                        if self.blend_mode == BlendMode::Stamp
                            && self.grid.committed[Grid::idx(c, r)] != BLANK
                        {
                            continue;
                        }
                        self.grid.set_preview(c, r, Some(ch));
                        self.render_cell(c, r);
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Rect => {
                // Clear the previous preview and redraw the rectangle outline each mousemove.
                //
                // Normal mode: draw_start and current cell are opposite corners.
                // Shift mode:  draw_start is the center; current cell defines the half-extents.
                //              Bounding box is symmetric and may extend outside the canvas —
                //              per-cell bounds check silently skips any off-canvas cells.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let (bx0, by0, bx1, by1): (i64, i64, i64, i64) = if shift_held || self.shift_locked {
                        let cx = sc as i64;
                        let cy = sr as i64;
                        let dx = (col as i64 - cx).abs();
                        let dy = (row as i64 - cy).abs();
                        (cx - dx, cy - dy, cx + dx, cy + dy)
                    } else {
                        (sc.min(col) as i64, sr.min(row) as i64,
                         sc.max(col) as i64, sr.max(row) as i64)
                    };
                    let in_bounds = |x: i64, y: i64| -> Option<(usize, usize)> {
                        if x >= 0 && y >= 0 && (x as usize) < COLS && (y as usize) < ROWS {
                            Some((x as usize, y as usize))
                        } else {
                            None
                        }
                    };
                    // Top and bottom edges, then left and right edges.
                    // Corners are visited twice but set_preview is idempotent.
                    let mut cells: Vec<(usize, usize)> = Vec::new();
                    for x in bx0..=bx1 {
                        if let Some(p) = in_bounds(x, by0) { cells.push(p); }
                        if let Some(p) = in_bounds(x, by1) { cells.push(p); }
                    }
                    for y in by0..=by1 {
                        if let Some(p) = in_bounds(bx0, y) { cells.push(p); }
                        if let Some(p) = in_bounds(bx1, y) { cells.push(p); }
                    }
                    for (c, r) in cells {
                        if self.blend_mode == BlendMode::Stamp
                            && self.grid.committed[Grid::idx(c, r)] != BLANK
                        {
                            continue;
                        }
                        let ch = self.stamp_char(c, r);
                        self.grid.set_preview(c, r, Some(ch));
                        self.render_cell(c, r);
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::RectFill => {
                // Same bounding-box logic as Rect — normal is corner-to-corner,
                // Shift is center-origin. Per-cell bounds check clips off-canvas cells.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let (bx0, by0, bx1, by1): (i64, i64, i64, i64) = if shift_held || self.shift_locked {
                        let cx = sc as i64;
                        let cy = sr as i64;
                        let dx = (col as i64 - cx).abs();
                        let dy = (row as i64 - cy).abs();
                        (cx - dx, cy - dy, cx + dx, cy + dy)
                    } else {
                        (sc.min(col) as i64, sr.min(row) as i64,
                         sc.max(col) as i64, sr.max(row) as i64)
                    };
                    for y in by0..=by1 {
                        for x in bx0..=bx1 {
                            if x < 0 || y < 0 || (x as usize) >= COLS || (y as usize) >= ROWS {
                                continue;
                            }
                            let (c, r) = (x as usize, y as usize);
                            if self.blend_mode == BlendMode::Stamp
                                && self.grid.committed[Grid::idx(c, r)] != BLANK
                            {
                                continue;
                            }
                            let ch = self.stamp_char(c, r);
                            self.grid.set_preview(c, r, Some(ch));
                            self.render_cell(c, r);
                        }
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Oval => {
                // Clear previous preview, then redraw the ellipse each mousemove.
                //
                // Normal mode: draw_start and current cell are opposite bounding-box corners.
                // Shift mode:  draw_start is the center; current cell defines the half-extents.
                //              The bounding box is expanded symmetrically, so it can extend
                //              outside the canvas — ellipse_cells clips those cells silently.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let (bx0, by0, bx1, by1) = if shift_held || self.shift_locked {
                        let cx = sc as i64;
                        let cy = sr as i64;
                        let dx = (col as i64 - cx).abs();
                        let dy = (row as i64 - cy).abs();
                        (cx - dx, cy - dy, cx + dx, cy + dy)
                    } else {
                        (sc as i64, sr as i64, col as i64, row as i64)
                    };
                    for (c, r) in ellipse_cells(bx0, by0, bx1, by1) {
                        if self.blend_mode == BlendMode::Stamp
                            && self.grid.committed[Grid::idx(c, r)] != BLANK
                        {
                            continue;
                        }
                        let ch = self.stamp_char(c, r);
                        self.grid.set_preview(c, r, Some(ch));
                        self.render_cell(c, r);
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::OvalFill => {
                // Same bounding-box logic as Oval — normal is corner-to-corner,
                // Shift is center-origin. Uses filled_ellipse_cells so interior
                // and outline share the same integer geometry.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let (bx0, by0, bx1, by1) = if shift_held || self.shift_locked {
                        let cx = sc as i64;
                        let cy = sr as i64;
                        let dx = (col as i64 - cx).abs();
                        let dy = (row as i64 - cy).abs();
                        (cx - dx, cy - dy, cx + dx, cy + dy)
                    } else {
                        (sc as i64, sr as i64, col as i64, row as i64)
                    };
                    for (c, r) in filled_ellipse_cells(bx0, by0, bx1, by1) {
                        if self.blend_mode == BlendMode::Stamp
                            && self.grid.committed[Grid::idx(c, r)] != BLANK
                        {
                            continue;
                        }
                        let ch = self.stamp_char(c, r);
                        self.grid.set_preview(c, r, Some(ch));
                        self.render_cell(c, r);
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Select => {
                // Selection doesn't touch the grid — just update the highlighted region
                // from draw_start to the current cell on every mousemove.
                if let Some((sc, sr)) = self.draw_start {
                    self.apply_selection(sc, sr, col, row);
                }
                self.last_painted_cell = Some((col, row));
            }

            Tool::Fill => {
                // Flood fill is a one-shot operation — act only on the first call
                // (mousedown). Subsequent mousemove calls are skipped by checking
                // whether last_painted_cell is already set.
                if self.last_painted_cell.is_some() { return; }
                let target_char = self.grid.committed[Grid::idx(col, row)];
                // No-op when the cell already contains the brush character; filling
                // would either do nothing or loop forever.
                // In AA mode always fill — there's no single reference char to compare.
                if self.aa_mode || target_char != self.brush_char {
                    let diagonals = self.fill_mode == FillMode::Flood8;
                    for (c, r) in flood_fill_cells(col, row, target_char, &self.grid.committed, diagonals) {
                        if self.blend_mode == BlendMode::Stamp
                            && self.grid.committed[Grid::idx(c, r)] != BLANK
                        {
                            continue;
                        }
                        let ch = self.stamp_char(c, r);
                        self.grid.set_preview(c, r, Some(ch));
                        self.render_cell(c, r);
                    }
                }
                self.last_painted_cell = Some((col, row));
            }

            _ => {} // NIY tools are silent no-ops for now
        }
    }

    /// Finalise the in-progress stroke: commit all preview cells to the backing
    /// store. Called on mouseup. Reuses for all tools once they are implemented.
    pub(crate) fn commit_stroke(&mut self) {
        let dirty = self.grid.commit_preview();
        for (c, r) in dirty {
            self.render_cell(c, r); // removes preview tint, applies final appearance
        }
        self.is_drawing = false;
        self.draw_start  = None;
        self.locked_axis = None;
        self.last_painted_cell = None;
    }

    /// Cancel the in-progress stroke: discard preview without committing, and
    /// remove the undo snapshot that was pushed at mousedown (cancelled strokes
    /// must not appear in undo history). Called on ESC.
    pub(crate) fn abort_stroke(&mut self) {
        let dirty = self.grid.abort_preview();
        for (c, r) in dirty {
            self.render_cell(c, r); // removes preview tint, restores committed char
        }
        if self.tool == Tool::Select {
            self.clear_selection(); // ESC cancels an in-progress or committed selection
        } else {
            self.undo_stack.pop(); // discard the pre-stroke snapshot — nothing was committed
        }
        self.is_drawing = false;
        self.draw_start  = None;
        self.locked_axis = None;
        self.last_painted_cell = None;
    }

    // ── Clear ────────────────────────────────────────────────────────────────

    /// Erase content and push an undo snapshot.
    /// If a selection is active, erases only the selected region then deselects.
    /// Otherwise erases the entire canvas.
    pub(crate) fn clear_canvas(&mut self) {
        self.commit_text_session(); // commit any open text entry before clearing
        self.push_undo_snapshot();
        if let Some((c0, r0, c1, r1)) = self.selection {
            for r in r0..=r1 {
                for c in c0..=c1 {
                    self.grid.set_committed(c, r, BLANK);
                }
            }
            self.clear_selection();
        } else {
            for cell in self.grid.committed.iter_mut() {
                *cell = BLANK;
            }
        }
        self.render_all();
    }

    // ── Undo / redo ──────────────────────────────────────────────────────────

    /// Snapshot committed state onto the undo stack before a destructive commit.
    /// Must be called before writing the final mouseup state, not after.
    pub(crate) fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.grid.committed.clone());
        self.redo_stack.clear(); // new action invalidates the redo branch
    }

    /// Restore the previous committed state.
    pub(crate) fn undo(&mut self) {
        self.commit_text_session(); // commit any open text entry before undoing
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.grid.committed.clone());
            self.grid.committed = prev;
            self.grid.preview = vec![None; COLS * ROWS]; // discard any in-progress op
            self.is_drawing = false;
            self.last_painted_cell = None;
            self.render_all();
        }
    }

    /// Reapply a previously undone state.
    pub(crate) fn redo(&mut self) {
        self.commit_text_session(); // commit any open text entry before redoing
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.grid.committed.clone());
            self.grid.committed = next;
            self.grid.preview = vec![None; COLS * ROWS];
            self.is_drawing = false;
            self.last_painted_cell = None;
            self.render_all();
        }
    }

    // ── Selection ────────────────────────────────────────────────────────────

    /// Remove the `.selected` CSS class from all currently selected cells and
    /// clear the selection state. Safe to call when there is no active selection.
    pub(crate) fn clear_selection(&mut self) {
        if let Some((c0, r0, c1, r1)) = self.selection.take() {
            for r in r0..=r1 {
                for c in c0..=c1 {
                    self.cell_els[Grid::idx(c, r)]
                        .class_list()
                        .remove_1("selected")
                        .unwrap();
                }
            }
        }
    }

    /// Highlight a rectangular region by applying `.selected` to every cell in it.
    /// Clears any previous selection first. Normalizes corner order automatically.
    pub(crate) fn apply_selection(&mut self, sc: usize, sr: usize, ec: usize, er: usize) {
        self.clear_selection();
        let c0 = sc.min(ec);
        let c1 = sc.max(ec);
        let r0 = sr.min(er);
        let r1 = sr.max(er);
        for r in r0..=r1 {
            for c in c0..=c1 {
                self.cell_els[Grid::idx(c, r)]
                    .class_list()
                    .add_1("selected")
                    .unwrap();
            }
        }
        self.selection = Some((c0, r0, c1, r1));
    }

    // ── Clipboard ────────────────────────────────────────────────────────────

    /// Render the committed canvas as plain text suitable for the clipboard.
    /// Each row is padded to COLS characters so trailing spaces are preserved,
    /// and rows are joined with newlines. Preview layer is intentionally ignored
    /// — copy always reflects committed state, never an in-progress stroke.
    pub(crate) fn canvas_text(&self) -> String {
        let mut out = String::with_capacity((COLS + 1) * ROWS);
        for r in 0..ROWS {
            for c in 0..COLS {
                out.push(self.grid.committed[Grid::idx(c, r)]);
            }
            if r < ROWS - 1 {
                out.push('\n');
            }
        }
        out
    }

    /// Render only the selected region as plain text. Rows are padded to the
    /// selection width (not full COLS). Returns None if there is no selection.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let (c0, r0, c1, r1) = self.selection?;
        let w = c1 - c0 + 1;
        let h = r1 - r0 + 1;
        let mut out = String::with_capacity((w + 1) * h);
        for r in r0..=r1 {
            for c in c0..=c1 {
                out.push(self.grid.committed[Grid::idx(c, r)]);
            }
            if r < r1 {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Render the full canvas as a styled HTML snippet suitable for rich clipboard copy.
    /// Wraps the plain-text content in a <pre> with inline CSS matching the current theme,
    /// so pasting into Notion, Docs, Slack, etc. preserves monospace font and colors.
    pub(crate) fn canvas_html(&self) -> String {
        html_wrap(self.dark_mode, &self.canvas_text())
    }

    /// Render only the selected region as styled HTML. Returns None if no selection.
    pub(crate) fn selected_html(&self) -> Option<String> {
        Some(html_wrap(self.dark_mode, &self.selected_text()?))
    }

    // ── Touch / zoom ─────────────────────────────────────────────────────────

    /// Regenerate the full inline style string for #grid from current state and apply it.
    /// Called by set_font_size and apply_bg_image so they never overwrite each other.
    fn sync_grid_style(&self) {
        let mut style = format!("font-size: {}px", self.font_size);
        if !self.bg_hidden {
            if let Some(ref url) = self.bg_image_url {
                let overlay = if self.dark_mode {
                    "rgba(13,13,13,0.5)"
                } else {
                    "rgba(255,255,255,0.5)"
                };
                style.push_str(&format!(
                    "; background-image: linear-gradient({ov},{ov}), url({url}); \
                     background-size: {w}px {h}px; \
                     background-position: {x}px {y}px; \
                     background-repeat: no-repeat",
                    ov = overlay,
                    url = url,
                    w  = self.bg_disp_w.round(),
                    h  = self.bg_disp_h.round(),
                    x  = self.bg_pos_x.round(),
                    y  = self.bg_pos_y.round(),
                ));
            }
        }
        let _ = self.grid_el.set_attribute("style", &style);
    }

    /// Recompute the cell-to-processed-image transform from the current layout state.
    ///
    /// Stores (origin_x, origin_y, proc_cell_w, proc_cell_h) in bg_visible_rect, where
    /// origin is the processed-image coordinate of the top-left of cell (0,0) and
    /// proc_cell_w/h is the processed-image size of one character cell.
    ///
    /// Called whenever bg_pos/disp or grid layout changes — never per painted cell.
    fn refresh_visible_rect(&mut self) {
        let proc_w = self.bg_luma_width  as f64;
        let proc_h = self.bg_luma_height as f64;
        if proc_w == 0.0 || proc_h == 0.0 || self.bg_disp_w == 0.0 || self.bg_disp_h == 0.0 {
            self.bg_visible_rect = (0.0, 0.0, 0.0, 0.0);
            return;
        }
        let grid_w = self.grid_el.client_width()  as f64;
        let grid_h = self.grid_el.client_height() as f64;
        if grid_w == 0.0 || grid_h == 0.0 {
            self.bg_visible_rect = (0.0, 0.0, 0.0, 0.0);
            return;
        }
        // Conversion: 1 CSS grid pixel = (proc_w / bg_disp_w) processed pixels.
        let px_per_css_x = proc_w / self.bg_disp_w;
        let px_per_css_y = proc_h / self.bg_disp_h;
        // Processed-image coordinate of the top-left of cell (0, 0).
        // bg_pos_x is where the image top-left sits in grid CSS pixels; invert it.
        let origin_x   = -self.bg_pos_x * px_per_css_x;
        let origin_y   = -self.bg_pos_y * px_per_css_y;
        // Processed-image size per character cell.
        let proc_cell_w = (grid_w / COLS as f64) * px_per_css_x;
        let proc_cell_h = (grid_h / ROWS as f64) * px_per_css_y;
        self.bg_visible_rect = (origin_x, origin_y, proc_cell_w, proc_cell_h);
    }

    /// Set the grid font size, clamped to 8–48 px, and apply it to the DOM.
    /// Called on every pinch-zoom touchmove frame.
    /// Also scales the background image proportionally: both grid dimensions are
    /// proportional to font_size (1ch and 1.25em both scale with it), so multiplying
    /// all four bg layout values by new/old keeps the image visually stationary.
    pub(crate) fn set_font_size(&mut self, size: f64) {
        let old = self.font_size;
        self.font_size = size.max(8.0).min(48.0);
        if self.bg_image_url.is_some() && old != 0.0 {
            let s = self.font_size / old;
            self.bg_disp_w *= s;
            self.bg_disp_h *= s;
            self.bg_pos_x  *= s;
            self.bg_pos_y  *= s;
            // Keep the ESC-cancel snapshot in sync so cancel_bg_move restores
            // the correct position at the current grid scale.
            if let Some((px, py, dw, dh)) = self.bg_move_saved {
                self.bg_move_saved = Some((px * s, py * s, dw * s, dh * s));
            }
        }
        self.sync_grid_style();
        self.refresh_visible_rect();
    }

    /// Store a new background image URL (revoking the old one) and update the grid style.
    /// Display position and size come from bg_pos_x/y and bg_disp_w/h — set those
    /// before calling this (e.g. via init_bg_layout for a fresh drop).
    pub(crate) fn apply_bg_image(&mut self, url: String) {
        if let Some(ref old) = self.bg_image_url {
            let _ = web_sys::Url::revoke_object_url(old);
        }
        self.bg_image_url = Some(url);
        self.sync_grid_style();
    }

    /// Full AA image pipeline: extract pixels, convert to luminance, stretch,
    /// render the processed grayscale back to the canvas, and set it as the
    /// #grid background. The stretched luma array is stored for the AA step.
    ///
    /// `blob_url` is the `createObjectURL` URL for the original colour image.
    /// It is revoked immediately after pixel extraction — the background CSS
    /// uses a grayscale data URL derived from the processed pixels instead.
    ///
    /// Blob URLs are same-origin, so `getImageData` is not blocked by the
    /// canvas taint check.
    pub(crate) fn process_bg_image(&mut self, img_el: &web_sys::HtmlImageElement, blob_url: &str) {
        let nw = img_el.natural_width();
        let nh = img_el.natural_height();
        if nw == 0 || nh == 0 { return; }

        let document = match web_sys::window().and_then(|w| w.document()) {
            Some(d) => d,
            None    => return,
        };

        // Off-screen canvas — never inserted into the DOM; pixel scratch buffer.
        let canvas = match document
            .create_element("canvas")
            .ok()
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        {
            Some(c) => c,
            None    => return,
        };
        // Scale to fixed processing height; width proportional to preserve aspect ratio.
        let scale  = asciiart::PROCESSING_HEIGHT as f64 / nh as f64;
        let proc_w = (nw as f64 * scale).round() as u32;
        let proc_h = asciiart::PROCESSING_HEIGHT;

        canvas.set_width(proc_w);
        canvas.set_height(proc_h);

        let ctx = match canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|obj| obj.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
        {
            Some(c) => c,
            None    => return,
        };

        if ctx.draw_image_with_html_image_element_and_dw_and_dh(
            img_el, 0.0, 0.0, proc_w as f64, proc_h as f64,
        ).is_err() {
            return;
        }

        let image_data = match ctx.get_image_data(0.0, 0.0, proc_w as f64, proc_h as f64) {
            Ok(d)  => d,
            Err(_) => return,
        };

        // Original colour image no longer needed — revoke its blob URL now.
        let _ = web_sys::Url::revoke_object_url(blob_url);

        // ImageData::data() returns Clamped<Vec<u8>> — unwrap the newtype.
        let rgba = image_data.data().0;
        let mut luma = asciiart::to_luminance(&rgba);
        asciiart::stretch_luminance(&mut luma);

        // Store the stretched-but-unadjusted luma so contrast can be re-applied
        // later without reloading the image. rebuild_from_params derives bg_luma
        // and bg_edges from this raw copy whenever a parameter changes.
        self.bg_luma_raw        = Some(luma);
        self.bg_luma_width      = proc_w;
        self.bg_luma_height     = proc_h;
        self.bg_luma_raw_width  = proc_w;  // canonical raw dims — never changed after load
        self.bg_luma_raw_height = proc_h;
        self.init_bg_layout();   // cover+center before first render
        self.rebuild_from_params(); // sets bg_luma, bg_edges, calls rebuild_background
        // Unlock the AA brush and eye toggle; reset visibility to shown.
        self.enable_aa_btn();
        self.enable_bg_eye(); // must run before auto-BgMove (it calls accept_bg_move if already active)
        // Always auto-enter BgMove on image drop so the user is immediately invited
        // to frame the image, regardless of whether this is the first drop or not.
        self.enter_bg_move();
        self.restore_tool_active_class(Tool::BgMove);
    }

    /// Re-render the stored luma data as a background image according to the current
    /// bg_outline_mode. Called after a new image is processed and when the mode cycles.
    /// No-ops gracefully when no image has been dropped yet.
    pub(crate) fn rebuild_background(&mut self) {
        // Compute pixel data while bg_luma is immutably borrowed; release borrow
        // before calling apply_bg_image which needs a mutable self.
        let (gray_rgba, proc_w, proc_h) = {
            let luma = match self.bg_luma.as_ref() {
                Some(l) => l,
                None    => return,
            };
            let w = self.bg_luma_width;
            let h = self.bg_luma_height;
            if w == 0 || h == 0 { return; }

            // For edge modes: use stored bg_edges rather than re-running Sobel.
            // bg_edges and bg_luma are separate fields; Rust NLL allows both borrows.
            let pixels = match self.bg_outline_mode {
                BgOutlineMode::Original => {
                    // Straight grayscale: R=G=B=L, A=255.
                    let mut v = vec![0u8; luma.len() * 4];
                    for (i, &l) in luma.iter().enumerate() {
                        v[i*4] = l; v[i*4+1] = l; v[i*4+2] = l; v[i*4+3] = 255;
                    }
                    v
                }
                BgOutlineMode::WhiteOnBlack => {
                    // Use cached edges; recompute only if missing (defensive fallback).
                    let edges: Vec<u8> = match self.bg_edges.as_ref() {
                        Some(e) => e.clone(),
                        None    => { let mut e = asciiart::sobel_edges(luma, w, h); asciiart::enhance_edges(&mut e); e }
                    };
                    let mut v = vec![0u8; edges.len() * 4];
                    for (i, &e) in edges.iter().enumerate() {
                        v[i*4] = e; v[i*4+1] = e; v[i*4+2] = e; v[i*4+3] = 255;
                    }
                    v
                }
                BgOutlineMode::BlackOnWhite => {
                    // Inverted cached edges.
                    let edges: Vec<u8> = match self.bg_edges.as_ref() {
                        Some(e) => e.clone(),
                        None    => { let mut e = asciiart::sobel_edges(luma, w, h); asciiart::enhance_edges(&mut e); e }
                    };
                    let mut v = vec![0u8; edges.len() * 4];
                    for (i, &e) in edges.iter().enumerate() {
                        let inv = 255u8.saturating_sub(e);
                        v[i*4] = inv; v[i*4+1] = inv; v[i*4+2] = inv; v[i*4+3] = 255;
                    }
                    v
                }
            };
            (pixels, w, h)
        };

        let document = match web_sys::window().and_then(|w| w.document()) {
            Some(d) => d,
            None    => return,
        };

        // Off-screen canvas — write processed pixels and export as a data URL.
        let canvas = match document
            .create_element("canvas").ok()
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        {
            Some(c) => c,
            None    => return,
        };
        canvas.set_width(proc_w);
        canvas.set_height(proc_h);

        let ctx = match canvas
            .get_context("2d").ok().flatten()
            .and_then(|obj| obj.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
        {
            Some(c) => c,
            None    => return,
        };

        let image_data = match web_sys::ImageData::new_with_u8_clamped_array_and_sh(
            wasm_bindgen::Clamped(&gray_rgba[..]), proc_w, proc_h,
        ) {
            Ok(d)  => d,
            Err(_) => return,
        };
        if ctx.put_image_data(&image_data, 0.0, 0.0).is_err() { return; }
        // JPEG encodes 5-10x faster than PNG for a same-size image — acceptable
        // quality loss for a tracing reference that is never saved or exported.
        let data_url = match canvas.to_data_url_with_type("image/jpeg") {
            Ok(u)  => u,
            Err(_) => return,
        };

        self.apply_bg_image(data_url);
        self.refresh_visible_rect();
    }

    /// Initialise bg_disp_w/h and bg_pos_x/y to the cover+center layout.
    /// Called on image drop before the first render. Does not call sync_grid_style.
    fn init_bg_layout(&mut self) {
        let proc_w = self.bg_luma_width  as f64;
        let proc_h = self.bg_luma_height as f64;
        let grid_w = self.grid_el.client_width()  as f64;
        let grid_h = self.grid_el.client_height() as f64;
        if proc_w == 0.0 || proc_h == 0.0 || grid_w == 0.0 || grid_h == 0.0 { return; }
        let s = (grid_w / proc_w).max(grid_h / proc_h).min(2.0);
        self.bg_disp_w = proc_w * s;
        self.bg_disp_h = proc_h * s;
        self.bg_pos_x  = (grid_w - self.bg_disp_w) / 2.0;
        self.bg_pos_y  = (grid_h - self.bg_disp_h) / 2.0;
    }

    // ── BgMove tool ──────────────────────────────────────────────────────────

    /// Activate BgMove mode: snapshot the current tool and layout for later restore.
    /// The wiring caller is responsible for moving the `.active` class to the BgMove button.
    pub(crate) fn enter_bg_move(&mut self) {
        self.prev_tool    = Some(self.tool);
        self.bg_move_saved = Some((self.bg_pos_x, self.bg_pos_y, self.bg_disp_w, self.bg_disp_h));
        self.tool = Tool::BgMove;
        self.show_image_controls(); // always show fresh (resets dismiss checkbox)
        let _ = self.bg_move_btn_el.class_list().add_1("blinking");
    }

    /// Accept current position and exit BgMove mode, restoring the previous tool.
    /// Called on second-click, Enter, or when a new image is dropped.
    pub(crate) fn accept_bg_move(&mut self) {
        // Reprocess edges at current scale before exiting so display and matching align.
        // Bumps debounce gen as a side effect — any pending debounce timer will no-op.
        self.reprocess_edges_for_scale();
        let prev = self.prev_tool.take().unwrap_or(Tool::Pencil);
        self.bg_move_saved = None;
        self.tool = prev;
        self.restore_tool_active_class(prev);
        self.hide_image_controls();
        let _ = self.bg_move_btn_el.class_list().remove_1("blinking");
    }

    /// Increment the zoom debounce generation counter and return the new value.
    /// The wiring layer captures this value in the pending timeout closure; if the
    /// generation has advanced by the time the timer fires, the closure no-ops.
    pub(crate) fn bump_zoom_debounce(&mut self) -> u32 {
        self.zoom_debounce_gen = self.zoom_debounce_gen.wrapping_add(1);
        self.zoom_debounce_gen
    }

    /// Recompute bg_edges from bg_luma_raw using a pre-blur radius matched to the
    /// current zoom level. At zoom-out (large cell_w) a wide blur suppresses fine edges
    /// so only cell-scale structure survives Sobel; at zoom-in (small cell_w) a narrow
    /// blur lets finer edges through. bg_luma, bg_pos/disp, and all CSS are untouched.
    pub(crate) fn reprocess_edges_for_scale(&mut self) {
        let raw = match self.bg_luma_raw.as_ref() {
            Some(r) => r,
            None    => return,
        };
        let w = self.bg_luma_raw_width;
        let h = self.bg_luma_raw_height;
        if w == 0 || h == 0 { return; }

        // Feature size of a 12pt font stroke ≈ 1pt ≈ 1/300 of canvas height.
        // In processing pixels: feature = cell_h / 12.5 where cell_h = pixels per row.
        // Pre-blur at this radius suppresses sub-feature noise before Sobel.
        let cell_h = self.bg_visible_rect.3;
        let radius  = ((cell_h / 12.5).round() as usize).max(1);

        let blurred = asciiart::scale_blur(raw, w, h, radius);
        let mut edges = asciiart::sobel_edges(&blurred, w, h);
        asciiart::enhance_edges(&mut edges);

        // Bump debounce gen so any pending timer fires as a no-op.
        self.zoom_debounce_gen = self.zoom_debounce_gen.wrapping_add(1);

        self.bg_edges = Some(edges);
        self.rebuild_background();
    }

    /// Remove the BgMove visual state (blinking button, image-controls strip) without
    /// touching tool state or background position. Called when another tool is selected
    /// via the toolbar, bypassing the normal accept/cancel paths.
    pub(crate) fn leave_bg_move_ui(&mut self) {
        self.hide_image_controls();
        let _ = self.bg_move_btn_el.class_list().remove_1("blinking");
    }

    /// Cancel BgMove: revert position/scale to the snapshot taken at entry, then exit.
    /// Called on ESC.
    pub(crate) fn cancel_bg_move(&mut self) {
        if let Some((px, py, dw, dh)) = self.bg_move_saved.take() {
            self.bg_pos_x  = px;
            self.bg_pos_y  = py;
            self.bg_disp_w = dw;
            self.bg_disp_h = dh;
            self.sync_grid_style();
            self.refresh_visible_rect();
        }
        let prev = self.prev_tool.take().unwrap_or(Tool::Pencil);
        self.tool = prev;
        self.restore_tool_active_class(prev);
        self.hide_image_controls();
        let _ = self.bg_move_btn_el.class_list().remove_1("blinking");
    }

    /// Move the `.active` CSS class to the `[data-tool]` button matching `tool`.
    /// Used by accept/cancel to highlight the restored tool without a wiring round-trip.
    fn restore_tool_active_class(&self, tool: Tool) {
        let document = match web_sys::window().and_then(|w| w.document()) {
            Some(d) => d,
            None    => return,
        };
        let attr = tool.data_attr();
        let all  = match document.query_selector_all("[data-tool]") {
            Ok(list) => list,
            Err(_)   => return,
        };
        for i in 0..all.length() {
            if let Some(node) = all.item(i) {
                if let Ok(el) = node.dyn_into::<Element>() {
                    let _ = el.class_list().remove_1("active");
                    if el.get_attribute("data-tool").as_deref() == Some(attr) {
                        let _ = el.class_list().add_1("active");
                    }
                }
            }
        }
    }

    /// Begin a background image drag. Records client coordinates and current bg_pos.
    pub(crate) fn start_bg_drag(&mut self, client_x: f64, client_y: f64) {
        self.bg_drag_start = Some((client_x, client_y, self.bg_pos_x, self.bg_pos_y));
    }

    /// Update background position during drag. No-ops if no drag is in progress.
    pub(crate) fn update_bg_drag(&mut self, client_x: f64, client_y: f64) {
        let (sx, sy, px, py) = match self.bg_drag_start {
            Some(s) => s,
            None    => return,
        };
        self.bg_pos_x = px + (client_x - sx);
        self.bg_pos_y = py + (client_y - sy);
        self.sync_grid_style();
        self.refresh_visible_rect();
    }

    /// End a background image drag.
    pub(crate) fn end_bg_drag(&mut self) {
        self.bg_drag_start = None;
    }

    /// Apply combined zoom + pan from a pinch gesture (mobile BgMove).
    ///
    /// `ratio` = cur_finger_dist / pinch_start_dist.
    /// `pivot_grid_x/y` = current pinch midpoint in CSS grid-element pixels.
    /// `pan_dx/dy` = midpoint delta since last touchmove frame (CSS pixels).
    ///
    /// Scale is derived from `pinch_start_bg_disp_w × ratio`, clamped to
    /// [½ contain, 2× natural]. Zoom pivots around the midpoint, then the pan
    /// delta is added so the content under the fingers tracks naturally.
    pub(crate) fn update_bg_from_pinch(
        &mut self,
        ratio: f64,
        pivot_grid_x: f64,
        pivot_grid_y: f64,
        pan_dx: f64,
        pan_dy: f64,
    ) {
        if self.bg_luma_raw_width == 0 { return; }
        // Use raw dims for zoom limits so limits remain stable across reprocess cycles.
        let raw_w  = self.bg_luma_raw_width  as f64;
        let raw_h  = self.bg_luma_raw_height as f64;
        let grid_w = self.grid_el.client_width()  as f64;
        let grid_h = self.grid_el.client_height() as f64;

        let contain_scale = (grid_w / raw_w).min(grid_h / raw_h);
        let new_disp_w    = (self.pinch_start_bg_disp_w * ratio)
            .clamp(raw_w * contain_scale * 0.5, raw_w * 2.0);
        let actual_scale  = new_disp_w / self.bg_disp_w;
        let new_disp_h    = self.bg_disp_h * actual_scale;

        // Pivot zoom: keep the midpoint content stationary.
        self.bg_pos_x  = pivot_grid_x - (pivot_grid_x - self.bg_pos_x) * actual_scale;
        self.bg_pos_y  = pivot_grid_y - (pivot_grid_y - self.bg_pos_y) * actual_scale;
        // Pan: shift with midpoint movement.
        self.bg_pos_x += pan_dx;
        self.bg_pos_y += pan_dy;

        self.bg_disp_w = new_disp_w;
        self.bg_disp_h = new_disp_h;

        self.sync_grid_style();
        self.refresh_visible_rect();
    }

    /// Zoom the background image around a CSS-grid-pixel pivot point.
    /// `delta` is signed scroll distance — positive zooms out, negative zooms in.
    /// Scale clamped to [half-contain, 2× natural size].
    pub(crate) fn zoom_bg_image(&mut self, delta: f64, pivot_x: f64, pivot_y: f64) {
        if self.bg_luma_raw_width == 0 { return; }
        // Use raw dims for zoom limits so limits remain stable across reprocess cycles.
        let raw_w  = self.bg_luma_raw_width  as f64;
        let raw_h  = self.bg_luma_raw_height as f64;
        let grid_w = self.grid_el.client_width()  as f64;
        let grid_h = self.grid_el.client_height() as f64;

        let factor = if delta > 0.0 { 0.9 } else { 1.0 / 0.9 };

        // Max = 2× natural; min = half the contain scale.
        let contain_scale = (grid_w / raw_w).min(grid_h / raw_h);
        let new_disp_w    = (self.bg_disp_w * factor)
            .clamp(raw_w * contain_scale * 0.5, raw_w * 2.0);
        let actual_scale  = new_disp_w / self.bg_disp_w;
        let new_disp_h    = self.bg_disp_h * actual_scale;

        // Keep the content under the cursor stationary as scale changes.
        self.bg_pos_x  = pivot_x - (pivot_x - self.bg_pos_x) * actual_scale;
        self.bg_pos_y  = pivot_y - (pivot_y - self.bg_pos_y) * actual_scale;
        self.bg_disp_w = new_disp_w;
        self.bg_disp_h = new_disp_h;

        self.sync_grid_style();
        self.refresh_visible_rect();
    }

    // ── Background visibility toggle ─────────────────────────────────────────

    /// Toggle background visibility. Highlighted (active) = visible; un-highlighted = hidden.
    /// Image data and edge map are preserved either way — AA brush is unaffected.
    pub(crate) fn toggle_bg_visibility(&mut self) {
        self.bg_hidden = !self.bg_hidden;
        self.sync_grid_style();
        if self.bg_hidden {
            let _ = self.bg_eye_el.class_list().remove_1("active");
        } else {
            let _ = self.bg_eye_el.class_list().add_1("active");
        }
    }

    /// Enable the eye button and show the background. Called when a new image is
    /// successfully processed — always resets to visible so the user sees the new image.
    /// Also exits BgMove if active (the new image resets position via init_bg_layout,
    /// so accept_bg_move just restores prev_tool without touching position).
    pub(crate) fn enable_bg_eye(&mut self) {
        if self.tool == Tool::BgMove {
            self.accept_bg_move();
        }
        self.bg_hidden = false;
        let _ = self.bg_eye_el.class_list().remove_1("disabled");
        let _ = self.bg_eye_el.class_list().add_1("active");
        self.sync_grid_style();
    }

    // ── Help mode ────────────────────────────────────────────────────────────

    /// Toggle help mode on/off. Returns the new state so the caller can
    /// update button and overlay visibility without a second borrow.
    pub(crate) fn toggle_help_mode(&mut self) -> bool {
        self.help_mode = !self.help_mode;
        self.help_mode
    }

    // ── Image controls strip ────────────────────────────────────────────────

    /// Show the image controls strip and reset the hide checkbox to unchecked.
    /// Called each time BgMove is entered so the strip always appears fresh.
    pub(crate) fn show_image_controls(&self) {
        let _ = self.image_controls_el.set_attribute("style", "display: flex");
        self.image_controls_hide_el.set_checked(false);
    }

    /// Hide the image controls strip. Called on BgMove exit or when the hide checkbox fires.
    pub(crate) fn hide_image_controls(&self) {
        let _ = self.image_controls_el.set_attribute("style", "display: none");
    }

    /// Apply current contrast_index to bg_luma_raw, recompute edges, and rebuild the background.
    /// No-ops when no image has been loaded yet.
    pub(crate) fn rebuild_from_params(&mut self) {
        let w = self.bg_luma_width;
        let h = self.bg_luma_height;
        if w == 0 || h == 0 { return; }
        // USM (texture/pop) sidelined — pass raw luma directly to Sobel.
        // apply_texture / apply_pop are preserved in asciiart.rs for future use.
        let luma = match self.bg_luma_raw.as_ref() {
            Some(raw) => raw.clone(),
            None => return,
        };
        let mut edges = asciiart::sobel_edges(&luma, w, h);
        asciiart::enhance_edges(&mut edges);
        self.bg_luma  = Some(luma);
        self.bg_edges = Some(edges);
        self.rebuild_background();
    }

    /// Add delta to texture_amount (clamped to [0, MAX_TEXTURE]) and rebuild.
    /// No-ops silently if already at the limit in that direction.
    #[allow(dead_code)]
    pub(crate) fn adjust_texture(&mut self, delta: i32) {
        let new_val = (self.texture_amount as i32 + delta).clamp(0, MAX_TEXTURE as i32) as u32;
        if new_val == self.texture_amount { return; }
        self.texture_amount = new_val;
        self.sync_texture_buttons();
        self.rebuild_from_params();
    }

    /// Reset texture sharpening to zero and rebuild.
    #[allow(dead_code)]
    pub(crate) fn reset_texture(&mut self) {
        if self.texture_amount == 0 { return; }
        self.texture_amount = 0;
        self.sync_texture_buttons();
        self.rebuild_from_params();
    }

    /// Sync disabled class on the ◀/▶ texture buttons to reflect the current amount.
    /// No-op: buttons removed from UI; preserved for future reinstatement.
    #[allow(dead_code)]
    fn sync_texture_buttons(&self) {}

    /// Add delta to pop_amount (clamped to [0, MAX_POP]) and rebuild.
    #[allow(dead_code)]
    pub(crate) fn adjust_pop(&mut self, delta: i32) {
        let new_val = (self.pop_amount as i32 + delta).clamp(0, MAX_POP as i32) as u32;
        if new_val == self.pop_amount { return; }
        self.pop_amount = new_val;
        self.sync_pop_buttons();
        self.rebuild_from_params();
    }

    /// Reset pop sharpening to zero and rebuild.
    #[allow(dead_code)]
    pub(crate) fn reset_pop(&mut self) {
        if self.pop_amount == 0 { return; }
        self.pop_amount = 0;
        self.sync_pop_buttons();
        self.rebuild_from_params();
    }

    /// Sync disabled class on the ◀/▶ pop buttons to reflect the current amount.
    /// No-op: buttons removed from UI; preserved for future reinstatement.
    #[allow(dead_code)]
    fn sync_pop_buttons(&self) {}

    // ── AA brush ─────────────────────────────────────────────────────────────

    /// Return a reference to the sprite catalog for the currently active charset.
    pub(crate) fn active_catalog(&self) -> &Vec<Vec<u8>> {
        match self.aa_charset {
            AaCharset::Ascii7  => &self.catalog_ascii7,
            AaCharset::Braille => &self.catalog_braille,
        }
    }

    /// Enable the charset-mode button. Called when AA mode turns on.
    pub(crate) fn enable_aa_charset_btn(&self) {
        let _ = self.aa_charset_btn_el.class_list().remove_1("disabled");
    }

    /// Disable the charset-mode button. Called when AA mode turns off.
    pub(crate) fn disable_aa_charset_btn(&self) {
        let _ = self.aa_charset_btn_el.class_list().add_1("disabled");
    }

    /// Enable the AA mode button. Called after a background image is loaded.
    pub(crate) fn enable_aa_btn(&self) {
        let _ = self.aa_btn_el.class_list().remove_1("disabled");
    }

    /// Disable the AA mode button and turn off AA mode.
    /// Called when the background image is cleared so stale matching can't occur.
    pub(crate) fn disable_aa_btn(&mut self) {
        self.aa_mode = false;
        let _ = self.aa_btn_el.class_list().remove_1("active");
        let _ = self.aa_btn_el.class_list().add_1("disabled");
    }

    /// Return the character to stamp at (col, row) for the current drawing operation.
    /// Eraser always returns BLANK. In AA mode with image + catalog available, queries
    /// the catalog; falls back to brush_char if AA mode is off or no image is loaded.
    fn stamp_char(&self, col: usize, row: usize) -> char {
        if self.tool == Tool::Eraser { return BLANK; }
        if self.aa_mode && self.bg_edges.is_some() && !self.active_catalog().is_empty() {
            self.compute_best_char(col, row)
        } else {
            self.brush_char
        }
    }

    /// Sample the bg_edges region under cell (col, row), compare against the sprite
    /// catalog using sum-of-squared-differences, and return the best-matching char.
    /// Falls back to brush_char if any data is unavailable.
    fn compute_best_char(&self, col: usize, row: usize) -> char {
        let edges = match self.bg_edges.as_ref() {
            Some(e) => e,
            None    => return self.brush_char,
        };
        let catalog = self.active_catalog();
        if catalog.is_empty() { return self.brush_char; }

        let proc_w = self.bg_luma_width  as f64;
        let proc_h = self.bg_luma_height as f64;
        let pw     = proc_w as usize;
        let ph     = proc_h as usize;
        let sw     = asciiart::SPRITE_W as usize;
        let sh     = asciiart::SPRITE_H as usize;

        // Cached transform: (origin_x, origin_y, proc_cell_w, proc_cell_h).
        // origin = processed-image coordinate of the top-left of cell (0,0).
        let (orig_x, orig_y, cell_w, cell_h) = self.bg_visible_rect;
        if cell_w == 0.0 || cell_h == 0.0 { return 32u8 as char; }

        // Processed-image region for this cell (may be partly/fully off image).
        let px0_f = orig_x + col       as f64 * cell_w;
        let py0_f = orig_y + row       as f64 * cell_h;
        let px1_f = orig_x + (col + 1) as f64 * cell_w;
        let py1_f = orig_y + (row + 1) as f64 * cell_h;

        // Cell doesn't overlap the processed image — return space.
        if px1_f <= 0.0 || py1_f <= 0.0 || px0_f >= proc_w || py0_f >= proc_h {
            return 32u8 as char;
        }

        let px0 = px0_f.max(0.0).round() as usize;
        let py0 = py0_f.max(0.0).round() as usize;
        let px1 = px1_f.min(proc_w).round() as usize;
        let py1 = py1_f.min(proc_h).round() as usize;
        let px1 = px1.max(px0 + 1).min(pw);
        let py1 = py1.max(py0 + 1).min(ph);

        // Box-average downsample the cell region to SPRITE_W × SPRITE_H.
        let mut patch = vec![0u8; sw * sh];
        for sy in 0..sh {
            for sx in 0..sw {
                let x0 = px0 + (sx       * (px1 - px0)) / sw;
                let y0 = py0 + (sy       * (py1 - py0)) / sh;
                let x1 = (px0 + ((sx + 1) * (px1 - px0)) / sw).max(x0 + 1).min(pw);
                let y1 = (py0 + ((sy + 1) * (py1 - py0)) / sh).max(y0 + 1).min(ph);
                let mut sum   = 0u32;
                let mut count = 0u32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum   += edges[y * pw + x] as u32;
                        count += 1;
                    }
                }
                patch[sy * sw + sx] = if count > 0 { (sum / count) as u8 } else { 0 };
            }
        }

        // If any patch pixel exceeds the threshold, a strong edge is present and
        // space must not be chosen — start the search at index 1 to exclude it.
        // Weak/mushy cells stay at 0 so space can win through normal SSD matching.
        let patch_max = patch.iter().cloned().max().unwrap_or(0);
        let min_idx   = if patch_max >= asciiart::AA_EDGE_THRESHOLD { 1 } else { 0 };

        let idx = asciiart::best_sprite_match(&patch, catalog, min_idx);
        self.aa_charset.char_from_idx(idx)
    }

    // ── Text entry ───────────────────────────────────────────────────────────

    /// Internal: position the cursor at (col, row) and push an undo snapshot.
    /// Does NOT commit any existing session — callers must do that first.
    fn start_text_session_at(&mut self, col: usize, row: usize) {
        self.push_undo_snapshot();
        self.text_cursor = Some((col, row));
        self.text_origin = Some((col, row));
        if col < COLS {
            self.grid.set_preview(col, row, Some(CURSOR_CHAR));
            self.render_cell(col, row);
            self.cell_els[Grid::idx(col, row)].class_list().add_1("cursor").unwrap();
        }
        // Focus the hidden input to raise the mobile virtual keyboard.
        // Must be called inside a user-gesture handler (mousedown / touchstart / touchend)
        // for the browser to honour it; callers guarantee that invariant.
        let _ = self.text_input_el.focus();
    }

    /// Begin a text session at (col, row), committing any existing session first.
    /// Called from mousedown/touchstart when the text tool is active.
    pub(crate) fn start_text_session(&mut self, col: usize, row: usize) {
        if self.text_origin.is_some() {
            self.commit_text_session();
        }
        self.start_text_session_at(col, row);
    }

    /// Finalise the text session: strip the cursor glyph and commit all typed
    /// characters to the backing store. Safe to call when no session is active.
    pub(crate) fn commit_text_session(&mut self) {
        if self.text_origin.is_none() {
            return; // no session active
        }
        // Strip the cursor glyph from the preview so it doesn't land on the canvas.
        if let Some((col, row)) = self.text_cursor {
            if col < COLS {
                self.grid.set_preview(col, row, None);
                self.cell_els[Grid::idx(col, row)].class_list().remove_1("cursor").unwrap();
                self.render_cell(col, row);
            }
        }
        let dirty = self.grid.commit_preview();
        for (c, r) in dirty {
            self.render_cell(c, r);
        }
        self.text_cursor = None;
        self.text_origin = None;
        // Dismiss the mobile keyboard and clear any buffered input.
        self.text_input_el.set_value("");
        let _ = self.text_input_el.blur();
    }

    /// Cancel the text session: discard all provisional characters, remove the
    /// cursor, pop the session's undo snapshot, and restart the cursor at the
    /// session origin. Called on ESC.
    pub(crate) fn abort_text_session(&mut self) {
        if self.text_origin.is_none() {
            return;
        }
        if let Some((col, row)) = self.text_cursor {
            if col < COLS {
                self.cell_els[Grid::idx(col, row)].class_list().remove_1("cursor").unwrap();
            }
        }
        let dirty = self.grid.abort_preview();
        for (c, r) in dirty {
            self.render_cell(c, r);
        }
        self.undo_stack.pop(); // discard pre-session snapshot — typing was cancelled
        let origin = self.text_origin;
        self.text_cursor = None;
        self.text_origin = None;
        // Clear any buffered input before restarting the session.
        self.text_input_el.set_value("");
        if let Some((oc, or)) = origin {
            self.start_text_session_at(oc, or); // reposition cursor at session start; refocuses
        }
    }

    /// Place a character at the cursor position and advance the cursor right.
    /// If the cursor reaches the right edge the visual cursor disappears but
    /// the session stays alive (Enter or tool-switch will commit, ESC will abort).
    pub(crate) fn type_char(&mut self, ch: char) {
        let (col, row) = match self.text_cursor {
            Some(cr) if cr.0 < COLS => cr,
            _ => return, // past end of row or no session
        };
        // Replace cursor glyph with the typed character.
        self.cell_els[Grid::idx(col, row)].class_list().remove_1("cursor").unwrap();
        self.grid.set_preview(col, row, Some(ch));
        self.render_cell(col, row);
        // Advance cursor.
        let next_col = col + 1;
        if next_col < COLS {
            self.grid.set_preview(next_col, row, Some(CURSOR_CHAR));
            self.render_cell(next_col, row);
            self.cell_els[Grid::idx(next_col, row)].class_list().add_1("cursor").unwrap();
            self.text_cursor = Some((next_col, row));
        } else {
            // End of row: visual cursor disappears, session stays alive.
            self.text_cursor = Some((COLS, row));
        }
    }

    /// Erase the last typed character and retreat the cursor one position.
    /// Does nothing when the cursor is at or before the session origin.
    pub(crate) fn text_backspace(&mut self) {
        let (col, row) = match self.text_cursor {
            Some(cr) => cr,
            None => return,
        };
        let origin_col = match self.text_origin {
            Some((oc, _)) => oc,
            None => return,
        };
        if col <= origin_col {
            return; // at or before session start — nothing to erase
        }
        // Remove visual cursor from current position (only if within canvas bounds).
        if col < COLS {
            self.cell_els[Grid::idx(col, row)].class_list().remove_1("cursor").unwrap();
            self.grid.set_preview(col, row, None);
            self.render_cell(col, row);
        }
        // Move cursor back and overwrite the previous character with the cursor glyph.
        // On commit the cursor glyph is stripped, leaving that cell blank (erased).
        let prev_col = col - 1;
        self.grid.set_preview(prev_col, row, Some(CURSOR_CHAR));
        self.render_cell(prev_col, row);
        self.cell_els[Grid::idx(prev_col, row)].class_list().add_1("cursor").unwrap();
        self.text_cursor = Some((prev_col, row));
    }
}


/// Return every (col, row) on the ellipse outline inscribed in bounding box
/// (bx0,by0)–(bx1,by1), using the Zingl-Bresenham integer algorithm.
/// No floating point or trig. Handles even/odd dimensions and degenerate
/// cases (single cell, horizontal/vertical line).
/// Accepts signed coordinates so center-origin mode can produce negative bounding
/// box corners without clamping (which would distort the ellipse). The `push!`
/// macro clips any out-of-canvas cells before they reach the caller.
fn ellipse_cells(bx0: i64, by0: i64, bx1: i64, by1: i64) -> Vec<(usize, usize)> {
    let mut cells: Vec<(usize, usize)> = Vec::new();

    let (mut x0, mut y0) = (bx0, by0);
    let (mut x1, mut y1) = (bx1, by1);

    if x0 > x1 { std::mem::swap(&mut x0, &mut x1); }
    if y0 > y1 { std::mem::swap(&mut y0, &mut y1); }

    let a = x1 - x0;   // full horizontal span
    let b = y1 - y0;   // full vertical span
    let b_odd = b & 1; // 1 if height is odd — affects midline placement

    let mut dx = 4 * (1 - a) * b * b;
    let mut dy = 4 * (b_odd + 1) * a * a;
    let mut err = dx + dy + b_odd * a * a;

    y0 += (b + 1) / 2; // advance y0 to the horizontal midline
    y1  = y0 - b_odd;

    let da = 8 * a * a; // added to dx each time a y step occurs
    let db = 8 * b * b; // added to dy each time an x step occurs

    // Inline bounds-checked push; defined as a macro to avoid closure borrow conflicts.
    macro_rules! push {
        ($px:expr, $py:expr) => {{
            let (px, py): (i64, i64) = ($px, $py);
            if px >= 0 && py >= 0 && (px as usize) < COLS && (py as usize) < ROWS {
                cells.push((px as usize, py as usize));
            }
        }};
    }

    // Main loop: x marches inward; y steps outward as the error term demands.
    // Four symmetric points are plotted each iteration (one per quadrant).
    loop {
        push!(x1, y0); // right, upper half
        push!(x0, y0); // left,  upper half
        push!(x0, y1); // left,  lower half
        push!(x1, y1); // right, lower half

        let e2 = 2 * err;
        if e2 <= dy                  { y0 += 1; y1 -= 1; dy += da; err += dy; }
        if e2 >= dx || 2 * err > dy  { x0 += 1; x1 -= 1; dx += db; err += dx; }

        if x0 > x1 { break; }
    }

    // Flat-ellipse correction: for very wide/short ellipses the main loop can
    // exit before the vertical tips are fully drawn; fill them in here.
    while y0 - y1 <= b {
        push!(x0 - 1, y0);
        push!(x1 + 1, y0); y0 += 1;
        push!(x0 - 1, y1);
        push!(x1 + 1, y1); y1 -= 1;
    }

    cells
}

/// Filled ellipse using the same Zingl-Bresenham stepper as `ellipse_cells`.
///
/// At each step the stepper knows the left (x0) and right (x1) extent at the
/// current pair of rows (y0 upper, y1 lower). We fill the full horizontal span
/// [x0, x1] at each row instead of just plotting the two edge points, so the
/// interior is derived from exactly the same integer geometry as the outline.
/// This guarantees zero gaps or overlaps when outline and fill use different
/// characters in future.
///
/// Spans are clipped to the canvas; out-of-canvas rows are skipped entirely.
fn filled_ellipse_cells(bx0: i64, by0: i64, bx1: i64, by1: i64) -> Vec<(usize, usize)> {
    let mut cells: Vec<(usize, usize)> = Vec::new();

    let (mut x0, mut y0) = (bx0, by0);
    let (mut x1, mut y1) = (bx1, by1);

    if x0 > x1 { std::mem::swap(&mut x0, &mut x1); }
    if y0 > y1 { std::mem::swap(&mut y0, &mut y1); }

    let a = x1 - x0;
    let b = y1 - y0;
    let b_odd = b & 1;

    let mut dx = 4 * (1 - a) * b * b;
    let mut dy = 4 * (b_odd + 1) * a * a;
    let mut err = dx + dy + b_odd * a * a;

    y0 += (b + 1) / 2;
    y1  = y0 - b_odd;

    let da = 8 * a * a;
    let db = 8 * b * b;

    // Fill [xl, xr] on row py, clipping to canvas bounds.
    // xl > xr produces an empty range and is silently skipped.
    macro_rules! fill_span {
        ($py:expr, $xl:expr, $xr:expr) => {{
            let (py, xl, xr): (i64, i64, i64) = ($py, $xl, $xr);
            if py >= 0 && (py as usize) < ROWS && xl <= xr {
                let x_lo = xl.max(0) as usize;
                let x_hi = xr.min(COLS as i64 - 1);
                if x_hi >= 0 {
                    for px in x_lo..=(x_hi as usize) {
                        cells.push((px, py as usize));
                    }
                }
            }
        }};
    }

    loop {
        fill_span!(y0, x0, x1); // upper half span
        fill_span!(y1, x0, x1); // lower half span (same row as y0 when b is even)

        let e2 = 2 * err;
        if e2 <= dy                  { y0 += 1; y1 -= 1; dy += da; err += dy; }
        if e2 >= dx || 2 * err > dy  { x0 += 1; x1 -= 1; dx += db; err += dx; }

        if x0 > x1 { break; }
    }

    // Flat-ellipse correction: fill the remaining tip rows not covered by the main loop.
    while y0 - y1 <= b {
        fill_span!(y0, x0 - 1, x1 + 1);
        y0 += 1;
        fill_span!(y1, x0 - 1, x1 + 1);
        y1 -= 1;
    }

    cells
}

/// 8-directional flood fill: return every cell reachable from (start_col, start_row)
/// via cells whose committed character equals `target_char`.
/// Uses BFS so the fill order is outward from the origin — natural for a paint bucket.
/// The canvas is small (80×24 = 1920 cells max) so a flat visited bitset is fast.
fn flood_fill_cells(
    start_col: usize,
    start_row: usize,
    target_char: char,
    committed: &[char],
    diagonals: bool,
) -> Vec<(usize, usize)> {
    let mut visited = vec![false; COLS * ROWS];
    let mut queue   = std::collections::VecDeque::new();
    let mut result  = Vec::new();

    let start_idx = Grid::idx(start_col, start_row);
    visited[start_idx] = true;
    queue.push_back((start_col, start_row));

    while let Some((c, r)) = queue.pop_front() {
        result.push((c, r));
        // Check neighbours — diagonals included only in Flood8 mode.
        for dr in -1i32..=1 {
            for dc in -1i32..=1 {
                if dc == 0 && dr == 0 { continue; }
                if !diagonals && dc != 0 && dr != 0 { continue; }
                let nc = c as i32 + dc;
                let nr = r as i32 + dr;
                if nc < 0 || nr < 0 || nc >= COLS as i32 || nr >= ROWS as i32 { continue; }
                let (nc, nr) = (nc as usize, nr as usize);
                let idx = Grid::idx(nc, nr);
                if !visited[idx] && committed[idx] == target_char {
                    visited[idx] = true;
                    queue.push_back((nc, nr));
                }
            }
        }
    }
    result
}

/// Map a Bresenham step direction vector to the ASCII art character that best
/// represents it. Covers all four axis-aligned and diagonal cases.
fn art_char(dc: i32, dr: i32) -> char {
    match (dc.signum(), dr.signum()) {
        (1, 0) | (-1, 0)  => '-',
        (0, 1) | (0, -1)  => '|',
        (1, 1) | (-1, -1) => '\\',
        _                  => '/',
    }
}


// ── HTML clipboard helpers ────────────────────────────────────────────────────

/// Escape the three characters that are special in HTML text content.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Wrap plain-text canvas content in a styled <pre> block for rich clipboard copy.
/// No foreground/background colors are embedded — those belong to per-cell spans
/// once color support is added; for now the receiving app's default text color is fine.
/// Trailing spaces are trimmed per line since most rich-text editors drop them anyway.
fn html_wrap(_dark_mode: bool, text: &str) -> String {
    let trimmed: String = text
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<pre style=\"font-family:monospace;line-height:1.2;white-space:pre;padding:4px\">{}</pre>",
        html_escape(&trimmed)
    )
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Build timestamp injected by build.rs — format "Build: MMDD:HHMM".
const BUILD_TS: &str = env!("BUILD_TIMESTAMP");

/// Called automatically by the generated JS glue when `init()` is awaited.
/// Builds the DOM, initialises application state, wires all event handlers.
#[wasm_bindgen(start)]
pub fn start() {
    // Route Rust panics to the browser console with a readable message + stack trace.
    console_error_panic_hook::set_once();

    let window: Window = web_sys::window().expect("no global window object");
    let document: Document = window.document().expect("window has no document");

    // Build the dynamic DOM sections that Rust owns
    let cell_els = dom_setup::build_grid(&document);
    dom_setup::build_palette(&document);

    // Initialise shared application state (shared via Rc<RefCell> across closures)
    let grid_el = document.get_element_by_id("grid").unwrap();
    let text_input_el = document
        .get_element_by_id("text-input")
        .expect("#text-input must exist in HTML")
        .dyn_into::<HtmlInputElement>()
        .expect("#text-input must be an <input> element");
    let aa_btn_el = document
        .get_element_by_id("aa-mode-btn")
        .expect("#aa-mode-btn must exist in HTML");
    let bg_eye_el = document
        .get_element_by_id("bg-visibility-btn")
        .expect("#bg-visibility-btn must exist in HTML");
    let aa_charset_btn_el = document
        .get_element_by_id("aa-charset-btn")
        .expect("#aa-charset-btn must exist in HTML");
    let image_controls_el = document
        .get_element_by_id("image-controls")
        .expect("#image-controls must exist in HTML");
    let image_controls_hide_el = document
        .get_element_by_id("image-controls-hide")
        .expect("#image-controls-hide must exist in HTML")
        .dyn_into::<web_sys::HtmlInputElement>()
        .expect("#image-controls-hide must be an <input> element");
    let bg_move_btn_el = document
        .get_element_by_id("bg-move-btn")
        .expect("#bg-move-btn must exist in HTML");
    let app = Rc::new(RefCell::new(App::new(cell_els, grid_el, text_input_el, aa_btn_el, bg_eye_el, aa_charset_btn_el, image_controls_el, image_controls_hide_el, bg_move_btn_el)));

    // Build sprite catalogs for each charset at startup.
    app.borrow_mut().catalog_ascii7  = dom_setup::build_ascii7_catalog(&document);
    app.borrow_mut().catalog_braille = dom_setup::build_braille_catalog(&document);

    // Stamp the build time below the canvas so the version is always visible.
    if let Some(el) = document.get_element_by_id("build-info") {
        el.set_text_content(Some(BUILD_TS));
    }

    // Populate the canvas with demo content to prove the pipeline end-to-end
    dom_setup::draw_demo(&mut app.borrow_mut());

    // Attach all event listeners — each takes a clone of the Rc, not ownership
    wire_grid_mouse(&document, &app);
    wire_toolbar(&document, &app);
    wire_palette(&document, &app);
    wire_undo_redo(&document, &app);
    wire_theme_toggle(&document, &app);
    wire_blend_mode(&document, &app);
    wire_line_tool(&document, &app);
    wire_pencil_tool(&document, &app);
    wire_fill_tool(&document, &app);
    wire_copy(&document, &app);
    wire_clear(&document, &app);
    wire_touch(&document, &app);
    wire_shift_toggle(&document, &app);
    wire_text_input(&document, &app);
    wire_help(&document, &app);
    wire_drag_drop(&document, &app);
    wire_outline_mode(&document, &app);
    wire_aa_mode(&document, &app);
    wire_aa_charset(&document, &app);
    wire_bg_visibility(&document, &app);
    wire_bg_move_tool(&document, &app);
    wire_load_image(&document, &app);
    wire_image_controls(&document, &app);
}
