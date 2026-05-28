//! ASCII art (AA) image processing pipeline — pure Rust, no DOM.
//!
//! Functions here accept raw pixel data extracted by the caller via the browser
//! canvas API and return processed Rust data structures. No web-sys types appear
//! in this module; it is independently testable.

/// Processing resolution bounds (height in pixels). The source image is scaled
/// so its height falls in [PROCESSED_MIN_HEIGHT, PROCESSED_MAX_HEIGHT] with
/// width proportional. Width is not independently clamped — extreme panoramas
/// and vertical strips are known edge cases deferred to the placement/rescale UI.
pub(crate) const PROCESSED_MIN_HEIGHT: u32 = 512;
pub(crate) const PROCESSED_MAX_HEIGHT: u32 = 2048;

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
