//! charpaint — a MacPaint-style character-cell painting app compiled to WebAssembly.
//!
//! All application logic lives here. The HTML/JS side is a minimal structural
//! shell; after `init()` is awaited in JS, this module owns the DOM and all
//! interactivity.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, KeyboardEvent, MouseEvent, NodeList, Window};

// ── Constants ─────────────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

/// The character placed in a cell when it is blank / erased.
const BLANK: char = ' ';

// ── Tool ──────────────────────────────────────────────────────────────────────

/// Every tool the toolbar can activate.
/// Variants marked NIY are declared so the data model is complete but their
/// drawing logic is not yet implemented — they will no-op gracefully.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
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
    fn from_data_attr(s: &str) -> Option<Self> {
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

// ── Grid ──────────────────────────────────────────────────────────────────────

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

// ── Application state ─────────────────────────────────────────────────────────

struct App {
    grid: Grid,

    /// Flat list of DOM `<span class="cell">` elements, one per grid cell,
    /// indexed `[row * COLS + col]`. Stored here to avoid repeated getElementById
    /// lookups — rendering just writes directly into these elements.
    cell_els: Vec<Element>,

    tool:       Tool,
    brush_char: char, // character the pencil/fill paints with

    is_drawing: bool,

    /// Last cell painted during the current stroke, used by Bresenham interpolation
    /// to fill gaps when the mouse moves faster than mousemove events fire.
    /// Cleared on mouseup / mousedown start.
    last_painted_cell: Option<(usize, usize)>,

    // TBD: draw_start: Option<(usize, usize)> — needed by line/rect/oval tools
    //      to remember where the drag began so preview can be redrawn on each
    //      mousemove from start→current rather than just stamping each cell.

    /// Undo history — each entry is a full snapshot of `grid.committed`.
    /// Pushed on every mouseup commit. Cheap: 80×24×4 B ≈ 7.5 KB per entry.
    undo_stack: Vec<Vec<char>>, // TBD: wire to Ctrl+Z

    /// States available for redo. Populated by undo(); cleared by new commits.
    redo_stack: Vec<Vec<char>>, // TBD: wire to Ctrl+Y / Ctrl+Shift+Z

    dark_mode: bool,
}

impl App {
    fn new(cell_els: Vec<Element>) -> Self {
        App {
            grid: Grid::new(),
            cell_els,
            tool:       Tool::Pencil,
            brush_char: '*',
            is_drawing: false,
            last_painted_cell: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dark_mode:  true,
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Push the current display character for one cell to its DOM element.
    fn render_cell(&self, col: usize, row: usize) {
        let ch = self.grid.display_char(col, row);
        // Space must not be HTML-collapsed; CSS `white-space: pre` on .cell handles this.
        self.cell_els[Grid::idx(col, row)]
            .set_text_content(Some(&ch.to_string()));
    }

    /// Re-render the entire grid — used after undo/redo or a full state restore.
    fn render_all(&self) {
        for r in 0..ROWS {
            for c in 0..COLS {
                self.render_cell(c, r);
            }
        }
    }

    // ── Painting ──────────────────────────────────────────────────────────────

    /// Stamp a single character into the committed grid and update its DOM cell.
    fn paint_cell(&mut self, col: usize, row: usize, ch: char) {
        self.grid.set_committed(col, row, ch);
        self.render_cell(col, row);
    }

    /// Paint a continuous stroke from `last_painted_cell` to (col, row).
    ///
    /// Uses Bresenham's line algorithm to fill any cells skipped when the mouse
    /// moves faster than mousemove events fire. On the first call of a stroke
    /// (last_painted_cell is None) only the target cell is painted.
    ///
    /// For multi-cell preview tools (line, rect, oval) this will instead write
    /// to the preview layer — NIY for those tools.
    fn paint_stroke_to(&mut self, col: usize, row: usize) {
        let ch = match self.tool {
            Tool::Pencil => self.brush_char,
            Tool::Eraser => BLANK,
            _ => return, // NIY tools are silent no-ops for now
        };

        let cells = match self.last_painted_cell {
            Some((pc, pr)) => bresenham(pc, pr, col, row),
            None            => vec![(col, row)],
        };

        for (c, r) in cells {
            self.paint_cell(c, r, ch);
        }

        self.last_painted_cell = Some((col, row));
    }

    // ── Undo / redo ───────────────────────────────────────────────────────────

    /// Snapshot committed state onto the undo stack before a destructive commit.
    /// Must be called before writing the final mouseup state, not after.
    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.grid.committed.clone());
        self.redo_stack.clear(); // new action invalidates the redo branch
    }

    /// Restore the previous committed state.
    fn undo(&mut self) {
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
    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.grid.committed.clone());
            self.grid.committed = next;
            self.grid.preview = vec![None; COLS * ROWS];
            self.is_drawing = false;
            self.last_painted_cell = None;
            self.render_all();
        }
    }
}

// ── Bresenham's line algorithm ────────────────────────────────────────────────

/// Return every (col, row) cell on the straight line from (c0,r0) to (c1,r1),
/// inclusive of both endpoints. Used to interpolate between consecutive
/// mousemove positions so fast strokes don't leave gaps.
fn bresenham(c0: usize, r0: usize, c1: usize, r1: usize) -> Vec<(usize, usize)> {
    let (mut x, mut y) = (c0 as i32, r0 as i32);
    let (x1, y1)       = (c1 as i32, r1 as i32);

    let dx =  (x1 - x).abs();
    let dy = -(y1 - y).abs(); // negated so error term works with a single comparison
    let sx = if x < x1 { 1 } else { -1 };
    let sy = if y < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    let mut cells = Vec::new();
    loop {
        cells.push((x as usize, y as usize));
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
    cells
}

// ── Cell hit-testing ──────────────────────────────────────────────────────────

/// Read `data-col` and `data-row` from the closest `.cell` ancestor of the
/// mouse event's target. Returns None if the event didn't land on the grid.
fn cell_from_mouse_event(e: &MouseEvent) -> Option<(usize, usize)> {
    let target: Element = e.target()?.dyn_into().ok()?;

    // Walk up in case a sub-node (e.g. a text node's parent span) caught the event.
    let cell_el = if target.class_list().contains("cell") {
        target
    } else {
        target.closest(".cell").ok()??
    };

    let col: usize = cell_el.get_attribute("data-col")?.parse().ok()?;
    let row: usize = cell_el.get_attribute("data-row")?.parse().ok()?;
    Some((col, row))
}

// ── DOM construction ──────────────────────────────────────────────────────────

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
        Some(PalEntry { label: "␣", ch: ' ', initially_active: false }), // space = erase
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

// ── Demo content ──────────────────────────────────────────────────────────────

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

// ── Event wiring ──────────────────────────────────────────────────────────────

/// Attach mouse handlers to `#grid` and a global mouseup handler to `window`.
///
/// mousedown → begin stroke and paint first cell
/// mousemove → continue painting while button held
/// mouseup   → commit stroke, push undo snapshot
/// TBD: ESC keydown → abort_preview (cancel in-progress operation)
fn wire_grid_mouse(document: &Document, app: &Rc<RefCell<App>>) {
    let grid_el = document.get_element_by_id("grid").unwrap();

    // mousedown — start a new draw stroke
    {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            e.prevent_default(); // suppress browser text-selection during drag
            if let Some((col, row)) = cell_from_mouse_event(&e) {
                let mut a = app.borrow_mut();
                // Snapshot before the stroke begins so Ctrl+Z restores pre-stroke state.
                // Also clears redo stack — a new stroke invalidates any undone future.
                a.push_undo_snapshot();
                a.is_drawing = true;
                a.last_painted_cell = None; // fresh stroke — no interpolation on first cell
                a.paint_stroke_to(col, row);
            }
        });
        grid_el
            .add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget(); // closure must live for the page lifetime
    }

    // mousemove — extend the stroke, Bresenham-filling any skipped cells
    {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let mut a = app.borrow_mut();
            if !a.is_drawing {
                return;
            }
            if let Some((col, row)) = cell_from_mouse_event(&e) {
                a.paint_stroke_to(col, row);
            }
        });
        grid_el
            .add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // mouseup on window — commit the finished stroke
    // Listening on window (not just #grid) catches releases outside the canvas.
    {
        let window: Window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |_e: MouseEvent| {
            let mut a = app.borrow_mut();
            if a.is_drawing {
                // TBD: for preview-based tools (line, rect) commit_preview() fires here.
                a.is_drawing = false;
                a.last_painted_cell = None;
            }
        });
        window
            .add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

/// Wire click handlers to `.tool` toolbar buttons.
/// Each button's `data-tool` attribute identifies the Tool variant to activate.
fn wire_toolbar(document: &Document, app: &Rc<RefCell<App>>) {
    let tool_nodes: NodeList = document.query_selector_all(".tool").unwrap();

    for i in 0..tool_nodes.length() {
        let el: Element = tool_nodes
            .item(i)
            .unwrap()
            .dyn_into()
            .expect("tool node must be Element");

        let tool = match el
            .get_attribute("data-tool")
            .and_then(|s| Tool::from_data_attr(&s))
        {
            Some(t) => t,
            None => continue,
        };

        let app       = Rc::clone(app);
        let el_clone  = el.clone();
        let doc_clone = document.clone();

        let cb = Closure::<dyn FnMut()>::new(move || {
            app.borrow_mut().tool = tool;

            // Move the `active` CSS class to the clicked button
            let all = doc_clone.query_selector_all(".tool").unwrap();
            for j in 0..all.length() {
                let t: Element = all.item(j).unwrap().dyn_into().unwrap();
                t.class_list().remove_1("active").unwrap();
            }
            el_clone.class_list().add_1("active").unwrap();
        });

        el.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

/// Wire click handlers to `.pal-char` palette entries.
/// Each entry's `data-char` attribute holds the character it represents.
fn wire_palette(document: &Document, app: &Rc<RefCell<App>>) {
    let pal_nodes: NodeList = document.query_selector_all(".pal-char").unwrap();

    for i in 0..pal_nodes.length() {
        let el: Element = pal_nodes
            .item(i)
            .unwrap()
            .dyn_into()
            .expect("palette node must be Element");

        // Parse the character from the data attribute set by build_palette()
        let ch: char = match el
            .get_attribute("data-char")
            .and_then(|s| s.chars().next())
        {
            Some(c) => c,
            None => continue,
        };

        let app       = Rc::clone(app);
        let el_clone  = el.clone();
        let doc_clone = document.clone();

        let cb = Closure::<dyn FnMut()>::new(move || {
            app.borrow_mut().brush_char = ch;

            // Move the `active` CSS class to the clicked palette entry
            let all = doc_clone.query_selector_all(".pal-char").unwrap();
            for j in 0..all.length() {
                let p: Element = all.item(j).unwrap().dyn_into().unwrap();
                p.class_list().remove_1("active").unwrap();
            }
            el_clone.class_list().add_1("active").unwrap();
        });

        el.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

/// Wire the ↩/↪ undo/redo buttons and Ctrl+Z / Shift+Ctrl+Z keyboard shortcuts.
fn wire_undo_redo(document: &Document, app: &Rc<RefCell<App>>) {
    // ↩ Undo button click
    if let Some(btn) = document.get_element_by_id("btn-undo") {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut()>::new(move || { app.borrow_mut().undo(); });
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // ↪ Redo button click
    if let Some(btn) = document.get_element_by_id("btn-redo") {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut()>::new(move || { app.borrow_mut().redo(); });
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // Keyboard: Ctrl+Z → undo, Shift+Ctrl+Z → redo
    // Listening on window so it works regardless of which element has focus.
    {
        let window: Window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            if e.ctrl_key() && e.key() == "z" {
                e.prevent_default(); // suppress browser's own undo in any editable fields
                if e.shift_key() {
                    app.borrow_mut().redo();
                } else {
                    app.borrow_mut().undo();
                }
            }
        });
        window.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Wire the light/dark theme toggle button.
/// Flips `data-theme` on `<html>` so CSS variable rules re-evaluate automatically.
fn wire_theme_toggle(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("theme-toggle") {
        Some(el) => el,
        None => return,
    };

    let app       = Rc::clone(app);
    let doc_clone = document.clone();

    let cb = Closure::<dyn FnMut()>::new(move || {
        let mut a = app.borrow_mut();
        a.dark_mode = !a.dark_mode;
        let dark = a.dark_mode;

        doc_clone
            .document_element()
            .unwrap()
            .set_attribute("data-theme", if dark { "dark" } else { "light" })
            .unwrap();

        // Update the button label to show what clicking again will do
        if let Some(b) = doc_clone.get_element_by_id("theme-toggle") {
            b.set_text_content(Some(if dark { "☀ Light" } else { "☾ Dark" }));
        }
    });

    btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
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
}
