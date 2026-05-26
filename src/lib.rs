//! charpaint — a MacPaint-style character-cell painting app compiled to WebAssembly.
//!
//! All application logic lives here. The HTML/JS side is a minimal structural
//! shell; after `init()` is awaited in JS, this module owns the DOM and all
//! interactivity.

use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, Window};

mod util;
use util::bresenham;

mod wiring;
use wiring::{wire_grid_mouse, wire_toolbar, wire_palette, wire_undo_redo, wire_theme_toggle, wire_blend_mode, wire_copy};

// ── Constants ────────────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

/// The character placed in a cell when it is blank / erased.
const BLANK: char = ' ';

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
    /// HTML `data-mode` attribute string → BlendMode variant.
    pub(crate) fn from_data_attr(s: &str) -> Option<Self> {
        match s {
            "overwrite"  => Some(BlendMode::Overwrite),
            "stamp"      => Some(BlendMode::Stamp),
            "combine"    => Some(BlendMode::Combine),
            "difference" => Some(BlendMode::Difference),
            _            => None,
        }
    }

    /// Unicode icon for the mode, displayed in the mode-button tile.
    pub(crate) fn icon(&self) -> &'static str {
        match self {
            BlendMode::Overwrite  => "▊",
            BlendMode::Stamp      => "⬚",
            BlendMode::Combine    => "┼",
            BlendMode::Difference => "∖",
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
    Text,     // NIY
    Line,     // NIY — will use brush char or directional chars (─ │ ╲ ╱)
    Rect,     // NIY — hollow rectangle, style driven by palette selection
    RectFill, // NIY — filled rectangle
    Oval,     // NIY
    OvalFill, // NIY
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

    pub(crate) dark_mode: bool,
    pub(crate) blend_mode:         BlendMode, // how new paint interacts with existing cells
    pub(crate) mode_dropdown_open: bool,      // true while the blend mode fly-out is visible

    /// Active selection bounding box (c0, r0, c1, r1), normalized so c0≤c1, r0≤r1.
    /// None means no selection. Cleared on tool switch or ESC; persists after mouseup.
    pub(crate) selection: Option<(usize, usize, usize, usize)>,
}

impl App {
    fn new(cell_els: Vec<Element>) -> Self {
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
            dark_mode:  true,
            blend_mode:         BlendMode::Overwrite,
            mode_dropdown_open: false,
            selection: None,
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
                let (col, row) = self.resolve_target(col, row, shift_held);
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
                    for (c, r) in bresenham(sc, sr, col, row) {
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

            Tool::Rect => {
                // Clear the previous preview and redraw the rectangle outline
                // from draw_start to the current cursor position each mousemove.
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let c0 = sc.min(col);
                    let c1 = sc.max(col);
                    let r0 = sr.min(row);
                    let r1 = sr.max(row);
                    // Collect the four edges; corners are shared so using ranges
                    // avoids painting them twice.
                    let mut cells: Vec<(usize, usize)> = Vec::new();
                    for c in c0..=c1 { cells.push((c, r0)); cells.push((c, r1)); } // top & bottom
                    for r in r0..=r1 { cells.push((c0, r)); cells.push((c1, r)); } // left & right
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
                let dirty = self.grid.abort_preview();
                for (c, r) in dirty {
                    self.render_cell(c, r);
                }
                if let Some((sc, sr)) = self.draw_start {
                    let c0 = sc.min(col);
                    let c1 = sc.max(col);
                    let r0 = sr.min(row);
                    let r1 = sr.max(row);
                    for r in r0..=r1 {
                        for c in c0..=c1 {
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

    // ── Undo / redo ──────────────────────────────────────────────────────────

    /// Snapshot committed state onto the undo stack before a destructive commit.
    /// Must be called before writing the final mouseup state, not after.
    pub(crate) fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.grid.committed.clone());
        self.redo_stack.clear(); // new action invalidates the redo branch
    }

    /// Restore the previous committed state.
    pub(crate) fn undo(&mut self) {
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

/// Create the `#mode-dropdown` fly-out and append it to `#toolbar`.
/// The dropdown is hidden by default (no `.open` class); `wire_blend_mode`
/// toggles visibility in response to mousedown / mouseup.
fn build_blend_mode_control(document: &Document) {
    let toolbar = document
        .get_element_by_id("toolbar")
        .expect("#toolbar must exist in HTML");

    let dropdown = document.create_element("div").unwrap();
    dropdown.set_attribute("id", "mode-dropdown").unwrap();

    // One tile per blend mode — Overwrite starts selected (matches App default).
    let modes: &[(&str, &str, &str, bool)] = &[
        ("overwrite",  "▊", "Overwrite — replace any cell",   true),
        ("stamp",      "⬚", "Stamp — paint only blank cells", false),
        ("combine",    "┼", "Combine (NIY)",                  false),
        ("difference", "∖", "Difference (NIY)",               false),
    ];

    for &(mode_id, icon, title, initially_selected) in modes {
        let tile = document.create_element("div").unwrap();
        tile.set_class_name(if initially_selected {
            "mode-tile selected"
        } else {
            "mode-tile"
        });
        tile.set_attribute("data-mode", mode_id).unwrap();
        tile.set_attribute("title", title).unwrap();
        tile.set_text_content(Some(icon));
        dropdown.append_child(&tile).unwrap();
    }

    toolbar.append_child(&dropdown).unwrap();
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
    build_blend_mode_control(&document);

    // Initialise shared application state (shared via Rc<RefCell> across closures)
    let app = Rc::new(RefCell::new(App::new(cell_els)));

    // Populate the canvas with demo content to prove the pipeline end-to-end
    draw_demo(&mut app.borrow_mut());

    // Attach all event listeners — each takes a clone of the Rc, not ownership
    wire_grid_mouse(&document, &app);
    wire_toolbar(&document, &app);
    wire_palette(&document, &app);
    wire_undo_redo(&document, &app);
    wire_theme_toggle(&document, &app);
    wire_blend_mode(&document, &app);
    wire_copy(&document, &app);
}
