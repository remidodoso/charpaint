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

mod wiring;
use wiring::{wire_grid_mouse, wire_toolbar, wire_palette, wire_undo_redo, wire_theme_toggle, wire_blend_mode, wire_line_tool, wire_copy, wire_clear, wire_touch, wire_shift_toggle, wire_text_input};

// ── Constants ────────────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

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
    Fill,     // NIY
    Text,
    Line,     // NIY — will use brush char or directional chars (─ │ ╲ ╱)
    Rect,     // NIY — hollow rectangle, style driven by palette selection
    RectFill, // NIY — filled rectangle
    Oval,
    OvalFill,
}

impl Tool {
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

    pub(crate) shift_locked: bool,       // true when the ⇧ toggle is active (axis constraint)
    pub(crate) dark_mode: bool,
    pub(crate) blend_mode: BlendMode, // how new paint interacts with existing cells
    pub(crate) line_mode:  LineMode,  // character-stamp vs art-geometry line drawing

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

    // ── Touch / pinch-zoom state ─────────────────────────────────────────────
    grid_el:                          Element,      // #grid element — target for font-size changes
    pub(crate) font_size:             f64,          // current grid font size in px (default 16)
    pub(crate) is_two_finger:         bool,         // true while two touches are on screen
    pub(crate) pinch_start_dist:      f64,          // finger separation when current pinch began
    pub(crate) pinch_start_font_size: f64,          // font_size at the start of current pinch
    pub(crate) pan_last_mid:          (f64, f64),   // last two-touch midpoint, for pan delta
}

impl App {
    fn new(cell_els: Vec<Element>, grid_el: Element, text_input_el: HtmlInputElement) -> Self {
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
            shift_locked: false,
            dark_mode:  true,
            blend_mode: BlendMode::Overwrite,
            line_mode:  LineMode::Character,
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
            Tool::Pencil | Tool::Eraser => {
                let ch = if self.tool == Tool::Pencil { self.brush_char } else { BLANK };
                let (col, row) = self.resolve_target(col, row, shift_held || self.shift_locked);
                let cells = match self.last_painted_cell {
                    Some((pc, pr)) => bresenham(pc, pr, col, row),
                    None           => vec![(col, row)],
                };
                for (c, r) in cells {
                    // Stamp: only paint blank cells; test committed so earlier preview
                    // cells in this stroke don't shadow the check.
                    if self.blend_mode == BlendMode::Stamp
                        && self.grid.committed[Grid::idx(c, r)] != BLANK
                    {
                        continue;
                    }
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
                        let ch = match self.line_mode {
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
                        self.grid.set_preview(c, r, Some(self.brush_char));
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
                            self.grid.set_preview(c, r, Some(self.brush_char));
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
                        self.grid.set_preview(c, r, Some(self.brush_char));
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
                        self.grid.set_preview(c, r, Some(self.brush_char));
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

    // ── Touch / zoom ─────────────────────────────────────────────────────────

    /// Set the grid font size, clamped to 8–48 px, and apply it to the DOM.
    /// Called on every pinch-zoom touchmove frame.
    pub(crate) fn set_font_size(&mut self, size: f64) {
        self.font_size = size.max(8.0).min(48.0);
        self.grid_el
            .set_attribute("style", &format!("font-size: {}px", self.font_size))
            .unwrap();
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

// ── DOM construction ─────────────────────────────────────────────────────────

/// Populate `#grid` with COLS×ROWS `<span class="cell">` elements.
/// Returns the flat element vec stored in App for direct render access.
fn build_grid(document: &Document) -> Vec<Element> {
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

/// One entry in the character palette strip.
struct PalEntry {
    label:     &'static str, // visible glyph in the palette button
    ch:        char,         // actual character value painted by this entry
    initially_active: bool,
}

/// Populate `#palette` with character picker entries.
/// Each entry gets a `data-char` attribute read by the click handler in wire_palette.
fn build_palette(document: &Document) {
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
                el.set_class_name(if e.initially_active {
                    "pal-char active"
                } else {
                    "pal-char"
                });
                el.set_text_content(Some(e.label));
                // data-char carries the actual character value to the click handler.
                el.set_attribute("data-char", &e.ch.to_string()).unwrap();
                container.append_child(&el).unwrap();
            }
        }
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


// ── Demo content ─────────────────────────────────────────────────────────────

/// Draw a border and greeting to prove the Rust→DOM rendering pipeline works.
/// Remove or replace once real drawing tools are exercised.
fn draw_demo(app: &mut App) {
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
    let cell_els = build_grid(&document);
    build_palette(&document);

    // Initialise shared application state (shared via Rc<RefCell> across closures)
    let grid_el = document.get_element_by_id("grid").unwrap();
    let text_input_el = document
        .get_element_by_id("text-input")
        .expect("#text-input must exist in HTML")
        .dyn_into::<HtmlInputElement>()
        .expect("#text-input must be an <input> element");
    let app = Rc::new(RefCell::new(App::new(cell_els, grid_el, text_input_el)));

    // Stamp the build time below the canvas so the version is always visible.
    if let Some(el) = document.get_element_by_id("build-info") {
        el.set_text_content(Some(BUILD_TS));
    }

    // Populate the canvas with demo content to prove the pipeline end-to-end
    draw_demo(&mut app.borrow_mut());

    // Attach all event listeners — each takes a clone of the Rc, not ownership
    wire_grid_mouse(&document, &app);
    wire_toolbar(&document, &app);
    wire_palette(&document, &app);
    wire_undo_redo(&document, &app);
    wire_theme_toggle(&document, &app);
    wire_blend_mode(&document, &app);
    wire_line_tool(&document, &app);
    wire_copy(&document, &app);
    wire_clear(&document, &app);
    wire_touch(&document, &app);
    wire_shift_toggle(&document, &app);
    wire_text_input(&document, &app);
}
