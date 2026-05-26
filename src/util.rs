//! Pure helper functions with no application state.
//! These are independent of App, Grid, and Tool — safe to call from anywhere.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, MouseEvent};

// ── Bresenham's line algorithm ────────────────────────────────────────────────

/// Return every (col, row) cell on the straight line from (c0,r0) to (c1,r1),
/// inclusive of both endpoints. Used to interpolate between consecutive
/// mousemove positions so fast strokes don't leave gaps.
pub fn bresenham(c0: usize, r0: usize, c1: usize, r1: usize) -> Vec<(usize, usize)> {
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
pub fn cell_from_mouse_event(e: &MouseEvent) -> Option<(usize, usize)> {
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

/// Find the grid cell at viewport coordinates `(x, y)` using the live DOM.
/// Used by touch handlers where no MouseEvent is available.
/// Mirrors `cell_from_mouse_event` but accepts raw coordinates.
pub fn cell_from_coords(x: f64, y: f64, document: &Document) -> Option<(usize, usize)> {
    let el = document.element_from_point(x as f32, y as f32)?;
    let cell_el = if el.class_list().contains("cell") {
        el
    } else {
        el.closest(".cell").ok()??
    };
    let col: usize = cell_el.get_attribute("data-col")?.parse().ok()?;
    let row: usize = cell_el.get_attribute("data-row")?.parse().ok()?;
    Some((col, row))
}

// ── Long-press wiring ─────────────────────────────────────────────────────────

/// Attach long-press behaviour to a button element:
/// - Quick release (< 400 ms): calls `on_click`
/// - Held ≥ 400 ms: calls `on_long_press` (mode fly-out, etc.)
/// - mouseleave while held: cancels the timer without firing either callback
///
/// This is the standard Photoshop-style tool-mode-picker UX: a tap selects
/// the tool, a hold opens its mode menu. Call this instead of wiring a plain
/// click listener for any tool that has sub-modes.
pub fn wire_long_press<F1, F2>(btn: &Element, on_click: F1, on_long_press: F2)
where
    F1: Fn() + 'static,
    F2: Fn() + 'static,
{
    let timer_id    = Rc::new(Cell::new(None::<i32>));
    let on_click    = Rc::new(on_click);
    let on_long_press = Rc::new(on_long_press);

    // mousedown — start the 400 ms long-press timer
    {
        let timer_id      = Rc::clone(&timer_id);
        let on_long_press = Rc::clone(&on_long_press);

        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            e.prevent_default();
            let timer_id_cb   = Rc::clone(&timer_id);
            let on_long_press = Rc::clone(&on_long_press);

            let timer_cb = Closure::once(move || {
                timer_id_cb.set(None); // mark as fired before calling handler
                on_long_press();
            });
            let id = web_sys::window()
                .unwrap()
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    timer_cb.as_ref().unchecked_ref(),
                    400,
                )
                .unwrap();
            timer_cb.forget();
            timer_id.set(Some(id));
        });
        btn.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mouseup — timer still pending means quick click: cancel timer, fire on_click
    {
        let timer_id = Rc::clone(&timer_id);

        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |_e: MouseEvent| {
            if let Some(id) = timer_id.get() {
                web_sys::window().unwrap().clear_timeout_with_handle(id);
                timer_id.set(None);
                on_click();
            }
            // If timer already fired (id is None), the mode menu is showing —
            // the window mouseup handler in the calling wire_* fn handles it.
        });
        btn.add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // mouseleave — cancel the timer if the cursor drifts off before 400 ms
    {
        let timer_id = Rc::clone(&timer_id);

        let cb = Closure::<dyn FnMut()>::new(move || {
            if let Some(id) = timer_id.get() {
                web_sys::window().unwrap().clear_timeout_with_handle(id);
                timer_id.set(None);
            }
        });
        btn.add_event_listener_with_callback("mouseleave", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }
}

// ── UI feedback helpers ───────────────────────────────────────────────────────

/// Briefly highlight a button to signal that its action or shortcut fired.
/// Mimics the classic Mac behaviour of flashing a menu item when its keyboard
/// equivalent is used, reinforcing the connection between key and visible control.
pub fn flash_button(el: &Element) {
    flash(el, "flash");
}

/// Briefly highlight a button in red to signal that its action failed
/// (e.g. clipboard permission denied). Same timing as flash_button.
pub fn flash_button_error(el: &Element) {
    flash(el, "flash-error");
}

fn flash(el: &Element, class: &'static str) {
    el.class_list().add_1(class).unwrap();
    let el = el.clone();
    // One-shot closure: removes the class after the flash duration and then drops itself.
    let cb = Closure::once(move || {
        el.class_list().remove_1(class).unwrap();
    });
    web_sys::window()
        .unwrap()
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            180, // ms — long enough to see, short enough to feel instant
        )
        .unwrap();
    cb.forget();
}
