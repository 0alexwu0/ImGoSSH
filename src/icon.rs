//! The application icon, painted from primitives rather than shipped as a bitmap.
//!
//! One picture, two consumers: `main.rs` hands the pixels to egui for the window
//! and taskbar, and `build.rs` compiles the very same pixels into a `.ico`
//! resource so that the executable itself carries the tile in Explorer. The
//! module is therefore `std` only - it is mounted into the build script with
//! `#[path]`.

use std::f32::consts::TAU;

/// Rounding radius and edge inset of the tile, on the 64px design grid.
const RADIUS: f32 = 15.0;
const INSET: f32 = 1.0;
/// Half thickness of the chevron strokes and of the caret, same grid.
const CHEVRON: f32 = 2.6;
const CARET: f32 = 1.5;
/// Size of the `>_` mark, and where its centre sits once the nameplate moved in.
const PROMPT: f32 = 0.8;
const PROMPT_Y: f32 = -12.0;
/// Centre of the mark as originally drawn, so `PROMPT` scales about it.
const MARK_X: f32 = 4.5;
const MARK_Y: f32 = -0.25;

/// The nameplate: where it starts, the cap height it carries and the baseline.
const PLATE_TOP: f32 = 7.0;
const CAP: f32 = 9.0;
const BASELINE: f32 = 23.8;
/// Stem weight of the lettering, in hundredths of a capital.
const STROKE: f32 = 15.0;
/// Gap left between the glyphs of the wordmark.
const TRACK: f32 = 4.0;
/// Horizontal centre of the inked wordmark, in lettering units.
const INK_CENTRE: f32 = 302.5;

/// Colours of the tile: a shallow gradient, light at the top.
const TOP: [u8; 3] = [0x5a, 0xa9, 0xee];
const BOTTOM: [u8; 3] = [0x2c, 0x6f, 0xb5];
/// The plate is the same blue, darkened, so the wordmark stays legible on it.
const PLATE: [u8; 3] = [0x14, 0x3c, 0x70];

/// Paint a `size` x `size` icon and return its RGBA8 bytes, top row first.
#[must_use]
pub fn paint(size: u32) -> Vec<u8> {
    assert!(size > 0, "the icon needs at least one pixel");
    // The small sizes are the ones that show jaggies, so they get the most
    // samples per pixel; the big ones take long enough as it is.
    let samples: u32 = if size <= 32 {
        4
    } else if size <= 64 {
        3
    } else {
        2
    };
    let step = 1.0 / samples as f32;
    let weight = 1.0 / (samples * samples) as f32;

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for (index, pixel) in rgba.chunks_mut(4).enumerate() {
        let x = (index % size as usize) as f32;
        let y = (index / size as usize) as f32;
        // Accumulate premultiplied, so the transparent corners cannot smear
        // their black into the antialiased edge of the tile.
        let mut sum = [0.0f32; 4];
        for row in 0..samples {
            for column in 0..samples {
                let [r, g, b, a] = sample(
                    x + (column as f32 + 0.5) * step,
                    y + (row as f32 + 0.5) * step,
                    size,
                );
                sum = [sum[0] + r * a, sum[1] + g * a, sum[2] + b * a, sum[3] + a];
            }
        }
        let alpha = sum[3] * weight;
        let channel = |value: f32| {
            if alpha > 0.0 {
                (value * weight / alpha).min(255.0)
            } else {
                0.0
            }
        };
        pixel[0] = channel(sum[0]).round() as u8;
        pixel[1] = channel(sum[1]).round() as u8;
        pixel[2] = channel(sum[2]).round() as u8;
        pixel[3] = (alpha * 255.0).round() as u8;
    }
    rgba
}

/// One sample at `(x, y)` in pixel coordinates, as `(r, g, b, alpha)`.
fn sample(x: f32, y: f32, size: u32) -> [f32; 4] {
    // The design lives on a 64px grid and is scaled to whatever is asked for.
    let scale = size as f32 / 64.0;
    let half = size as f32 / 2.0;
    let (px, py) = (x - half, y - half);

    let tile = rounded_square(px, py, half - INSET * scale, RADIUS * scale);
    let alpha = coverage(tile);
    if alpha <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }

    let t = (py + half) / (half * 2.0);
    let mix = |top: u8, bottom: u8| top as f32 * (1.0 - t) + bottom as f32 * t;
    let mut rgb = [
        mix(TOP[0], BOTTOM[0]),
        mix(TOP[1], BOTTOM[1]),
        mix(TOP[2], BOTTOM[2]),
    ];

    // The nameplate is the tile cut off below a line, so its bottom keeps the
    // rounding of the tile it was carved out of.
    let plate = coverage(tile.max(PLATE_TOP * scale - py));
    for (index, channel) in rgb.iter_mut().enumerate() {
        *channel = *channel * (1.0 - plate) + f32::from(PLATE[index]) * plate;
    }

    // Everything drawn on the tile is the same white.
    let mark = prompt(px, py, scale).max(wordmark(px, py, scale));
    for channel in &mut rgb {
        *channel = *channel * (1.0 - mark) + 255.0 * mark;
    }

    [rgb[0], rgb[1], rgb[2], alpha]
}

/// The `>_` prompt, scaled down and lifted above the nameplate.
fn prompt(px: f32, py: f32, scale: f32) -> f32 {
    let unit = PROMPT * scale;
    let (mx, my) = (
        MARK_X + px / (PROMPT * scale),
        MARK_Y + (py / scale - PROMPT_Y) / PROMPT,
    );
    let chevron = segment(mx, my, -11.0, -9.0, 4.0, 0.0).min(segment(mx, my, -11.0, 9.0, 4.0, 0.0));
    let caret = segment(mx, my, 7.0, 8.5, 20.0, 8.5);
    coverage(chevron * unit - (CHEVRON * unit).max(1.0))
        .max(coverage(caret * unit - (CARET * unit).max(0.7)))
}

/// The `ImGoSSH` lettering, drawn from the strokes below.
fn wordmark(px: f32, py: f32, scale: f32) -> f32 {
    // Lettering units: a capital is 100 tall and the baseline sits at 100.
    let unit = CAP * 0.01 * scale;
    let (lx, ly) = (px / unit + INK_CENTRE, py / unit + BASE);
    let mut distance = f32::INFINITY;
    let mut left = 0.0;
    for (advance, strokes) in WORDMARK {
        for stroke in *strokes {
            let at = match stroke {
                Stroke::Line(points) => polyline(lx - left, ly, points),
                Stroke::Arc(orbit) => arc(lx - left, ly, orbit),
            };
            distance = distance.min(at);
        }
        left += advance + TRACK;
    }
    // The stems are only a pixel wide by the time the tile is 32px across,
    // so they get a floor: below it the plate would simply come out blank.
    let half = (STROKE * 0.5).max(0.42 / unit);
    coverage((distance - half) * unit)
}

/// A capital is 100 units tall; this is where the baseline lands once the
/// `BASELINE` and `CAP` design measurements are turned into lettering units.
const BASE: f32 = 100.0 - BASELINE * 100.0 / CAP;

/// One piece of a letter: a polyline, or a circular arc swept counter clockwise
/// from the first angle to the second, in degrees, with 90 degrees at the top.
enum Stroke {
    Line(&'static [[f32; 2]]),
    Arc(&'static [f32; 5]),
}

const UPPER_I: &[Stroke] = &[Stroke::Line(&[[15.0, 0.0], [15.0, 100.0]])];

const LOWER_M: &[Stroke] = &[
    Stroke::Line(&[[13.0, 44.0], [13.0, 100.0]]),
    Stroke::Arc(&[28.25, 57.0, 15.25, 0.0, 180.0]),
    Stroke::Line(&[[43.5, 57.0], [43.5, 100.0]]),
    Stroke::Arc(&[58.75, 57.0, 15.25, 0.0, 180.0]),
    Stroke::Line(&[[74.0, 57.0], [74.0, 100.0]]),
];

const UPPER_G: &[Stroke] = &[
    Stroke::Arc(&[56.0, 50.0, 48.0, 30.0, 330.0]),
    Stroke::Line(&[[97.6, 74.0], [97.6, 50.0]]),
    Stroke::Line(&[[97.6, 50.0], [72.0, 50.0]]),
];

const LOWER_O: &[Stroke] = &[Stroke::Arc(&[44.0, 72.0, 28.0, 0.0, 360.0])];

const UPPER_S: &[Stroke] = &[
    Stroke::Arc(&[42.0, 26.0, 24.0, 30.0, 270.0]),
    Stroke::Arc(&[42.0, 74.0, 24.0, -160.0, 90.0]),
];

const UPPER_H: &[Stroke] = &[
    Stroke::Line(&[[17.0, 0.0], [17.0, 100.0]]),
    Stroke::Line(&[[87.0, 0.0], [87.0, 100.0]]),
    Stroke::Line(&[[17.0, 52.0], [87.0, 52.0]]),
];

/// The name, as advance widths and the strokes that spell them out.
const WORDMARK: &[(f32, &[Stroke])] = &[
    (30.0, UPPER_I),
    (87.0, LOWER_M),
    (106.0, UPPER_G),
    (88.0, LOWER_O),
    (84.0, UPPER_S),
    (84.0, UPPER_S),
    (104.0, UPPER_H),
];

/// Coverage of a shape from a signed distance: one pixel of antialiasing.
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Signed distance to a rounded square centred on the origin: negative inside.
fn rounded_square(px: f32, py: f32, half: f32, radius: f32) -> f32 {
    let qx = px.abs() - half + radius;
    let qy = py.abs() - half + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Distance from `(px, py)` to the segment `a`-`b`.
fn segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let along = (((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    (px - (ax + vx * along)).hypot(py - (ay + vy * along))
}

/// Distance to a polyline given as consecutive points.
fn polyline(px: f32, py: f32, points: &[[f32; 2]]) -> f32 {
    let mut distance = f32::INFINITY;
    for pair in points.windows(2) {
        distance = distance.min(segment(
            px, py, pair[0][0], pair[0][1], pair[1][0], pair[1][1],
        ));
    }
    distance
}

/// Distance to a circular arc, given as centre, radius and the two angles.
fn arc(px: f32, py: f32, spec: &[f32; 5]) -> f32 {
    let (cx, cy, radius, from, to) = (spec[0], spec[1], spec[2], spec[3], spec[4]);
    // The grid has y pointing down; the angles are the usual way round.
    let (dx, dy) = (px - cx, cy - py);
    let sweep = (to - from).to_radians();
    let mut turned = dy.atan2(dx) - from.to_radians();
    while turned < 0.0 {
        turned += TAU;
    }
    if turned <= sweep {
        // A stroke is a band around the circle, so it is the distance to the
        // curve that matters, in either direction.
        return (dx.hypot(dy) - radius).abs();
    }
    let angle = if turned - sweep < TAU - turned {
        to
    } else {
        from
    };
    (px - (cx + radius * angle.to_radians().cos()))
        .hypot(py - (cy - radius * angle.to_radians().sin()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_delivers_one_byte_per_channel() {
        for size in [16, 24, 32, 48, 64, 128, 256] {
            assert_eq!(paint(size).len(), (size * size * 4) as usize, "{size}px");
        }
    }

    #[test]
    fn the_tile_is_opaque_and_its_corners_are_not() {
        const SIDE: usize = 32;
        let icon = paint(SIDE as u32);
        let alpha = |x: usize, y: usize| icon[(y * SIDE + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "the rounded corner must be see-through");
        assert!(
            alpha(SIDE / 2, SIDE / 2) > 250,
            "the middle of the tile must be opaque"
        );
    }

    #[test]
    fn the_prompt_survives_at_the_smallest_size() {
        // The mark is white on a blue tile: something inside it has to come out
        // plainly brighter than the gradient behind it.
        let brightest = paint(16)
            .chunks(4)
            .filter(|pixel| pixel[3] > 250)
            .map(|pixel| u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2]))
            .max();
        assert!(
            brightest.is_some_and(|sum| sum > 3 * 200),
            "16px lost the prompt: {brightest:?}"
        );
    }

    /// The pixel at a point of the 64px design grid, as `(r, g, b)`.
    fn design_pixel(icon: &[u8], x: i32, y: i32) -> (u8, u8, u8) {
        let at = (((y + 32) * 64 + (x + 32)) * 4) as usize;
        (icon[at], icon[at + 1], icon[at + 2])
    }

    #[test]
    fn the_nameplate_carries_the_wordmark() {
        // The plate is the darker blue below the seam; the lettering has to put
        // white inside it rather than wash the whole band out.
        let icon = paint(64);
        let (left, top, band, height) = (-25i32, 15i32, 51i32, 9i32);
        let mut inked = 0;
        for row in top..top + height {
            for column in left..left + band {
                let (r, _, _) = design_pixel(&icon, column, row);
                if r > 140 {
                    inked += 1;
                }
            }
        }
        let area = (band * height) as usize;
        assert!(
            (area / 4..area * 4 / 5).contains(&inked),
            "the wordmark inked {inked} of {area} plate pixels"
        );
        // Above the lettering the plate stays dark, so the name has room to read.
        let (r, g, b) = design_pixel(&icon, 0, 10);
        assert!(
            r < 90 && g < 130 && b < 190,
            "the plate came out as ({r},{g},{b})"
        );
    }

    #[test]
    fn the_prompt_and_the_nameplate_stay_apart() {
        // The gap between the two marks has to be plate blue, not prompt white,
        // otherwise the tile reads as one blob at the smaller sizes.
        let icon = paint(64);
        for x in -20..21 {
            let (r, g, b) = design_pixel(&icon, x, 2);
            assert!(
                r < 150,
                "the prompt bleeds into the plate at x={x}: ({r},{g},{b})"
            );
        }
    }
}
