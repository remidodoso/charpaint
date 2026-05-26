//! Wiring — connects DOM elements to App state via browser event listeners.
//!
//! Each `wire_*` function attaches one or more listeners and `.forget()`s the
//! closures so they live for the page lifetime. No business logic lives here;
//! the functions are pure patch cables between browser events and App methods.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, KeyboardEvent, MouseEvent, NodeList, TouchEvent, Window};

use wasm_bindgen_futures::spawn_local;

use crate::{App, BlendMode, LineMode, Tool};
use crate::util::{cell_from_coords, cell_from_mouse_event, flash_button, flash_button_error, wire_long_press};

/// Attach mouse handlers to `#grid` and a global mouseup handler to `window`.
///
/// mousedown → begin stroke and paint first cell
/// mousemove → continue painting while button held
/// mouseup   → commit stroke (listening on window catches releases outside the canvas)
pub fn wire_grid_mouse(document: &Document, app: &Rc<RefCell<App>>) {
    let grid_el = document.get_element_by_id("grid").unwrap();

    // mousedown — start a new draw stroke
    {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            e.prevent_default(); // suppress browser text-selection during drag
            if let Some((col, row)) = cell_from_mouse_event(&e) {
                let mut a = app.borrow_mut();
                // Select doesn't modify the canvas so needs no undo snapshot.
                // All other tools snapshot before the stroke so Ctrl+Z can restore.
                if a.tool != Tool::Select {
                    a.push_undo_snapshot();
                }
                a.is_drawing = true;
                a.draw_start  = Some((col, row));
                a.locked_axis = None;
                a.last_painted_cell = None; // fresh stroke — no interpolation on first cell
                a.paint_stroke_to(col, row, e.shift_key());
            }
        });
        grid_el
            .add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
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
                a.paint_stroke_to(col, row, e.shift_key());
            }
        });
        grid_el
            .add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // mouseup on window — commit the finished stroke
    {
        let window: Window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |_e: MouseEvent| {
            let mut a = app.borrow_mut();
            if a.is_drawing {
                a.commit_stroke();
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
pub fn wire_toolbar(document: &Document, app: &Rc<RefCell<App>>) {
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

        // Line tool has sub-modes and uses long-press; wired separately by wire_line_tool.
        if tool == Tool::Line { continue; }

        let app       = Rc::clone(app);
        let el_clone  = el.clone();
        let doc_clone = document.clone();

        let cb = Closure::<dyn FnMut()>::new(move || {
            {
                let mut a = app.borrow_mut();
                a.clear_selection(); // switching tools always drops any active selection
                a.tool = tool;
            }

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
pub fn wire_palette(document: &Document, app: &Rc<RefCell<App>>) {
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
pub fn wire_undo_redo(document: &Document, app: &Rc<RefCell<App>>) {
    // ↩ Undo button click
    if let Some(btn) = document.get_element_by_id("btn-undo") {
        let app = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            app.borrow_mut().undo();
            flash_button(&btn_clone);
        });
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // ↪ Redo button click
    if let Some(btn) = document.get_element_by_id("btn-redo") {
        let app = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            app.borrow_mut().redo();
            flash_button(&btn_clone);
        });
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // Keyboard: Ctrl+Z → undo, Shift+Ctrl+Z → redo, Escape → abort stroke.
    // Listening on window so it works regardless of which element has focus.
    {
        let window: Window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        // Capture button elements so the handler can flash them on shortcut use.
        let btn_undo = document.get_element_by_id("btn-undo");
        let btn_redo = document.get_element_by_id("btn-redo");
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            match e.key().as_str() {
                "z" | "Z" if e.ctrl_key() => {
                    e.prevent_default(); // suppress browser's own undo in editable fields
                    if e.shift_key() {
                        app.borrow_mut().redo();
                        if let Some(ref el) = btn_redo { flash_button(el); }
                    } else {
                        app.borrow_mut().undo();
                        if let Some(ref el) = btn_undo { flash_button(el); }
                    }
                }
                "Escape" => {
                    // Cancel any in-progress stroke — preview discarded, no undo entry.
                    let mut a = app.borrow_mut();
                    if a.is_drawing {
                        a.abort_stroke();
                    }
                }
                _ => {}
            }
        });
        window.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Wire the light/dark theme toggle button.
/// Flips `data-theme` on `<html>` so CSS variable rules re-evaluate automatically.
pub fn wire_theme_toggle(document: &Document, app: &Rc<RefCell<App>>) {
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

/// Wire the blend mode fly-out control.
///
/// Interaction: mousedown on `#mode-btn` opens the dropdown; the user drags to
/// a tile; mouseup on a tile selects that mode. Mouseup anywhere else dismisses
/// without changing mode. No ESC handling — the window mouseup always closes it.
pub fn wire_blend_mode(document: &Document, app: &Rc<RefCell<App>>) {
    let mode_btn = match document.get_element_by_id("mode-btn") {
        Some(el) => el,
        None => return,
    };

    // mousedown on the mode tile → show the fly-out
    {
        let app = Rc::clone(app);
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            e.prevent_default(); // suppress text-selection during drag
            app.borrow_mut().mode_dropdown_open = true;
            if let Some(dd) = doc_clone.get_element_by_id("mode-dropdown") {
                dd.class_list().add_1("open").unwrap();
            }
        });
        mode_btn
            .add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // window mouseup → close dropdown and commit selection if over a mode tile.
    // Coexists with the wire_grid_mouse window mouseup; each checks its own flag.
    {
        let window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let mut a = app.borrow_mut();
            if !a.mode_dropdown_open {
                return;
            }
            a.mode_dropdown_open = false;

            // Hide dropdown regardless of where the mouse was released
            if let Some(dd) = doc_clone.get_element_by_id("mode-dropdown") {
                dd.class_list().remove_1("open").unwrap();
            }

            // Check if the release landed on a mode tile (or a child of one)
            let target: Option<Element> = e.target().and_then(|t| t.dyn_into().ok());
            let tile = target.and_then(|el| {
                if el.class_list().contains("mode-tile") {
                    Some(el)
                } else {
                    el.closest(".mode-tile").ok().flatten()
                }
            });

            if let Some(tile_el) = tile {
                let mode_str = tile_el.get_attribute("data-mode").unwrap_or_default();
                if let Some(mode) = BlendMode::from_data_attr(&mode_str) {
                    a.blend_mode = mode;

                    // Update the mode button icon so it always shows the active mode
                    if let Some(btn) = doc_clone.get_element_by_id("mode-btn") {
                        btn.set_text_content(Some(mode.icon()));
                        btn.set_attribute(
                            "title",
                            &format!("Blend mode: {}", mode_str),
                        )
                        .unwrap();
                    }

                    // Move `selected` highlight to the newly chosen blend mode tile.
                    // Scoped to [data-mode] so line-mode tiles are not affected.
                    let tiles = doc_clone.query_selector_all("[data-mode]").unwrap();
                    for i in 0..tiles.length() {
                        let t: Element = tiles.item(i).unwrap().dyn_into().unwrap();
                        t.class_list().remove_1("selected").unwrap();
                    }
                    tile_el.class_list().add_1("selected").unwrap();
                }
            }
        });
        window
            .add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

/// Wire the ⧉ copy button to copy the full canvas as plain text to the clipboard.
/// Clipboard write is async (browser API returns a Promise); spawn_local bridges
/// Rust async/await to the JS microtask queue without blocking.
/// On failure (e.g. permission denied) the button flashes red briefly.
///
/// navigator.clipboard is accessed via js_sys::Reflect rather than web-sys typed
/// bindings, since the Clipboard interface is not available as a web-sys feature
/// in the version pinned by this toolchain.
pub fn wire_copy(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("btn-copy") {
        Some(el) => el,
        None => return,
    };

    let app      = Rc::clone(app);
    let app_kb   = Rc::clone(&app); // separate clone for the keyboard handler below
    let btn_copy = btn.clone();

    let cb = Closure::<dyn FnMut()>::new(move || {
        // Borrow app only long enough to build the text — dropped before spawn_local.
        // If a selection is active, copy only that region; otherwise copy the full canvas.
        let text = {
            let a = app.borrow();
            a.selected_text().unwrap_or_else(|| a.canvas_text())
        };

        // Flash immediately — feedback should feel instant, not wait for the Promise.
        flash_button(&btn_copy);

        let btn = btn_copy.clone();
        spawn_local(async move {
            let result = (|| -> Result<js_sys::Promise, wasm_bindgen::JsValue> {
                let window    = web_sys::window().unwrap();
                let nav       = js_sys::Reflect::get(&window, &"navigator".into())?;
                let clipboard = js_sys::Reflect::get(&nav, &"clipboard".into())?;
                let write_fn  = js_sys::Reflect::get(&clipboard, &"writeText".into())?;
                let write_fn: js_sys::Function = write_fn.dyn_into()?;
                let promise   = write_fn.call1(&clipboard, &text.into())?;
                promise.dyn_into::<js_sys::Promise>()
            })();

            match result {
                Ok(promise) => {
                    if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                        flash_button_error(&btn);
                    }
                }
                Err(_) => flash_button_error(&btn), // clipboard API not available
            }
        });
    });

    btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();

    // Ctrl+C keyboard shortcut — fires the same copy logic as the button click.
    // Listening on window so focus doesn't need to be on any particular element.
    {
        let window: Window = web_sys::window().unwrap();
        let app      = app_kb.clone();
        let btn_copy = document.get_element_by_id("btn-copy");
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            if e.key().as_str() == "c" && e.ctrl_key() && !e.shift_key() {
                e.prevent_default();
                let text = {
                    let a = app.borrow();
                    a.selected_text().unwrap_or_else(|| a.canvas_text())
                };
                if let Some(ref btn) = btn_copy { flash_button(btn); }
                let btn = btn_copy.clone();
                spawn_local(async move {
                    let result = (|| -> Result<js_sys::Promise, wasm_bindgen::JsValue> {
                        let window    = web_sys::window().unwrap();
                        let nav       = js_sys::Reflect::get(&window, &"navigator".into())?;
                        let clipboard = js_sys::Reflect::get(&nav, &"clipboard".into())?;
                        let write_fn  = js_sys::Reflect::get(&clipboard, &"writeText".into())?;
                        let write_fn: js_sys::Function = write_fn.dyn_into()?;
                        let promise   = write_fn.call1(&clipboard, &text.into())?;
                        promise.dyn_into::<js_sys::Promise>()
                    })();
                    match result {
                        Ok(promise) => {
                            if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                                if let Some(ref btn) = btn { flash_button_error(btn); }
                            }
                        }
                        Err(_) => { if let Some(ref btn) = btn { flash_button_error(btn); } }
                    }
                });
            }
        });
        window.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Wire the line mode fly-out (╲ / 📐 tile in the toolbar).
/// Wire the line tool button with long-press mode selection.
/// Quick click → activate Tool::Line. Hold 400ms → open the line mode fly-out.
/// Window mouseup commits the chosen mode tile and updates the button icon.
pub fn wire_line_tool(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("line-tool-btn") {
        Some(el) => el,
        None => return,
    };

    let app_click = Rc::clone(app);
    let app_lp    = Rc::clone(app);
    let doc_click = document.clone();
    let doc_lp    = document.clone();
    let btn_click = btn.clone();

    wire_long_press(
        &btn,
        // on_click: activate the line tool, move .active class
        move || {
            {
                let mut a = app_click.borrow_mut();
                a.clear_selection();
                a.tool = Tool::Line;
            }
            let all = doc_click.query_selector_all(".tool").unwrap();
            for j in 0..all.length() {
                let t: Element = all.item(j).unwrap().dyn_into().unwrap();
                t.class_list().remove_1("active").unwrap();
            }
            btn_click.class_list().add_1("active").unwrap();
        },
        // on_long_press: open the mode fly-out
        move || {
            app_lp.borrow_mut().line_mode_dropdown_open = true;
            if let Some(dd) = doc_lp.get_element_by_id("line-mode-dropdown") {
                dd.class_list().add_1("open").unwrap();
            }
        },
    );

    // window mouseup → close dropdown and commit the chosen mode tile.
    // If the user released over a tile: set mode, activate line tool, update icon.
    // If the user dragged off without landing on a tile: dismiss without activating.
    {
        let window    = web_sys::window().unwrap();
        let app       = Rc::clone(app);
        let doc_clone = document.clone();
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            {
                let mut a = app.borrow_mut();
                if !a.line_mode_dropdown_open { return; }
                a.line_mode_dropdown_open = false;
            } // release borrow before DOM work

            if let Some(dd) = doc_clone.get_element_by_id("line-mode-dropdown") {
                dd.class_list().remove_1("open").unwrap();
            }

            let target: Option<Element> = e.target().and_then(|t| t.dyn_into().ok());
            let tile = target.and_then(|el| {
                if el.get_attribute("data-line-mode").is_some() {
                    Some(el)
                } else {
                    el.closest("[data-line-mode]").ok().flatten()
                }
            });

            if let Some(tile_el) = tile {
                let mode_str = tile_el.get_attribute("data-line-mode").unwrap_or_default();
                if let Some(mode) = LineMode::from_data_attr(&mode_str) {
                    // Commit mode and activate the line tool (long-press + tile = tool select).
                    {
                        let mut a = app.borrow_mut();
                        a.line_mode = mode;
                        a.clear_selection();
                        a.tool = Tool::Line;
                    }

                    // Move .active to the line tool button
                    let all = doc_clone.query_selector_all(".tool").unwrap();
                    for i in 0..all.length() {
                        let t: Element = all.item(i).unwrap().dyn_into().unwrap();
                        t.class_list().remove_1("active").unwrap();
                    }
                    btn_clone.class_list().add_1("active").unwrap();

                    // Update button icon and title to reflect the chosen mode
                    btn_clone.set_text_content(Some(mode.icon()));
                    btn_clone
                        .set_attribute("title", &format!("Line ({}) — hold for mode", mode.icon()))
                        .unwrap();

                    // Move `selected` highlight to the chosen tile
                    let tiles = doc_clone.query_selector_all("[data-line-mode]").unwrap();
                    for i in 0..tiles.length() {
                        let t: Element = tiles.item(i).unwrap().dyn_into().unwrap();
                        t.class_list().remove_1("selected").unwrap();
                    }
                    tile_el.class_list().add_1("selected").unwrap();
                }
            }
            // No tile → user dragged off without selecting; line tool stays inactive.
        });
        window
            .add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

/// Wire single-finger drawing and two-finger pan/zoom to `#grid`.
///
/// One finger: mirrors the mouse handlers — touchstart/move/end map to
/// mousedown/move/up and drive the same App painting methods.
///
/// Two fingers: any in-progress single-finger stroke is aborted; the gesture
/// becomes pan + pinch-zoom. Midpoint movement scrolls `#canvas-wrap`;
/// finger-spread change scales `#grid` font-size (clamped 8–48 px). Scroll
/// is corrected each frame so the content under the pinch midpoint stays
/// stationary as zoom changes.
///
/// After a two-finger gesture, drawing is suppressed until all fingers lift,
/// preventing accidental strokes as the second finger leaves the screen.
///
/// CSS `touch-action: none` on `#grid` (set in index.html) tells the browser
/// to skip its own scroll/zoom handling so these handlers receive every touch.
pub fn wire_touch(document: &Document, app: &Rc<RefCell<App>>) {
    let grid_el = document.get_element_by_id("grid").unwrap();
    let wrap_el = document.get_element_by_id("canvas-wrap").unwrap();

    // ── touchstart ───────────────────────────────────────────────────────────
    {
        let app = Rc::clone(app);
        let doc = document.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            let touches = e.touches();
            match touches.length() {
                1 => {
                    let mut a = app.borrow_mut();
                    if a.is_two_finger {
                        // One finger touching down while recovering from a two-finger
                        // gesture — suppress until the screen is fully clear.
                        return;
                    }
                    let t = touches.get(0).unwrap();
                    if let Some((col, row)) = cell_from_coords(
                        t.client_x() as f64, t.client_y() as f64, &doc,
                    ) {
                        if a.tool != Tool::Select {
                            a.push_undo_snapshot();
                        }
                        a.is_drawing        = true;
                        a.draw_start        = Some((col, row));
                        a.locked_axis       = None;
                        a.last_painted_cell = None;
                        a.paint_stroke_to(col, row, false);
                    }
                }
                2 => {
                    let mut a = app.borrow_mut();
                    if a.is_drawing {
                        a.abort_stroke(); // cancel any in-progress single-finger stroke
                    }
                    a.is_two_finger = true;
                    let t0 = touches.get(0).unwrap();
                    let t1 = touches.get(1).unwrap();
                    let (x0, y0) = (t0.client_x() as f64, t0.client_y() as f64);
                    let (x1, y1) = (t1.client_x() as f64, t1.client_y() as f64);
                    let dx = x1 - x0;
                    let dy = y1 - y0;
                    a.pinch_start_dist      = (dx * dx + dy * dy).sqrt();
                    a.pinch_start_font_size = a.font_size;
                    a.pan_last_mid          = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                }
                _ => {}
            }
        });
        grid_el
            .add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // ── touchmove ────────────────────────────────────────────────────────────
    {
        let app  = Rc::clone(app);
        let doc  = document.clone();
        let wrap = wrap_el.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            let touches = e.touches();
            match touches.length() {
                1 => {
                    let mut a = app.borrow_mut();
                    if !a.is_drawing { return; }
                    let t = touches.get(0).unwrap();
                    if let Some((col, row)) = cell_from_coords(
                        t.client_x() as f64, t.client_y() as f64, &doc,
                    ) {
                        a.paint_stroke_to(col, row, false);
                    }
                }
                2 => {
                    let t0 = touches.get(0).unwrap();
                    let t1 = touches.get(1).unwrap();
                    let (x0, y0) = (t0.client_x() as f64, t0.client_y() as f64);
                    let (x1, y1) = (t1.client_x() as f64, t1.client_y() as f64);
                    let dx       = x1 - x0;
                    let dy       = y1 - y0;
                    let cur_dist = (dx * dx + dy * dy).sqrt();
                    let mid_x    = (x0 + x1) / 2.0;
                    let mid_y    = (y0 + y1) / 2.0;

                    // Short immutable borrow: read pinch/pan state, compute deltas.
                    let new_font: f64;
                    let inc_scale: f64;
                    let pan_dx: f64;
                    let pan_dy: f64;
                    {
                        let a = app.borrow();
                        if !a.is_two_finger { return; }
                        let ratio = if a.pinch_start_dist > 0.0 {
                            cur_dist / a.pinch_start_dist
                        } else {
                            1.0
                        };
                        new_font  = (a.pinch_start_font_size * ratio).max(8.0).min(48.0);
                        inc_scale = new_font / a.font_size; // incremental scale this frame
                        pan_dx    = mid_x - a.pan_last_mid.0;
                        pan_dy    = mid_y - a.pan_last_mid.1;
                    }

                    // Scroll correction: keep the content point under the pinch midpoint
                    // stationary as zoom changes, then shift by the pan delta.
                    // mid_vp = midpoint relative to canvas-wrap's top-left corner.
                    let wrap_rect = wrap.get_bounding_client_rect();
                    let mid_vp_x  = mid_x - wrap_rect.left();
                    let mid_vp_y  = mid_y - wrap_rect.top();
                    let old_sl    = wrap.scroll_left() as f64;
                    let old_st    = wrap.scroll_top()  as f64;
                    let new_sl = (old_sl * inc_scale + mid_vp_x * (inc_scale - 1.0) - pan_dx).max(0.0);
                    let new_st = (old_st * inc_scale + mid_vp_y * (inc_scale - 1.0) - pan_dy).max(0.0);

                    {
                        let mut a = app.borrow_mut();
                        a.set_font_size(new_font);
                        a.pan_last_mid = (mid_x, mid_y);
                    }

                    wrap.set_scroll_left(new_sl.round() as i32);
                    wrap.set_scroll_top(new_st.round()  as i32);
                }
                _ => {}
            }
        });
        grid_el
            .add_event_listener_with_callback("touchmove", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // ── touchend ─────────────────────────────────────────────────────────────
    // On window so releases anywhere on the page are caught, consistent with
    // how the mouse-up handler is wired.
    {
        let window = web_sys::window().unwrap();
        let app    = Rc::clone(app);
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            if e.touches().length() == 0 {
                // All fingers off — commit any stroke and exit two-finger guard.
                let mut a = app.borrow_mut();
                if a.is_drawing {
                    a.commit_stroke();
                }
                a.is_two_finger = false;
            }
            // One finger remaining after two-finger gesture: keep is_two_finger
            // true so the lingering finger doesn't accidentally start a stroke.
        });
        window
            .add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // ── touchcancel ──────────────────────────────────────────────────────────
    // Fired when the OS interrupts a touch (incoming call, system gesture, etc.).
    // Abort rather than commit so the interrupted stroke doesn't pollute the canvas.
    {
        let window = web_sys::window().unwrap();
        let app    = Rc::clone(app);
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |_e: TouchEvent| {
            let mut a = app.borrow_mut();
            if a.is_drawing {
                a.abort_stroke();
            }
            a.is_two_finger = false;
        });
        window
            .add_event_listener_with_callback("touchcancel", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}
