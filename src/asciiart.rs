//! ASCII art (AA) image processing pipeline — pure Rust, no DOM.
//!
//! Functions here accept raw pixel data extracted by the caller via the browser
//! canvas API and return processed Rust data structures. No web-sys types appear
//! in this module; it is independently testable.

/// Fixed processing height in pixels. Every dropped image is scaled to exactly
/// this height (width proportional) before the AA pipeline runs. 1024px gives
/// 3×3 Sobel edge features of 1–2px width — approximately one font-point stroke
/// width at notional 12pt scale. Balances detail vs. PNG-encoding performance.
pub(crate) const PROCESSING_HEIGHT: u32 = 1024;

/// Assumed character cell height in font points for the AA character-matching step.
/// Not used for edge detection, which runs at full PROCESSING_HEIGHT resolution.
pub(crate) const NOTIONAL_CELL_PX: u32 = 12;

/// Sprite catalog dimensions. Each character is rendered to a bitmap of this size
/// for catalog storage and image-patch comparison. SPRITE_H = NOTIONAL_CELL_PX;
/// SPRITE_W ≈ 0.6× height — standard monospace aspect ratio approximation.
/// Both must match the canvas rendering in build_sprite_catalog (lib.rs).
pub(crate) const SPRITE_W: u32 = 7;
pub(crate) const SPRITE_H: u32 = 12;

/// Top fraction of edge pixels (by magnitude) that maps to 255 after clipping.
/// Lowering this makes fewer pixels qualify as "real edges" — sharper but potentially
/// missing faint edges. Raising it admits more gradual transitions.
const EDGE_CLIP_PERCENT: f32 = 5.0;

/// Gamma exponent applied after the percentile stretch. Values > 1 crush midtones
/// toward black, pushing the result toward binary. 2.5 is a good starting point.
const EDGE_GAMMA: f32 = 2.5;

/// Fraction of pixels clipped at each end of the luminance histogram before
/// the linear stretch. 1 % is gentle — a small number of blown-out highlights
/// or crushed shadows do not drag the stretch endpoints to extremes the way a
/// pure min/max stretch would. Adjust here to taste.
const CLIP_PERCENT: f32 = 1.0;

/// Convert a flat RGBA byte slice (length = width × height × 4) to a flat
/// luminance byte slice (length = width × height).
///
/// Uses the ITU-R BT.709 perceptual weights:
///   L = 0.2126·R + 0.7152·G + 0.0722·B
///
/// Alpha is ignored — images dropped onto the canvas are assumed opaque.
pub fn to_luminance(rgba: &[u8]) -> Vec<u8> {
    let pixel_count = rgba.len() / 4;
    let mut luma = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let r = rgba[i * 4    ] as f32;
        let g = rgba[i * 4 + 1] as f32;
        let b = rgba[i * 4 + 2] as f32;
        luma.push((0.2126 * r + 0.7152 * g + 0.0722 * b).round() as u8);
    }
    luma
}

/// Stretch the luminance range so the image uses the full 0–255 span.
///
/// The bottom and top CLIP_PERCENT of pixels are treated as outliers and mapped
/// to 0 and 255 respectively; everything in between is linearly rescaled.
/// Pixels outside the clip boundaries are clamped.
///
/// This is a linear rescaling — the shape of the tonal distribution is
/// preserved. It is intentionally gentler than histogram equalisation, which
/// redistributes tones non-linearly and can introduce halos and noise.
///
/// If the image is already flat (all pixels the same value) the slice is left
/// unchanged to avoid a divide-by-zero.
pub fn stretch_luminance(luma: &mut [u8]) {
    if luma.is_empty() { return; }

    // Build a 256-bin histogram.
    let mut hist = [0u64; 256];
    for &v in luma.iter() {
        hist[v as usize] += 1;
    }

    let total = luma.len() as f32;
    let clip  = (CLIP_PERCENT / 100.0 * total).round() as u64;

    // Low cut: first bin where the cumulative count exceeds the clip threshold.
    let mut lo: u8 = 0;
    {
        let mut cum = 0u64;
        for (i, &count) in hist.iter().enumerate() {
            cum += count;
            if cum > clip {
                lo = i as u8;
                break;
            }
        }
    }

    // High cut: last bin where the cumulative count from the bright end exceeds
    // the clip threshold.
    let mut hi: u8 = 255;
    {
        let mut cum = 0u64;
        for (i, &count) in hist.iter().enumerate().rev() {
            cum += count;
            if cum > clip {
                hi = i as u8;
                break;
            }
        }
    }

    // Flat image — nothing useful to stretch.
    if lo >= hi { return; }

    let range = (hi - lo) as f32;
    for v in luma.iter_mut() {
        let stretched = ((*v as f32 - lo as f32) / range * 255.0)
            .max(0.0)
            .min(255.0);
        *v = stretched.round() as u8;
    }
}

/// Horizontal box blur using a prefix sum — O(W) per row regardless of radius.
/// Output is f32 to preserve precision across the two-pass separable blur.
fn box_blur_h(src: &[u8], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut dst = vec![0f32; w * h];
    for y in 0..h {
        let mut prefix = vec![0u32; w + 1];
        for x in 0..w { prefix[x + 1] = prefix[x] + src[y * w + x] as u32; }
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r + 1).min(w);
            dst[y * w + x] = (prefix[hi] - prefix[lo]) as f32 / (hi - lo) as f32;
        }
    }
    dst
}

/// Horizontal box blur on f32 data — same logic as box_blur_h but accepts f32 input
/// so it can be chained after a first blur pass without converting back to u8.
#[allow(dead_code)]
fn box_blur_hf(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut dst = vec![0f32; w * h];
    for y in 0..h {
        let mut prefix = vec![0f32; w + 1];
        for x in 0..w { prefix[x + 1] = prefix[x] + src[y * w + x]; }
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r + 1).min(w);
            dst[y * w + x] = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        }
    }
    dst
}

/// Vertical box blur using a prefix sum — O(H) per column regardless of radius.
fn box_blur_v(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut dst = vec![0f32; w * h];
    for x in 0..w {
        let mut prefix = vec![0f32; h + 1];
        for y in 0..h { prefix[y + 1] = prefix[y] + src[y * w + x]; }
        for y in 0..h {
            let lo = y.saturating_sub(r);
            let hi = (y + r + 1).min(h);
            dst[y * w + x] = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        }
    }
    dst
}

/// Apply texture sharpening via unsharp mask: output = clamp(input + factor*(input-blur), 0, 255).
///
/// Uses a single separable box blur at a fixed radius (~10px ≈ 3pt at PROCESSING_HEIGHT 1024).
/// `amount_step` 0 returns a plain clone — no blur is computed.
/// Each step above 0 adds 0.5 to the USM factor (steps 1–5 → factors 0.5–2.5).
#[allow(dead_code)]
pub fn apply_texture(luma: &[u8], w: u32, h: u32, amount_step: u32) -> Vec<u8> {
    if amount_step == 0 { return luma.to_vec(); }
    let factor         = amount_step as f32 * 0.5;
    const RADIUS: usize = 10; // ≈ 3pt at PROCESSING_HEIGHT 1024
    let blurred = box_blur_v(&box_blur_h(luma, w as usize, h as usize, RADIUS), w as usize, h as usize, RADIUS);
    luma.iter().zip(blurred.iter())
        .map(|(&orig, &blur)| {
            (orig as f32 + factor * (orig as f32 - blur)).max(0.0).min(255.0).round() as u8
        })
        .collect()
}

/// Apply pop (large-radius) sharpening via unsharp mask.
///
/// Uses two separable box blur passes at a fixed radius (~45px ≈ one canvas cell at
/// PROCESSING_HEIGHT 1024). Two passes approximate a triangular kernel, giving smoother
/// halos at large radius than a single pass. `amount_step` 0 is a no-op clone.
#[allow(dead_code)]
pub fn apply_pop(luma: &[u8], w: u32, h: u32, amount_step: u32) -> Vec<u8> {
    if amount_step == 0 { return luma.to_vec(); }
    let factor         = amount_step as f32 * 0.5;
    const RADIUS: usize = 45; // ≈ one canvas cell at PROCESSING_HEIGHT 1024
    let wu = w as usize;
    let hu = h as usize;
    // Two-pass separable box blur.
    let pass1   = box_blur_v(&box_blur_h(luma, wu, hu, RADIUS), wu, hu, RADIUS);
    let blurred = box_blur_v(&box_blur_hf(&pass1, wu, hu, RADIUS), wu, hu, RADIUS);
    luma.iter().zip(blurred.iter())
        .map(|(&orig, &blur)| {
            (orig as f32 + factor * (orig as f32 - blur)).max(0.0).min(255.0).round() as u8
        })
        .collect()
}

/// Apply a separable (H then V) box blur and return the result as `u8`.
/// Used as a scale-matched pre-blur before Sobel: suppresses edges finer than `radius`
/// processing pixels, leaving only edges at the spatial scale of a character cell or larger.
pub fn scale_blur(src: &[u8], w: u32, h: u32, radius: usize) -> Vec<u8> {
    if radius == 0 { return src.to_vec(); }
    let wu = w as usize;
    let hu = h as usize;
    box_blur_v(&box_blur_h(src, wu, hu, radius), wu, hu, radius)
        .into_iter()
        .map(|v| v.round() as u8)
        .collect()
}

/// Minimum patch-max value (0–255) for the "must place a non-space character" rule.
/// If the maximum pixel value in the downsampled edge patch meets or exceeds this
/// threshold, space (catalog index 0) is excluded from the SSD search. Cells below
/// the threshold participate in normal matching — space may or may not win naturally.
pub(crate) const AA_EDGE_THRESHOLD: u8 = 35;


/// Find the catalog sprite whose grayscale bitmap most closely matches `patch`
/// by minimum sum-of-squared differences. Both `patch` and every sprite must be
/// SPRITE_W × SPRITE_H bytes.
///
/// `min_idx` sets the first candidate index. Pass `0` to allow space (index 0 =
/// ASCII 32); pass `1` to exclude it when a non-blank character is required.
///
/// Returns the index of the best match; caller maps it back to a char via `idx + 32`.
pub fn best_sprite_match(patch: &[u8], sprites: &[Vec<u8>], min_idx: usize) -> usize {
    let start          = min_idx.min(sprites.len().saturating_sub(1));
    let mut best_idx   = start;
    let mut best_score = f32::MAX;
    for (i, sprite) in sprites.iter().enumerate().skip(start) {
        let score: f32 = patch.iter().zip(sprite.iter())
            .map(|(&a, &b)| { let d = a as f32 - b as f32; d * d })
            .sum();
        if score < best_score {
            best_score = score;
            best_idx   = i;
        }
    }
    best_idx
}

/// Compute a Sobel edge-magnitude map from a luminance slice.
///
/// Runs a 3×3 Sobel filter directly on the full-resolution luma image.
/// At PROCESSING_HEIGHT = 1024px, edges are 1–2px wide — approximately one
/// font-point stroke width at notional 12pt scale.
///
/// Boundary pixels use nearest-edge clamping so the output is the same size
/// as the input (no border shrinkage).
///
/// Returns a flat byte slice (length = width × height) of edge magnitudes,
/// normalised to [0, 255], where 0 = no edge and 255 = strongest edge.
pub fn sobel_edges(luma: &[u8], width: u32, height: u32) -> Vec<u8> {
    if luma.is_empty() || width == 0 || height == 0 {
        return vec![0u8; luma.len()];
    }

    let w = width  as usize;
    let h = height as usize;

    // Gx = [[-1,0,1],[-2,0,2],[-1,0,1]]
    // Gy = [[-1,-2,-1],[0,0,0],[1,2,1]]
    let mut mag = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let s = |oy: i32, ox: i32| -> f32 {
                let nx = ((x as i32 + ox).max(0) as usize).min(w - 1);
                let ny = ((y as i32 + oy).max(0) as usize).min(h - 1);
                luma[ny * w + nx] as f32
            };
            let gx = -s(-1,-1) + s(-1,1) - 2.0*s(0,-1) + 2.0*s(0,1) - s(1,-1) + s(1,1);
            let gy = -s(-1,-1) - 2.0*s(-1,0) - s(-1,1) + s(1,-1) + 2.0*s(1,0) + s(1,1);
            mag[y * w + x] = (gx * gx + gy * gy).sqrt();
        }
    }

    // Normalise to [0, 255].
    let max_val = mag.iter().cloned().fold(0.0f32, f32::max);
    let mut result = vec![0u8; w * h];
    if max_val > 0.0 {
        for (i, &v) in mag.iter().enumerate() {
            result[i] = (v / max_val * 255.0).min(255.0).round() as u8;
        }
    }
    result
}

/// Post-process a Sobel edge map toward a near-binary appearance.
///
/// Two steps:
///   1. Percentile clip — the top EDGE_CLIP_PERCENT of pixels map to 255;
///      everything below is linearly stretched. This resets the "real edge" floor
///      so the noise floor becomes relatively darker.
///   2. Gamma curve — raise each normalised value to the EDGE_GAMMA power.
///      Exponents > 1 crush midtones toward black while leaving bright edges intact.
///
/// The combination produces an output that reads as near-binary: background ~0,
/// edges ~255, with only a short transition band between them.
pub fn enhance_edges(edges: &mut [u8]) {
    if edges.is_empty() { return; }

    // Build a 256-bin histogram to locate the clip threshold.
    let mut hist = [0u64; 256];
    for &v in edges.iter() {
        hist[v as usize] += 1;
    }

    let total      = edges.len() as f32;
    let clip_count = (EDGE_CLIP_PERCENT / 100.0 * total).round() as u64;

    // Find the value where the top EDGE_CLIP_PERCENT of pixels start (high cut).
    let mut hi: u8 = 255;
    {
        let mut cum = 0u64;
        for (i, &count) in hist.iter().enumerate().rev() {
            cum += count;
            if cum > clip_count {
                hi = i as u8;
                break;
            }
        }
    }

    // Degenerate case: no edges detected (blank image or uniform field).
    if hi == 0 { return; }

    let scale = 255.0 / hi as f32;

    for v in edges.iter_mut() {
        // Step 1: linear stretch so the clip point maps to 255, clamp above.
        let stretched = (*v as f32 * scale).min(255.0);
        // Step 2: gamma — (v/255)^gamma * 255 — crushes midtones toward black.
        *v = ((stretched / 255.0).powf(EDGE_GAMMA) * 255.0).round() as u8;
    }
}
