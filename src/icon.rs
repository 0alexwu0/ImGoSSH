//! The application icon, taken from a picture rather than painted to order.
//!
//! One asset, three consumers: `main.rs` hands the pixels to egui for the window
//! and the taskbar, `build.rs` compiles the very same pixels into a `.ico`
//! resource so that the executable itself carries the tile in Explorer, and
//! `ui::theme::logo` bakes them into the brand mark of the welcome page and the
//! About box. The module is mounted into the build script with `#[path]`, so
//! everything here has to be something a build script can do as well: read a
//! file that is compiled in, and shrink it.
//!
//! The picture is a golden cat curled around an `SSH>` terminal. That is the
//! whole argument for it - the cat is the part you remember, the prompt is the
//! part the program is for.
//!
//! What the picture was drawn on is not part of it. The master is cropped to the
//! tile and carries the tile's own rounded silhouette as its alpha, so the icon
//! is a tile with air around it rather than a slate of dark navy squared off to
//! the edges of the canvas. Transparent pixels are not finished pixels though:
//! they still hold the colour they were cut out of, and that colour travels into
//! every average taken across the edge of the tile, and into every scaler that
//! samples the icon up for a large Explorer view. Two things keep it from
//! arriving. [`matte`] dresses the air in the tile's own colour, so that air and
//! tile agree wherever they are averaged together; and [`shrink`] averages with
//! alpha as its weight, so that air counts for coverage and nothing else.
//!
//! The picture in `assets/` has been through [`matte`] once before it got here,
//! and a test keeps it that way. Its own ring of half-covered pixels along the
//! rounded corners had been cut against something a good deal darker than the
//! tile - `(0,5,12)` next to a body of `(0,57,152)` - and no amount of weighing
//! in [`shrink`] gets a darkness like that out of an average, because those
//! pixels are the tile and not air: they are what the edge of the icon is made
//! of. Matting the picture first leaves the alpha of every one of them exactly
//! as it was, and leaves the runtime pass with nothing left to do but wait for
//! the next picture to be swapped in.

use std::collections::VecDeque;
use std::sync::OnceLock;

/// The master artwork: the tile cropped to fill the canvas, the ground cut away
/// along its corner arcs, and the only picture the app ships. Replace this file
/// with another square PNG - alpha included, at `{MASTER}`px or better - and
/// every size follows, the window icon, the brand mark and the `.ico` alike.
const ART: &[u8] = include_bytes!("../assets/icon.png");
/// The side the master is drawn at - twice the largest size the `.ico` offers,
/// so an icon is always shrunk from art that still had room to be sharp.
const MASTER: u32 = 512;

/// A picture weighed by its own alpha, in the range the filter reads colours
/// in, and in the room a float gives a running total of nine of them.
type Weighed = image::ImageBuffer<image::Rgba<f32>, Vec<f32>>;

/// Paint a `size` x `size` icon and return its RGBA8 bytes, top row first.
#[must_use]
pub fn paint(size: u32) -> Vec<u8> {
    assert!(size > 0, "the icon needs at least one pixel");
    shrink(master(), size)
}

/// Shrink `art` to `size` x `size` and return its RGBA8 bytes, top row first.
fn shrink(art: &image::RgbaImage, size: u32) -> Vec<u8> {
    // Over a source this much bigger than the target a tent filter is an area
    // average, which is what an icon shrunk to 16px wants: the gold of the cat
    // and the dark of the terminal each keep their own patch instead of mixing
    // into a bruise along every edge. Weighing the colours by alpha first is
    // what makes the same average tell the truth where the tile stops and the
    // air begins.
    let shrunk = image::imageops::resize(
        &premultiply(art),
        size,
        size,
        image::imageops::FilterType::Triangle,
    );

    let (width, height) = art.dimensions();
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let [red, green, blue, coverage] = shrunk.get_pixel(x, y).0;
            // A pixel of nothing has no colour of its own to divide back out,
            // so it keeps the one the master has at the same spot. It is
            // invisible either way, but the shell scales the `.ico` it is handed
            // to whatever size a view wants, and a scaler that samples across
            // the edge of the silhouette then paints the tile's own border
            // rather than a black seam.
            let rgba = if coverage <= 0.0 {
                art.get_pixel(x * width / size, y * height / size).0
            } else {
                let straight =
                    |weighed: f32| (weighed / coverage * 255.0).round().clamp(0.0, 255.0) as u8;
                [
                    straight(red),
                    straight(green),
                    straight(blue),
                    (coverage * 255.0).round() as u8,
                ]
            };
            out.extend_from_slice(&rgba);
        }
    }
    out
}

/// Scale every colour of `art` by the alpha that comes with it.
///
/// An average taken over straight-alpha pixels counts the air beside a
/// silhouette as so much paint, and drags whatever the artist had laid down
/// before cutting the shape out into the edge of the result: here a cold seam
/// along the arc of a warm cat. Weighed by alpha, air contributes nothing but
/// its share of the coverage, and the average is the colour the tile would have
/// had if the corner had simply been further away.
fn premultiply(art: &image::RgbaImage) -> Weighed {
    image::ImageBuffer::from_fn(art.width(), art.height(), |x, y| {
        let [red, green, blue, alpha] = art.get_pixel(x, y).0;
        let coverage = f32::from(alpha) / 255.0;
        image::Rgba([
            f32::from(red) / 255.0 * coverage,
            f32::from(green) / 255.0 * coverage,
            f32::from(blue) / 255.0 * coverage,
            coverage,
        ])
    })
}

/// Dress the air outside the tile in the tile's own colour, alpha left as it is.
///
/// Cutting a rounded tile out of a picture empties its corners of alpha but not
/// of colour, and the colour left behind is the ground the tile was drawn on -
/// here a blue a long way from the navy of the tile. Nothing shows while the
/// alpha is nil, but every filter that reaches across the edge picks the ground
/// up, and the air is exactly where the edge of an icon is. Bleeding the tile
/// outward means that wherever air and tile are averaged together they agree
/// about the colour, whatever silhouette the next picture swapped in happens to
/// carry.
fn matte(mut art: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = art.dimensions();
    // The walk reads colours out of the picture as it goes, so it has to ask
    // about alpha from a copy rather than from the picture it is writing.
    let alpha: Vec<u8> = art.pixels().map(|pixel| pixel.0[3]).collect();
    let see_through = |index: u32| alpha[index as usize] < u8::MAX;

    // Only the silhouette has to be reached from: the walk starts on the opaque
    // pixels with air beside them and dresses the air outward from there.
    let mut frontier: VecDeque<u32> = (0..width * height)
        .filter(|&index| {
            !see_through(index)
                && neighbours(index, width, height)
                    .iter()
                    .any(|&next| see_through(next))
        })
        .collect();
    let mut walked = vec![false; alpha.len()];
    while let Some(index) = frontier.pop_front() {
        let colour = art.get_pixel(index % width, index / width).0;
        for next in neighbours(index, width, height) {
            if see_through(next) && !walked[next as usize] {
                walked[next as usize] = true;
                let pixel = art.get_pixel_mut(next % width, next / width);
                pixel.0[..3].copy_from_slice(&colour[..3]);
                frontier.push_back(next);
            }
        }
    }
    art
}

/// The indices of the pixels beside `index`, leaving out the ones off the canvas.
fn neighbours(index: u32, width: u32, height: u32) -> Vec<u32> {
    let (x, y) = (index % width, index / width);
    let mut around = Vec::with_capacity(4);
    if x > 0 {
        around.push(index - 1);
    }
    if x + 1 < width {
        around.push(index + 1);
    }
    if y > 0 {
        around.push(index - width);
    }
    if y + 1 < height {
        around.push(index + width);
    }
    around
}

/// The decoded master, made once per process: the build script asks for six
/// sizes and the interface for two more, all out of a single decode.
fn master() -> &'static image::RgbaImage {
    static DECODED: OnceLock<image::RgbaImage> = OnceLock::new();
    DECODED.get_or_init(|| {
        let art = image::load_from_memory(ART)
            .expect("assets/icon.png is compiled into the binary, so it is readable")
            .into_rgba8();
        assert_eq!(
            art.dimensions(),
            (MASTER, MASTER),
            "assets/icon.png is the {MASTER}px square master the icon is shrunk from"
        );
        matte(art)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel at a point of the 64px grid, as `(r, g, b, a)`.
    fn design_pixel(icon: &[u8], x: i32, y: i32) -> (u8, u8, u8, u8) {
        let at = (((y + 32) * 64 + (x + 32)) * 4) as usize;
        (icon[at], icon[at + 1], icon[at + 2], icon[at + 3])
    }

    /// The gold of the cat's fur: warm against everything blue on the tile.
    fn fur(pixel: &[u8]) -> bool {
        i16::from(pixel[0]) > i16::from(pixel[2]) + 30 && pixel[0] > 120
    }

    /// How dark a pixel is, out of its three channels.
    fn darkness(pixel: &[u8]) -> u8 {
        pixel[0].max(pixel[1]).max(pixel[2])
    }

    /// How white a pixel is, out of its three channels. The fur is a warm white
    /// and scores low here; the prompt, which is a cold one, scores high.
    fn whiteness(pixel: &[u8]) -> u8 {
        pixel[0].min(pixel[1]).min(pixel[2])
    }

    /// The lower half of the tile, where the terminal window is.
    fn below_the_belt(icon: &[u8], size: usize) -> Vec<&[u8]> {
        icon.chunks_exact(4)
            .enumerate()
            .filter(move |&(index, _)| (index / size) >= size / 2)
            .map(|(_, pixel)| pixel)
            .collect()
    }

    /// A picture with a hard-edged tile in one corner and air everywhere else,
    /// the air still wearing a red the tile has never been any part of.
    fn tile_over_ground(side: u32, tile: u32) -> image::RgbaImage {
        let mut art = image::RgbaImage::from_pixel(side, side, image::Rgba([255, 0, 0, 0]));
        for column in 0..tile {
            for row in 0..side {
                art.put_pixel(column, row, image::Rgba([255, 255, 255, 255]));
            }
        }
        art
    }

    #[test]
    fn every_size_delivers_one_byte_per_channel() {
        for size in [16u32, 24, 32, 48, 64, 128, 256] {
            assert_eq!(paint(size).len(), (size * size * 4) as usize, "{size}px");
        }
    }

    #[test]
    fn the_master_shrinks_from_its_own_size() {
        // Asking for the master at its own size has to hand back the master: the
        // 256px entry of the .ico is one resample away from it, and no further.
        assert_eq!(paint(MASTER).as_slice(), master().as_raw().as_slice());
    }

    #[test]
    fn the_tile_is_opaque_and_its_corners_are_not() {
        // The picture is a rounded tile on a ground of nearly the same navy, and
        // the ground is not part of the icon. Leave paint in the corners and the
        // tile reads as a slate dropped on a light taskbar.
        let icon = paint(64);
        for (x, y) in [(-31, -31), (30, -31), (-31, 30), (30, 30)] {
            let (_, _, _, a) = design_pixel(&icon, x, y);
            assert!(a < 20, "the corner at ({x},{y}) was not cut away: {a}");
        }
        // Where the tile is, the icon is solid: nothing to blend with a wallpaper.
        for (x, y) in [(0, 0), (0, -31), (0, 30), (-31, 0), (30, 0)] {
            let (_, _, _, a) = design_pixel(&icon, x, y);
            assert!(a > 250, "the tile is see-through at ({x},{y}): {a}");
        }
    }

    #[test]
    fn the_air_wears_a_colour_the_tile_has() {
        // Transparent is not the same as inert. The shrink weighs the air out of
        // its averages, but the shell scales the .ico it is given, and a scaler
        // reaching across the arc reads the colour under the alpha as plainly as
        // it reads the arc itself. Ground left there is a seam waiting to happen.
        let art = master();
        let tile: std::collections::HashSet<[u8; 3]> = art
            .pixels()
            .filter(|pixel| pixel.0[3] == u8::MAX)
            .map(|pixel| [pixel.0[0], pixel.0[1], pixel.0[2]])
            .collect();
        for (index, pixel) in art.pixels().enumerate() {
            if pixel.0[3] < u8::MAX {
                let colour = [pixel.0[0], pixel.0[1], pixel.0[2]];
                assert!(
                    tile.contains(&colour),
                    "the air at {} still wears the ground: {colour:?}",
                    index as u32 % MASTER
                );
            }
        }
    }

    #[test]
    fn shrinking_a_silhouette_leaves_the_ground_behind() {
        // Three columns of white tile beside air cut out of a red ground. The
        // red shows through nowhere on its own, so the only way it reaches an
        // icon is by being averaged in: by this shrink along the join, or by a
        // scaler along the same join later. The alpha has to survive the trip,
        // or the tile would have lost its edge instead of its ground.
        let icon = shrink(&matte(tile_over_ground(8, 3)), 4);
        for (index, pixel) in icon.chunks_exact(4).enumerate() {
            assert!(
                pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200,
                "the ground leaked into pixel {index}: {pixel:?}"
            );
        }
        let alpha = |x: u32| icon[(x * 4 + 3) as usize];
        assert_eq!(alpha(0), u8::MAX, "the tile lost its own edge");
        assert_eq!(alpha(3), 0, "the air stopped being air");
    }

    #[test]
    fn matting_leaves_the_alpha_it_was_given() {
        let art = matte(tile_over_ground(8, 3));
        for row in 0..8 {
            assert_eq!(
                art.get_pixel(0, row).0,
                [255, 255, 255, 255],
                "the tile moved"
            );
            assert_eq!(
                art.get_pixel(7, row).0,
                [255, 255, 255, 0],
                "the air at row {row} is neither the tile's colour nor still air"
            );
        }
    }

    #[test]
    fn the_picture_ships_already_clean() {
        // The master in the repository has had the same treatment as the one this
        // module hands out: the ring of half-covered pixels along the rounded
        // corners was cut out against something darker than the tile, and wore
        // that darkness as its colour. Left in the picture, it is a black line
        // along every arc on a light taskbar - which the shrink cannot weigh out
        // of an average, because those pixels are the tile and not air. Matting
        // the shipped picture has to be a no-op, so the picture cannot quietly
        // grow a fringe again the next time it is swapped out.
        let raw = image::load_from_memory(ART)
            .expect("assets/icon.png is compiled into the binary, so it is readable")
            .into_rgba8();
        let shipped = raw.as_raw().clone();
        assert_eq!(
            matte(raw).as_raw(),
            &shipped,
            "assets/icon.png carries colours under its alpha that the tile does not have"
        );
    }

    #[test]
    fn the_cat_survives_at_the_smallest_size() {
        // The cat is the picture: shrunk to a title bar the gold still owns a
        // good part of the tile instead of washing out into the navy behind it.
        let warm = paint(16).chunks(4).filter(|pixel| fur(pixel)).count();
        assert!(warm > 16 * 16 / 8, "16px lost the cat: {warm} warm pixels");
    }

    #[test]
    fn the_terminal_carries_the_prompt() {
        // The bottom half of the tile is a terminal window: dark glass, with the
        // prompt cut into it in white. That is the half that says SSH, and the
        // fur above it is warm enough to never pass for either.
        let icon = paint(64);
        let glass = below_the_belt(&icon, 64)
            .iter()
            .filter(|pixel| darkness(pixel) < 90 && pixel[2] > pixel[0])
            .count();
        assert!(
            glass > 64 * 32 / 4,
            "the terminal is not dark glass: {glass} of {}",
            64 * 32
        );
        let whitest = below_the_belt(&icon, 64)
            .iter()
            .map(|&pixel| whiteness(pixel))
            .max();
        assert!(
            whitest.is_some_and(|white| white > 200),
            "the prompt is gone from the terminal: {whitest:?}"
        );
    }
}
