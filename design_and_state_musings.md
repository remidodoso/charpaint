# Design & State Musings

Maintain a running notes file (`design_and_state_musings.md`) to capture design decisions, open questions, and state observations as work progresses.

---

## New project state & auto-BgMove

`is_new_project: bool` in App — true on first load, true after a future "New Project" button press, false once the user has meaningfully started work. Two things end the new-project state:
1. **First image loaded** — originally triggered auto-entry into BgMove only on first load. **Updated:** BgMove is now entered automatically on *every* image drop, not just the first — the user always wants to frame a new image. `is_new_project` no longer gates this; `process_bg_image` unconditionally calls `enter_bg_move()`.
2. **First canvas touch** — same hook as `clear_demo_if_active()`. Still resets `is_new_project` to false, which matters for the future "New Project" button.

**Implementation note:** the auto-BgMove call in `process_bg_image` must happen *after* `enable_bg_eye()`, not before — `enable_bg_eye` currently calls `accept_bg_move()` if BgMove is already active (designed for second-image drops), which would immediately undo an auto-entry that happened earlier in the same function.

**DOM class:** `enter_bg_move()` in App doesn't currently move the `.active` CSS class to the BgMove button (the wiring caller does that). Auto-entry needs App to do this itself — a small helper analogous to `restore_tool_active_class()` is needed.

**"New Project" button (NIY):** Planned for the menubar. On press: clear canvas, clear background, reset `is_new_project = true`, `demo_active = false` (new project starts blank, no demo), reset texture/pop to 0. The current "new project" moment is just page load; "New" button makes it repeatable without a reload.

---

## TODO

- **Landscape / nature drawing tools** — specialised stamps or generators for natural motifs: a "leaf" brush (oval-ish with a vein), a "shining sun" (circle with radiating lines, possibly using the art-line tool's logic for the rays), foliage clusters, etc. Note: winter landscapes (bare trees, snow) already emerge naturally from the art-line tool's `\` `/` `|` characters — these new tools would extend that into warmer seasons. Could be implemented as parameterised stamp patterns or as dedicated tool modes.

- **Nicer ovals / speech balloons** — the current oval tool uses integer Zingl-Bresenham which can look lumpy at small sizes. Investigate whether the character selection or the geometry algorithm can be improved for rounder-looking results. Speech balloons (oval + a "tail" pointing to a speaker) are a natural extension — the tail could be a short line or triangle drawn from a point on the oval perimeter toward a user-chosen direction.

- **Multi-character brushes / brush shapes** — paint with a stamp larger than a single cell; e.g. 2×2 block, diagonal slash, custom pattern.

- **Ink dropper** — click a cell on the canvas to pick up the character that's already there and make it the active brush character.

- **Rework character palette** — the current palette strip is a static list; needs a redesign (scrollable set, categories, user-customisable slots, etc.).

- **Ordered cell sequences in drawing tools** — every drawing tool (pencil, line, oval, rect, etc.) produces an ordered sequence of cells from start to end, with a consistent notion of "direction of drawing." This ordering should be a first-class concept available to all tools, so that character-assignment strategies (direction-based, sequential/rotating, 2D pattern fill, etc.) can be applied uniformly. Example patterns: "dot space dot space" along a line, `*-*-*-` along a stroke, or eventually 2D tiling patterns. Gap-filling via Bresenham is part of this: interpolated cells are ordered in the same direction as travel, so the sequence is always contiguous and unambiguous. The smart pencil and sequential character mode are the first consumers of this notion.

- **"Smart pencil" / freehand-to-line mode** *(partially implemented — see Implemented section)* — a pencil sub-mode that interprets freehand strokes on the fly and substitutes directional line characters: `|` `-` `_` `/` `\`, possibly `(` `)` and others for curves. The basic 4-direction version is working; `_`, `(`, `)` rounding cases and backtrack behaviour are not yet handled.

- **Background image tone controls** — adjustable parameters for brightness, contrast, edge blend, etc. that feed directly into the AA conversion pipeline (not just the visual overlay).

  **Pipeline stages (clarified):**
  ```
  original image
    → monochrome conversion  (channel weighting; see colour filter note below)
    → [store as luma_raw]
    → brightness / contrast adjustments   ← knobs live HERE
    → [store as luma_adjusted]
    → edge detection
    → [store as edge_map]
  ```
  Adjustments belong before edge detect because edge detection responds to gradients — contrast applied before it changes *which* edges are found, not just how bright the output looks. Contrast applied after edge detect is cosmetic only.

  **Recomputation:** the full pipeline (including monochrome conversion and downsample) only needs to run when a new image is loaded. Parameter adjustments only need to reprocess from `luma_raw` onward. If `luma_raw` is stored at cell resolution (~2000 values for an 80-column canvas), parameter changes are instantaneous. Storing at full image resolution and downsampling after adjustment would be higher quality but probably indistinguishable at cell scale — experiment to confirm, but cell-res storage is likely fine.

  **Colour filter presets** — rather than a plain luma conversion (standard weights ~0.21R + 0.72G + 0.07B), offer a small set of channel-weighting presets analogous to B&W photographic filters: Neutral, Yellow (lightens skin/foliage, good general-purpose), Red (high drama, dark skies), Green (texture emphasis), Blue (lightens sky, darkens skin). Single enum control, no complex UI. Note for later.

  Key design decisions settled in discussion:

  - **UI pattern:** a contextual panel that appears just below the toolbar whenever the BgMove (scroll) tool is active. Includes a dismiss `[×]` button to collapse it; it reappears the next time BgMove is entered. This keeps controls out of the way when not needed without burying them in a menu.
  - **No sliders** — each parameter uses ◀ value ▶ arrow buttons with discrete steps. Large tap targets, works on mobile, value is always readable. Rough layout: `Brightness  ◀  0  ▶     Contrast  ◀  1.0×  ▶     Edge blend  ◀  50%  ▶     [×]`
  - **Parameters (initial set):**
    - *Brightness* — "squish toward endpoint": brightening pivots at 255 and pulls all values toward it (`output = 255 - (255-input)×(1-b)`); darkening pivots at 0 (`output = input×(1-d)`). NOT a simple additive shift. The histogram compresses toward one end: values at the fixed endpoint are unchanged, values at the far end move the most — the "thinning" and "piling up" effect is real and visible. Does NOT meaningfully affect Sobel edge detection (gradients still scale linearly and enhance_edges normalises back), except in the pathological case where very dark images have gradients crushed near 0 that brightening can reveal. Primary uses: (1) making the visual reference easier to trace; (2) controlling the tonal range for luminance-based AA matching (Braille — brighter = fewer dots per cell, darker = more dots). Implemented as a single integer index with a zero crossing: negative = darken, positive = brighten, 0 = no change.
    - *Contrast* — global contrast (multiplicative around midpoint) also turns out to be useless for edge detection: Sobel measures local differences, scaling all values scales all gradients equally, and enhance_edges normalises them back. May still be marginally useful as a display/luminance-AA control but deprioritised.
    - *Local contrast / unsharp mask* — the correct way to improve edge detection quality. `output = clamp(input + amount × (input − blur(input)), 0, 255)`. Unlike global brightness/contrast, amplifies detail selectively by spatial scale; changes relative edge strengths in a way that normalisation cannot undo. Applied to `bg_luma` after brightness/contrast and before Sobel.
      - **Two separate fixed-radius controls** (no tweakable radius): "Texture" (small, ~10–11 processing pixels ≈ 3pt at canvas scale, good for strokes/hatching/fine detail) and "Pop" (large, ~40–60px ≈ one canvas cell, good for faces/silhouettes/coarse structure).
      - **Implementation**: box blur (sliding window average) is O(W×H) *regardless of radius* and is the right tool — no need for true Gaussian. The blur in the USM formula only needs to estimate local average at the target scale; the exact kernel shape (bell vs. flat rectangle) is irrelevant since you're subtracting it to get a detail signal that feeds into Sobel anyway. Single-pass box blur for texture radius is probably adequate. Two or three passes for pop radius (triangular/quasi-Gaussian kernel) avoids flat-topped halos at large radii. Each pass is still O(W×H) so 3-pass large-radius costs the same as 1-pass small-radius. Photoshop's "Gaussian Blur" filter is one of the very few tools that actually uses a true Gaussian; almost everything else uses stacked box blur or similar.
      - **Amount**: off / mild (0.5×) / strong (1.5–2×) discrete steps for each control, using ◀ ▶ arrows or similar. Default off.
      - **Implemented, then sidelined.** Texture (radius ~10px) and Pop (radius ~45px) USM controls were built and exposed as ◀▶ buttons in the image-controls strip, but were removed after finding the effect underwhelming. Code preserved in `asciiart.rs` with `#[allow(dead_code)]`. The pre-blur in `reprocess_edges_for_scale` now handles scale-appropriate smoothing more principally — see the Scale-adaptive edge detection note.
    - *Edge blend* — edge-vs-luminance mix for the AA brush (see AA source signal note); steps of 10%, 0–100%.
    - Later candidates: *edge threshold*, *pre-blur radius*.
  - **Pipeline implication:** processing is currently one-shot at image load. Live tweaking requires storing the raw luma data (`bg_luma_raw`) before any brightness/contrast transform, so control changes re-derive the processed maps from the original rather than reloading the image.
  - **Blended preview as a natural mode:** touching the edge blend control should switch the background preview to the blended result (luma × (1−blend) + edges × blend) and leave it there until the user manually cycles away. This keeps what-you-see and what-the-AA-brush-reads in sync — if the preview stays on pure-edge while you've dialled in a 50% blend, the brush paints characters that don't match the display. Brightness/contrast adjustments should similarly update the active preview. Long-term observation: at blend=0% the blended preview is identical to the luma preview, and at blend=100% it is the edge preview — the existing two modes may eventually collapse into special cases of the blend control rather than being maintained separately.

- **AA source signal: edge vs. luminance vs. blend** — currently the AA brush uses only the edge-detected image, which suits Ascii7 well (ASCII characters at normal density read as lines and edges). But Braille is a different beast: with 6–8 dots per cell it's essentially a halftone system capable of representing a full tonal range, and edge detection discards exactly the tonal information braille is good at encoding. The pipeline should store *both* the raw luma map and the edge map, then blend them at paint time with a controllable mix parameter (0 = pure luminance / halftone, 1 = pure edge / contour, somewhere in between = both). This mix parameter is also a natural candidate for a UI slider alongside the tone controls above. Architecturally the implication is that both maps need to survive past the initial `process_bg_image` call so they're available per brush stroke. **All the specific decisions here (what gets derived from what, independent vs. coupled controls) are experiment-and-heuristics territory — no point over-designing in advance.**

- **AA feature extraction: structural line detection** — rather than treating the image as a single homogeneous signal, optionally run structural feature detectors as a pre-pass and render those features with "correct" characters independently of the main AA brush:
  - *Horizontal/vertical line detection* — find strong H/V edges and render them with `—` `|` or box-drawing characters, regardless of what the tonal/edge pass would have chosen.
  - *General straight-line detection* — detect arbitrary lines (Hough transform style?) and attempt to fit them with the same character set the art-line tool uses (`/` `\` `|` `—` etc.), essentially applying the line tool's logic automatically to image content.
  - The interesting design question is *layering*: does feature extraction run first and "claim" cells, with the regular AA pass filling in the rest? Or does it run as a post-process that overrides? Or is it a separate optional mode entirely?
  - This is deeply in heuristics territory — worth keeping as a long-range idea rather than designing prematurely.
  - **Long-range**: a hybrid Braille+line-drawing charset — Braille for tonal/density regions, box-drawing and slash chars for detected strong edges — would combine the halftone strength of Braille with explicit structural lines. Connects to the layered feature extraction idea above.

- **AA matching: correlation vs. fixed-position SSD** — the current `compute_best_char` samples a patch at a fixed position and scores every sprite against it via SSD. If the image has a `|` that's slightly off-centre relative to the cell grid, the matcher may prefer `:` or `!` just because their strokes overlap the off-centre feature better at the fixed sample point — even though `|` is unambiguously the right character. The goal is **character fidelity**, not pixel-perfect reproduction: a `|` that's two pixels out of whack is still a `|`. Cross-correlation (trying each sprite at multiple offsets, e.g. ±2px x/y) would let the matcher identify the correct character even when the grid and image aren't aligned. The winning *offset* is not the useful output — it's just the mechanism; discard it once the winning *character* is found. Cost: ~25× current per-cell cost, still sub-millisecond. **Applies to Ascii7 only** — Braille is a halftone/density system, not a stroke-matching system, so it naturally absorbs positional ambiguity for free. Correlation would add noise to Braille matching without benefit.

- **AA post-process: peephole optimisation** — after the AA brush paints a region, run a lightweight pass over the *character output* (not the source image) to regularise near-straight runs. E.g. a sequence of cells that are mostly `|` with one stray `/` gets corrected to all `|`; a diagonal run of mixed `/` and `\` gets snapped to whichever is dominant. This is a classic peephole approach — small sliding window, local pattern matching, simple substitution rules. Should be optional and relatively straightforward to implement; no image-space analysis needed since it operates purely on the already-chosen characters. **Note:** the substitution rules here are analogous to — but probably not identical to — the rules needed for the smart pencil freehand-to-line mode. Both reason about local character runs and directionality, so there may be shared logic or a common rule table worth factoring out, but the inputs differ (AA output cells vs. live stroke path) so they shouldn't be forced to share prematurely.

- **AA tuning philosophy** — all the signal-mixing, feature-extraction, and tone-control questions above share a common answer: *experiment and develop heuristics*. Don't over-design the pipeline in advance; build controls that expose the raw parameters and let empirical testing with real images dictate what the defaults and interactions should be.

- **AA matching semantic — "white = stroke" invariant** — the matching pipeline always expects its source image in white-on-black orientation: bright pixels mean ink/stroke present, dark pixels mean empty space. This is why `bg_edges` works directly (edges are white on black). It also implies that matching against a grayscale image requires using the **inverted** ("negative") grayscale — `255 - luma` — not the original. A white sky in the source image becomes dark in the inverted version, which correctly matches to sparse/empty characters. A dark shadow becomes bright, correctly matching to dense characters. The user is always shown the original un-inverted grayscale; the inversion is an internal matching detail only. The three background display modes map to matching sources as follows:
  - **Grayscale (`Original`)** → match against `255 - bg_luma` (inverted grayscale, stored as `bg_luma_neg`)
  - **Edges white-on-black (`WhiteOnBlack`)** → match against `bg_edges` (current behaviour)
  - **Edges black-on-white (`BlackOnWhite`)** → match against `bg_edges` (same data as above; display inversion is cosmetic only)

- **Scale-adaptive edge detection (implemented)** — the Sobel edge detector runs on `bg_luma_raw` (the full image at processing resolution, `PROCESSING_HEIGHT = 1024`). The processed `bg_edges` covers the whole image; the CSS pan/zoom model is unchanged. The only thing that varies with zoom is the **pre-blur radius** applied to `bg_luma_raw` before Sobel.

  **Reasoning:** a normal-weight 12pt font has stroke widths of approximately 1pt. The canvas represents 24 rows × 12pt = 288pt ≈ 300pt of vertical content. So the feature scale we need to resolve is 1/300 of the canvas height. In processing pixels, this is:

  ```
  feature_px = (ROWS × cell_h) / 300  =  cell_h / 12.5
  ```

  where `cell_h = bg_visible_rect.3` (processing pixels per character row at current zoom).

  A box blur at radius `≈ cell_h / 12.5` (minimum 1) before Sobel suppresses sub-stroke noise while preserving stroke-width edges. As the user zooms in, `cell_h` decreases, the radius shrinks, and the edge map becomes finer — showing detail appropriate to the zoomed-in view. As the user zooms out, the radius grows, suppressing fine detail and leaving only coarse structure visible at character-cell scale.

  **Implementation:** `reprocess_edges_for_scale()` in `lib.rs`, called on `accept_bg_move()` and debounced 500ms after zoom-idle events. Uses `asciiart::scale_blur()` (a two-pass H+V box blur returning `u8`), which reuses the `box_blur_h` / `box_blur_v` helpers originally written for the USM controls. The USM controls themselves were sidelined as ineffective — the pre-blur here supersedes their role in a more principled way.

  **Debounce:** wheel and pinch zoom events increment `zoom_debounce_gen` and schedule a 500ms `setTimeout`. When the timer fires it checks the generation and no-ops if stale. `accept_bg_move()` also calls `reprocess_edges_for_scale()` directly as a final sync (and bumps the gen to cancel pending timers).

  **Zoom limits:** `bg_luma_raw_width` / `bg_luma_raw_height` store the original processing dimensions and are used for pan/zoom limits so those limits remain stable even if future processing changes `bg_luma_width`/`height`.

  **TODO:** try making the reprocess fully interactive (per zoom event, not debounced) — the pre-blur + Sobel pipeline at processing resolution may be fast enough.

- **Arbitrary Unicode character picker** — the current fixed palette (`.`, `*`, box-drawing chars, etc.) works surprisingly well but is a small curated slice of Unicode. The open question is: how do you let someone pick *any* Unicode character as their brush, especially on mobile?

  **Decided:** A "recently used" strip — à la recent colours in colour pickers — will definitely be implemented. It self-populates as the user paints and surfaces the characters they actually care about without any explicit management.

  **Still a head-scratcher:** cold-start discovery of arbitrary Unicode on mobile. The OS keyboard is actually a decent picker (e.g. an IPA keyboard installed on the phone gives access to a wide range of unusual characters), but the challenge remains routing a character from a keyboard input back to the *brush* rather than onto the canvas. No clean solution yet.

---

## Implemented — session notes

**Art pencil mode (implemented)**

A second mode of the pencil tool, cycled by tapping the pencil button while it's already active. Icon: `~`. On each Bresenham-interpolated step, `art_char(dc, dr)` selects `-` `|` `/` `\` based on the direction of travel. The previous cell is retroactively corrected when its exit direction becomes known (i.e., when the cursor moves to the next cell). The first cell of a stroke gets `-` as a placeholder. On pointer-up, the stroke is committed via the normal preview system (`set_preview` / `render_cell`). Follows the same tap-to-cycle pattern as the line/art-line tool. Remaining work: `_`, `(`, `)` rounding cases; backtracking behaviour.

**Styled HTML clipboard copy (implemented)**

The copy button and Ctrl+C now place both `text/plain` and `text/html` on the clipboard simultaneously using `ClipboardItem`. The HTML is a `<pre>` block with `font-family: monospace; line-height: 1.2; white-space: pre` — no color styling (reserved for when per-cell color is added). Trailing spaces are trimmed per line since rich-text editors (Docs, Notion, Slack) drop them anyway. The JS helper `charpaintCopyRich(plain, html)` in `index.html` handles the `ClipboardItem`/`Blob` construction; Rust calls it via `js_sys::Reflect` without needing a `#[wasm_bindgen]` extern. Both the button and the Ctrl+C keyboard path share a single `rich_copy()` async helper in `wiring.rs`.

**BgMove tool lifecycle fixes (implemented)**

- `leave_bg_move_ui()` — a thin helper that removes `.blinking` from the BgMove button and hides the image-controls strip without touching tool state or background position. Called by the generic toolbar handler and by the tap handlers for pencil, line, and fill (all of which previously set `tool` directly, leaving the UI in a blinking/visible state after switching away from BgMove).
- `reprocess_edges_for_scale()` is called from `accept_bg_move()` as a final sync, so the edge map always reflects the accepted framing before the tool exits.

**Image-controls strip — current state**

The Texture and Pop USM controls were removed from the strip after finding them ineffective. The strip now shows only the Hide checkbox. The strip structure is preserved for future controls (the "more edge / less edge" slider discussed but not yet designed).

**AA matching source selection (implemented)**

`compute_best_char` now selects its source image based on `bg_outline_mode`: `Original` (grayscale display) uses `bg_luma_neg` (`255 - luma`); edge modes use `bg_edges`. `bg_luma_neg` is pre-computed in `rebuild_from_params()` alongside `bg_luma` — one pass, no extra processing. The rest of the matching pipeline (box-average patch, SSD, `AA_EDGE_THRESHOLD` check) is identical for both sources. Knobs for the user to control matching parameters are a next step.
