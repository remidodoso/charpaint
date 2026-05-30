# Design & State Musings

Maintain a running notes file (`design_and_state_musings.md`) to capture design decisions, open questions, and state observations as work progresses.

---

## New project state & auto-BgMove

`is_new_project: bool` in App — true on first load, true after a future "New Project" button press, false once the user has meaningfully started work. Two things end the new-project state:
1. **First image loaded** — triggers auto-entry into BgMove so the user can immediately frame the image. Sets `is_new_project = false`.
2. **First canvas touch** — same hook as `clear_demo_if_active()`. If the user paints before loading an image, auto-BgMove on a later image drop would be wrong, so the state ends here too.

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

- **"Smart pencil" / freehand-to-line mode** — a pencil sub-mode that interprets freehand strokes on the fly and substitutes directional line characters: `|` `-` `_` `/` `\`, possibly `(` `)` and others for curves. The challenge is doing this in real time as the stroke is drawn, not as a post-process. Needs a scheme for choosing the right character based on local stroke direction, and deciding how much smoothing / look-ahead to apply.

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
      - **TODO: implement and experiment.**
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

- **Arbitrary Unicode character picker** — the current fixed palette (`.`, `*`, box-drawing chars, etc.) works surprisingly well but is a small curated slice of Unicode. The open question is: how do you let someone pick *any* Unicode character as their brush, especially on mobile?

  **Decided:** A "recently used" strip — à la recent colours in colour pickers — will definitely be implemented. It self-populates as the user paints and surfaces the characters they actually care about without any explicit management.

  **Still a head-scratcher:** cold-start discovery of arbitrary Unicode on mobile. The OS keyboard is actually a decent picker (e.g. an IPA keyboard installed on the phone gives access to a wide range of unusual characters), but the challenge remains routing a character from a keyboard input back to the *brush* rather than onto the canvas. No clean solution yet.
