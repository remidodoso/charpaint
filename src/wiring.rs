//! Wiring — connects DOM elements to App state via browser event listeners.
//!
//! Each `wire_*` function attaches one or more listeners and `.forget()`s the
//! closures so they live for the page lifetime. No business logic lives here;
//! the functions are pure patch cables between browser events and App methods.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Document, DragEvent, Element, Event, KeyboardEvent, MouseEvent, NodeList, PointerEvent, TouchEvent, Window};

use wasm_bindgen_futures::spawn_local;

use crate::{App, Tool};
use crate::util::{cell_from_coords, cell_from_mouse_event, flash_button, flash_button_error};

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
                a.clear_demo_if_active(); // wipe intro content before first undo snapshot
                // Text tool: start (or move) the text session; no stroke state needed.
                if a.tool == Tool::Text {
                    a.start_text_session(col, row);
                    return;
                }
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

        // Line and Fill have sub-modes and tap-to-cycle; wired separately.
        if tool == Tool::Line || tool == Tool::Fill { continue; }

        let app       = Rc::clone(app);
        let el_clone  = el.clone();
        let doc_clone = document.clone();

        let cb = Closure::<dyn FnMut()>::new(move || {
            {
                let mut a = app.borrow_mut();
                a.commit_text_session(); // commit any open text entry before switching tools
                a.clear_selection();     // switching tools always drops any active selection
                a.tool = tool;
            }

            // Move the `active` CSS class to the clicked button.
            // Only sweep [data-tool] elements — shift-toggle and mode-btn use .tool
            // for styling but are independent toggles, not tool-selector buttons.
            let all = doc_clone.query_selector_all("[data-tool]").unwrap();
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

    // Keyboard: Ctrl+Z → undo, Shift+Ctrl+Z → redo, Escape → abort stroke or text.
    // Text session keys (printable chars, Backspace, Enter) are intercepted first.
    // Listening on window so it works regardless of which element has focus.
    {
        let window: Window = web_sys::window().unwrap();
        let app = Rc::clone(app);
        // Capture button elements so the handler can flash them on shortcut use.
        let btn_undo = document.get_element_by_id("btn-undo");
        let btn_redo = document.get_element_by_id("btn-redo");
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let key = e.key();

            // Text session: intercept typing before standard shortcuts.
            {
                let text_active = app.borrow().text_origin.is_some();
                if text_active {
                    if key == "Escape" {
                        // ESC during text: discard typed chars, cursor returns to origin.
                        app.borrow_mut().abort_text_session();
                        return;
                    }
                    if key == "Enter" {
                        // Enter: commit typed text, end session.
                        app.borrow_mut().commit_text_session();
                        return;
                    }
                    if key == "Backspace" && !e.ctrl_key() && !e.alt_key() {
                        e.prevent_default();
                        app.borrow_mut().text_backspace();
                        return;
                    }
                    // Single printable character — route to text input.
                    if key.chars().count() == 1 && !e.ctrl_key() && !e.alt_key() {
                        e.prevent_default();
                        let ch = key.chars().next().unwrap();
                        app.borrow_mut().type_char(ch);
                        return;
                    }
                }
            }

            // Standard global shortcuts.
            if (key == "z" || key == "Z") && e.ctrl_key() {
                e.prevent_default(); // suppress browser's own undo in editable fields
                if e.shift_key() {
                    app.borrow_mut().redo();
                    if let Some(ref el) = btn_redo { flash_button(el); }
                } else {
                    app.borrow_mut().undo();
                    if let Some(ref el) = btn_undo { flash_button(el); }
                }
            } else if key == "Escape" {
                // Cancel any in-progress stroke — preview discarded, no undo entry.
                let mut a = app.borrow_mut();
                if a.is_drawing {
                    a.abort_stroke();
                }
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

/// Wire the `#mode-btn` blend-mode button.
///
/// Tap-to-cycle: each tap advances to the next implemented blend mode
/// (Overwrite → Stamp → Overwrite). The button icon and title update to reflect
/// the new mode. The mode button is never "selected" — it has no active state.
///
/// Three listeners — same touch/mouse pattern used by `wire_shift_toggle`:
///   touchstart — preventDefault to suppress synthetic mouse event chain.
///   touchend   — cycle for touch.
///   mousedown  — cycle for desktop mouse, with coordinate guard.
pub fn wire_blend_mode(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("mode-btn") {
        Some(el) => el,
        None => return,
    };

    // touchstart — suppress synthetic mouse events from this button's tap.
    {
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            e.prevent_default();
        });
        btn.add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // touchend — cycle blend mode for touch devices.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |_e: TouchEvent| {
            let mode = {
                let mut a = app.borrow_mut();
                a.blend_mode = a.blend_mode.cycle();
                a.blend_mode
            };
            btn_clone.set_text_content(Some(mode.icon()));
            btn_clone.set_attribute("title", &format!("Blend mode: {}", mode.name())).unwrap();
        });
        btn.add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mousedown — cycle blend mode for desktop mouse.
    // Coordinate guard rejects rerouted synthetic events from Firefox Android.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let rect = btn_clone.get_bounding_client_rect();
            let x = e.client_x() as f64;
            let y = e.client_y() as f64;
            if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() {
                return;
            }
            let mode = {
                let mut a = app.borrow_mut();
                a.blend_mode = a.blend_mode.cycle();
                a.blend_mode
            };
            btn_clone.set_text_content(Some(mode.icon()));
            btn_clone.set_attribute("title", &format!("Blend mode: {}", mode.name())).unwrap();
        });
        btn.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref()).unwrap();
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

/// Wire the `#line-tool-btn` line-tool button.
///
/// Tap-to-select, tap-again-to-cycle:
///   • First tap when the line tool is not active: activates the line tool.
///   • Second tap when already active: cycles LineMode (Character → Art → Character)
///     and updates the button icon. The tool stays active.
///
/// Three listeners — same touch/mouse pattern used by `wire_shift_toggle`:
///   touchstart — preventDefault to suppress synthetic mouse event chain.
///   touchend   — select-or-cycle for touch.
///   mousedown  — select-or-cycle for desktop mouse, with coordinate guard.
pub fn wire_line_tool(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("line-tool-btn") {
        Some(el) => el,
        None => return,
    };

    // touchstart — suppress synthetic mouse events from this button's tap.
    {
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            e.prevent_default();
        });
        btn.add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // touchend — select-or-cycle for touch devices.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |_e: TouchEvent| {
            line_tool_tap(&app, &btn_clone, &doc_clone);
        });
        btn.add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mousedown — select-or-cycle for desktop mouse.
    // Coordinate guard rejects rerouted synthetic events from Firefox Android.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let rect = btn_clone.get_bounding_client_rect();
            let x = e.client_x() as f64;
            let y = e.client_y() as f64;
            if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() {
                return;
            }
            line_tool_tap(&app, &btn_clone, &doc_clone);
        });
        btn.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Shared action for touch and mouse line-tool taps.
/// First tap (tool inactive): selects the line tool and moves `.active`.
/// Second tap (tool already active): cycles LineMode and updates icon/title.
fn line_tool_tap(app: &Rc<RefCell<App>>, btn: &Element, document: &Document) {
    let already_active = app.borrow().tool == Tool::Line;

    if already_active {
        let mode = {
            let mut a = app.borrow_mut();
            a.line_mode = a.line_mode.cycle();
            a.line_mode
        };
        btn.set_text_content(Some(mode.icon()));
        btn.set_attribute("title", &format!("Line ({}) — tap again to cycle mode", mode.icon())).unwrap();
    } else {
        {
            let mut a = app.borrow_mut();
            a.commit_text_session(); // commit any open text entry before switching to line tool
            a.clear_selection();
            a.tool = Tool::Line;
        }
        let all = document.query_selector_all("[data-tool]").unwrap();
        for j in 0..all.length() {
            let t: Element = all.item(j).unwrap().dyn_into().unwrap();
            t.class_list().remove_1("active").unwrap();
        }
        btn.class_list().add_1("active").unwrap();
    }
}

/// Wire the ⌧ clear button.
/// Clears the active selection's content if one exists, otherwise clears the full
/// canvas. Always undoable — a snapshot is pushed before any change is made.
pub fn wire_clear(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("btn-clear") {
        Some(el) => el,
        None => return,
    };
    let app = Rc::clone(app);
    let cb = Closure::<dyn FnMut()>::new(move || {
        app.borrow_mut().clear_canvas();
    });
    btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
    cb.forget();
}

/// Wire the ⇧ shift-lock toggle button.
/// Tapping it toggles `app.shift_locked`, which ORs with the physical Shift key
/// in `resolve_target` so axis constraint works on touch devices without a keyboard.
///
/// Three listeners rather than one `click`:
///   touchstart — calls preventDefault() so the browser does not synthesise a
///                mousedown+click after the tap (which would double-fire the toggle).
///   touchend   — fires the toggle for touch devices.
///   mousedown  — fires the toggle for desktop mouse.
///
/// `click` is intentionally NOT used. Firefox Android reroutes synthetic click
/// events from touches on non-interactive areas (blank canvas, etc.) to the
/// nearest element that has a `click` listener. Removing the `click` listener
/// from this button eliminates it as a rerouting target entirely.
pub fn wire_shift_toggle(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("shift-toggle") {
        Some(el) => el,
        None => return,
    };

    // touchstart — suppress synthetic mouse event chain from this button's tap.
    {
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            e.prevent_default();
        });
        btn.add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // touchend — toggle for touch devices.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |_e: TouchEvent| {
            let mut a = app.borrow_mut();
            a.shift_locked = !a.shift_locked;
            if a.shift_locked {
                btn_clone.class_list().add_1("active").unwrap();
            } else {
                btn_clone.class_list().remove_1("active").unwrap();
            }
        });
        btn.add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mousedown — toggle for desktop mouse.
    // Guard: check that the event coordinates actually land inside the button.
    // Firefox Android reroutes synthetic mousedown events (manufactured from canvas
    // touches) to the nearest element with a mousedown listener. The rerouted event
    // carries the touch's original coordinates — outside this button — so the check
    // rejects it. A genuine mouse press will always have coordinates inside the button.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let rect = btn_clone.get_bounding_client_rect();
            let x = e.client_x() as f64;
            let y = e.client_y() as f64;
            if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() {
                return; // rerouted synthetic event — coordinates aren't on this button
            }
            let mut a = app.borrow_mut();
            a.shift_locked = !a.shift_locked;
            if a.shift_locked {
                btn_clone.class_list().add_1("active").unwrap();
            } else {
                btn_clone.class_list().remove_1("active").unwrap();
            }
        });
        btn.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref()).unwrap();
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
            // Suppress synthetic mouse/click events — we drive all drawing from
            // touch events directly. Without this, Firefox Android synthesises a
            // mousedown+mouseup+click chain after each touch and can reroute the
            // click to unrelated elements with click listeners (e.g. #shift-toggle).
            e.prevent_default();
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
                        a.clear_demo_if_active(); // wipe intro content before first undo snapshot
                        // Text tool: start/move session; start_text_session_at calls
                        // focus() to raise the mobile keyboard. touchstart is a user
                        // gesture so the browser honours the focus() call.
                        if a.tool == Tool::Text {
                            a.start_text_session(col, row);
                            return;
                        }
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

/// Wire the `#fill-tool-btn` fill-tool button.
///
/// Tap-to-select, tap-again-to-cycle:
///   • First tap when fill is not active: activates the fill tool.
///   • Second tap when already active: cycles FillMode (Flood4 ↔ Flood8)
///     and updates the button icon/title.
///
/// Same three-listener pattern as `wire_line_tool`.
pub fn wire_fill_tool(document: &Document, app: &Rc<RefCell<App>>) {
    let btn = match document.get_element_by_id("fill-tool-btn") {
        Some(el) => el,
        None => return,
    };

    // touchstart — suppress synthetic mouse events.
    {
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |e: TouchEvent| {
            e.prevent_default();
        });
        btn.add_event_listener_with_callback("touchstart", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // touchend — select-or-cycle for touch devices.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(TouchEvent)>::new(move |_e: TouchEvent| {
            fill_tool_tap(&app, &btn_clone, &doc_clone);
        });
        btn.add_event_listener_with_callback("touchend", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mousedown — select-or-cycle for desktop mouse, with coordinate guard.
    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let rect = btn_clone.get_bounding_client_rect();
            let x = e.client_x() as f64;
            let y = e.client_y() as f64;
            if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() {
                return;
            }
            fill_tool_tap(&app, &btn_clone, &doc_clone);
        });
        btn.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Shared action for touch and mouse fill-tool taps.
/// First tap (tool inactive): selects fill and moves `.active`.
/// Second tap (tool already active): cycles FillMode and updates icon/title.
fn fill_tool_tap(app: &Rc<RefCell<App>>, btn: &Element, document: &Document) {
    let already_active = app.borrow().tool == Tool::Fill;

    if already_active {
        let mode = {
            let mut a = app.borrow_mut();
            a.fill_mode = a.fill_mode.cycle();
            a.fill_mode
        };
        btn.set_text_content(Some(mode.icon()));
        btn.set_attribute("title", &format!("{} — tap again to cycle mode", mode.name())).unwrap();
    } else {
        {
            let mut a = app.borrow_mut();
            a.commit_text_session();
            a.clear_selection();
            a.tool = Tool::Fill;
        }
        // Sync icon/title to the current fill_mode (may have been cycled previously).
        let mode = app.borrow().fill_mode;
        btn.set_text_content(Some(mode.icon()));
        btn.set_attribute("title", &format!("{} — tap again to cycle mode", mode.name())).unwrap();
        let all = document.query_selector_all("[data-tool]").unwrap();
        for j in 0..all.length() {
            let t: Element = all.item(j).unwrap().dyn_into().unwrap();
            t.class_list().remove_1("active").unwrap();
        }
        btn.class_list().add_1("active").unwrap();
    }
}

/// Wire the hidden `#text-input` element to capture mobile virtual keyboard input.
///
/// When the text tool is active, Rust focuses this element (raising the keyboard).
/// Each `input` event means the user typed something — we read `.value()`, route
/// each character to `type_char()`, then clear the value so the next event is fresh.
///
/// Desktop typing is handled by the window `keydown` handler in `wire_undo_redo`,
/// which calls `e.prevent_default()` on printable keys — that suppresses the
/// `input` event on desktop, so there is no double-fire.
pub fn wire_text_input(document: &Document, app: &Rc<RefCell<App>>) {
    let el = match document.get_element_by_id("text-input") {
        Some(el) => el,
        None => return,
    };
    let app = Rc::clone(app);
    let cb = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        // Read and immediately clear the input value.
        // Each `input` event carries exactly the newly inserted text.
        let value = app.borrow().text_input_el.value();
        if value.is_empty() { return; }
        app.borrow().text_input_el.set_value("");
        for ch in value.chars() {
            app.borrow_mut().type_char(ch);
        }
    });
    el.add_event_listener_with_callback("input", cb.as_ref().unchecked_ref()).unwrap();
    cb.forget();
}

/// Wire the `#btn-help` toggle and `#help-overlay` pointer handlers.
///
/// Clicking `?` toggles help mode on/off (updating `app.help_mode`, the button's
/// active class, and overlay/popup visibility). The `?` button sits above the
/// overlay via CSS z-index so it remains clickable while help is active.
///
/// While help mode is on, the overlay captures:
///   pointermove — hover-to-explore on desktop: moves the help popup continuously.
///   pointerdown — tap-to-explore on mobile (and desktop click): same lookup.
///
/// For each event, `show_help_for_point` briefly sets `pointer-events: none` on
/// the overlay, calls `elementFromPoint` to discover the element underneath, then
/// restores the overlay. It walks up the DOM to find the nearest `data-help`
/// attribute and displays the matching YAML string in `#help-popup`.
pub fn wire_help(document: &Document, app: &Rc<RefCell<App>>) {
    // ── ? button: toggle help mode ────────────────────────────────────────────
    let btn = match document.get_element_by_id("btn-help") {
        Some(el) => el,
        None => return,
    };

    {
        let app       = Rc::clone(app);
        let btn_clone = btn.clone();
        let doc_clone = document.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            let help_on = app.borrow_mut().toggle_help_mode();
            if help_on {
                btn_clone.class_list().add_1("active").unwrap();
            } else {
                btn_clone.class_list().remove_1("active").unwrap();
            }
            set_help_visibility(&doc_clone, help_on);
        });
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // ── Overlay: discover element under pointer, show help ────────────────────
    let overlay = match document.get_element_by_id("help-overlay") {
        Some(el) => el,
        None => return,
    };

    // pointermove — continuous hover discovery on desktop.
    {
        let doc_clone     = document.clone();
        let overlay_clone = overlay.clone();
        let cb = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
            show_help_for_point(e.client_x() as f64, e.client_y() as f64, &doc_clone, &overlay_clone);
        });
        overlay.add_event_listener_with_callback("pointermove", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // pointerdown — tap discovery for mobile (also fires on desktop click).
    {
        let doc_clone     = document.clone();
        let overlay_clone = overlay.clone();
        let cb = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
            e.prevent_default(); // prevent synthetic mouse/scroll events on mobile
            show_help_for_point(e.client_x() as f64, e.client_y() as f64, &doc_clone, &overlay_clone);
        });
        overlay.add_event_listener_with_callback("pointerdown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

/// Show or hide the help overlay and popup together.
/// Called when help mode is toggled; on hide, clears the popup content.
fn set_help_visibility(document: &Document, visible: bool) {
    if let Some(overlay) = document.get_element_by_id("help-overlay") {
        let _ = overlay.set_attribute("style", if visible { "display: block" } else { "display: none" });
    }
    if let Some(popup) = document.get_element_by_id("help-popup") {
        let _ = popup.set_attribute("style", if visible { "display: block" } else { "display: none" });
        if visible {
            // Seed with the "move over something" prompt so the popup isn't blank.
            if let Some(el) = document.get_element_by_id("help-popup-key") {
                el.set_text_content(Some("—"));
            }
            if let Some(el) = document.get_element_by_id("help-popup-text") {
                el.set_text_content(Some("Hover over or tap any control to see what it does."));
            }
        }
    }
}

/// Briefly remove the overlay from hit-testing, discover the element at (x, y),
/// restore the overlay, then look up its `data-help` key and update the popup.
fn show_help_for_point(x: f64, y: f64, document: &Document, overlay: &Element) {
    // Step aside so elementFromPoint sees through us.
    let _ = overlay.set_attribute("style", "display: block; pointer-events: none");
    let found = document.element_from_point(x as f32, y as f32);
    let _ = overlay.set_attribute("style", "display: block; pointer-events: auto");

    let key_el  = document.get_element_by_id("help-popup-key");
    let text_el = document.get_element_by_id("help-popup-text");

    match found.and_then(|el| find_help_key(&el)) {
        Some(key) => {
            let text = crate::help(&key).unwrap_or("");
            if let Some(el) = key_el  { el.set_text_content(Some(&key)); }
            if let Some(el) = text_el { el.set_text_content(Some(text)); }
        }
        None => {
            if let Some(el) = key_el  { el.set_text_content(Some("—")); }
            if let Some(el) = text_el { el.set_text_content(Some("Hover over or tap any control to see what it does.")); }
        }
    }
}

/// Walk up the DOM from `el`, returning the first `data-help` attribute value found.
fn find_help_key(el: &Element) -> Option<String> {
    let mut current = Some(el.clone());
    while let Some(e) = current {
        if let Some(key) = e.get_attribute("data-help") {
            return Some(key);
        }
        current = e.parent_element();
    }
    None
}

/// Wire drag-and-drop of image files onto `#canvas-wrap`.
///
/// Dropping an image file (from the desktop or file manager) displays it as a
/// ghostly background behind the character grid. The image is shown at ~50%
/// opacity via a CSS gradient overlay layered over it on `#grid`.
///
/// Magnification is capped at 2×: if `background-size: cover` would scale the
/// image more than 2× its natural size, an explicit pixel size is used instead
/// and the image is centred, leaving canvas-bg colour at the edges.
///
/// Dropping a second image replaces the first (old blob URL is revoked).
/// To clear the background the user reloads the page; toolbar controls TBD.
pub fn wire_drag_drop(document: &Document, app: &Rc<RefCell<App>>) {
    let canvas_wrap = match document.get_element_by_id("canvas-wrap") {
        Some(el) => el,
        None => return,
    };

    // Window-level dragover + drop suppressors: without these, Firefox (and other
    // browsers) intercept any drag that lands outside #canvas-wrap and open the
    // file as a new tab. preventDefault() at the window level opts the whole page
    // out of that behaviour. The canvas-wrap drop handler below still fires for
    // drops on the canvas; everything else is silently swallowed here.
    {
        let window = web_sys::window().unwrap();
        let cb = Closure::<dyn FnMut(DragEvent)>::new(|e: DragEvent| { e.prevent_default(); });
        window.add_event_listener_with_callback("dragover", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
    {
        let window = web_sys::window().unwrap();
        let cb = Closure::<dyn FnMut(DragEvent)>::new(|e: DragEvent| { e.prevent_default(); });
        window.add_event_listener_with_callback("drop", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // dragover on canvas-wrap — required for the canvas-specific `drop` to fire.
    {
        let cb = Closure::<dyn FnMut(DragEvent)>::new(|e: DragEvent| {
            e.prevent_default();
        });
        canvas_wrap
            .add_event_listener_with_callback("dragover", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }

    // drop — read the file, create a blob URL, measure natural dimensions via a
    // temporary off-screen img element, then apply the background.
    {
        let app = Rc::clone(app);
        let cb = Closure::<dyn FnMut(DragEvent)>::new(move |e: DragEvent| {
            e.prevent_default();

            // Pull the first file out of the drag payload.
            let file = e
                .data_transfer()
                .and_then(|dt| dt.files())
                .and_then(|fl| fl.get(0));
            let file = match file {
                Some(f) => f,
                None    => return,
            };

            // Only accept image/* — reject text, PDFs, etc.
            if !file.type_().starts_with("image/") {
                return;
            }

            // Synchronously create a blob: URL; no FileReader round-trip needed.
            // The previous URL (if any) is revoked inside apply_bg_image.
            let url = match web_sys::Url::create_object_url_with_blob(file.as_ref()) {
                Ok(u)  => u,
                Err(_) => return,
            };

            // Temporary off-screen img element so we can read naturalWidth/Height.
            // It never enters the DOM — onload fires regardless.
            let img_el = match web_sys::HtmlImageElement::new() {
                Ok(el) => el,
                Err(_) => return,
            };

            let app_2 = Rc::clone(&app);
            let url_2 = url.clone();
            let img_2 = img_el.clone();

            // One-shot closure: process_bg_image owns the full pipeline —
            // pixel extraction, luma conversion, stretch, grayscale render,
            // data-URL creation, background display, and luma storage.
            let on_load = Closure::once_into_js(move || {
                app_2.borrow_mut().process_bg_image(&img_2, &url_2);
            });

            img_el
                .add_event_listener_with_callback("load", on_load.unchecked_ref())
                .unwrap();
            // Setting src after the listener is registered — guaranteed ordering.
            img_el.set_src(&url);
        });
        canvas_wrap
            .add_event_listener_with_callback("drop", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}
