// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The textures one look is drawn with, laid out for an atlas that hands out square slots: a model's own
//! texture and those of its layers ([`crate::resolver::Layer`]), each maybe several frames shown in turn.
//!
//! Cobblemon binds each texture as it draws. An atlas has a fixed number of slots for every Pokémon in
//! view, and a look's textures are most often of one small size (64 by 64 for a third of Cobblemon
//! 1.8.1's), so they go side by side in as few slots as hold them: Ponyta's coat and the four frames of
//! its mane take one slot, not five.

/// `width * height` RGBA texels, rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Where one of the images given to [`pack`] is in its sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The image's place among those given.
    pub image: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One slot's worth of images.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheet {
    /// No larger than the slot, and no larger than its cells need.
    pub image: Image,
    pub cells: Vec<Cell>,
}

/// `images`, in their order, in as few sheets no larger than `slot` square as a grid of equal cells
/// gives: every cell as wide as the widest image and as tall as the tallest, each image at its cell's
/// top left corner, so the first image is at the first sheet's. What no image covers is transparent.
///
/// None without images, for an image larger than a slot (the caller's to shrink first) and for one whose
/// texels are not `width * height`.
pub fn pack(images: &[Image], slot: u32) -> Option<Vec<Sheet>> {
    let cell_width = images.iter().map(|image| image.width).max()?;
    let cell_height = images.iter().map(|image| image.height).max()?;
    let whole = |image: &Image| {
        image.width > 0
            && image.height > 0
            && image.rgba.len() as u64 == u64::from(image.width) * u64::from(image.height) * 4
    };
    if cell_width > slot || cell_height > slot || !images.iter().all(whole) {
        return None;
    }
    let columns = slot / cell_width;
    let per_sheet = (columns * (slot / cell_height)) as usize;
    let sheets = images
        .chunks(per_sheet)
        .enumerate()
        .map(|(sheet, held)| {
            let count = held.len() as u32;
            let width = count.min(columns) * cell_width;
            let height = count.div_ceil(columns) * cell_height;
            let mut rgba = vec![0; (width * height * 4) as usize];
            let cells = held
                .iter()
                .enumerate()
                .map(|(place, image)| {
                    let x = place as u32 % columns * cell_width;
                    let y = place as u32 / columns * cell_height;
                    let row = (image.width * 4) as usize;
                    for (line, texels) in image.rgba.chunks_exact(row).enumerate() {
                        let at = (((y + line as u32) * width + x) * 4) as usize;
                        rgba[at..at + row].copy_from_slice(texels);
                    }
                    Cell {
                        image: sheet * per_sheet + place,
                        x,
                        y,
                        width: image.width,
                        height: image.height,
                    }
                })
                .collect();
            Sheet {
                image: Image {
                    width,
                    height,
                    rgba,
                },
                cells,
            }
        })
        .collect();
    Some(sheets)
}

/// A texture as a render type without blending shows it (`RenderType.entityCutout`, and Cobblemon's own
/// for a layer that is not translucent): a texel is there whole or is not there, by the entity shaders'
/// `alpha < 0.1`. Drawn where texels blend, the texture then looks as it does there.
pub fn cutout(rgba: &mut [u8]) {
    for texel in rgba.chunks_exact_mut(4) {
        // A tenth of 255 is 25.5.
        texel[3] = if texel[3] < 26 { 0 } else { 255 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image whose every texel says which image it is of (red) and where in it (green, blue).
    fn image(tag: u8, width: u32, height: u32) -> Image {
        let rgba = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [tag, x as u8, y as u8, 255]))
            .collect();
        Image {
            width,
            height,
            rgba,
        }
    }

    /// The texel of `sheet` at `x`, `y`.
    fn texel(sheet: &Sheet, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * sheet.image.width + x) * 4) as usize;
        sheet.image.rgba[at..at + 4].try_into().unwrap()
    }

    /// Every texel of every image is where its cell says, whatever the layout.
    fn holds(sheets: &[Sheet], images: &[Image]) {
        let mut seen = vec![false; images.len()];
        for sheet in sheets {
            for cell in &sheet.cells {
                let image = &images[cell.image];
                assert_eq!((cell.width, cell.height), (image.width, image.height));
                assert!(!std::mem::replace(&mut seen[cell.image], true));
                for y in 0..image.height {
                    for x in 0..image.width {
                        let at = ((y * image.width + x) * 4) as usize;
                        assert_eq!(
                            texel(sheet, cell.x + x, cell.y + y),
                            image.rgba[at..at + 4],
                            "image {} at {x}, {y}",
                            cell.image
                        );
                    }
                }
            }
        }
        assert!(seen.iter().all(|&seen| seen));
    }

    #[test]
    fn a_coat_and_its_four_flames_take_one_slot() {
        // Ponyta: five textures of 64 by 64, in a slot of 256.
        let images: Vec<Image> = (0..5).map(|tag| image(tag, 64, 64)).collect();
        let sheets = pack(&images, 256).unwrap();
        assert_eq!(sheets.len(), 1);
        let sheet = &sheets[0];
        // Four to a row, the fifth under the first; the sheet is no larger than that.
        assert_eq!((sheet.image.width, sheet.image.height), (256, 128));
        let places: Vec<(usize, u32, u32)> =
            sheet.cells.iter().map(|c| (c.image, c.x, c.y)).collect();
        assert_eq!(
            places,
            [(0, 0, 0), (1, 64, 0), (2, 128, 0), (3, 192, 0), (4, 0, 64)]
        );
        holds(&sheets, &images);
        // Beside the fifth, nothing.
        assert_eq!(texel(sheet, 64, 64), [0; 4]);
        assert_eq!(texel(sheet, 255, 127), [0; 4]);
    }

    #[test]
    fn a_lone_texture_is_its_own_sheet() {
        let images = [image(7, 128, 64)];
        let sheets = pack(&images, 256).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].image, images[0]);
        assert_eq!(
            sheets[0].cells,
            [Cell {
                image: 0,
                x: 0,
                y: 0,
                width: 128,
                height: 64
            }]
        );
    }

    #[test]
    fn what_one_slot_cannot_hold_goes_on_to_the_next() {
        // Charizard: its coat and four flames, 256 by 128 each once shrunk: two to a slot.
        let images: Vec<Image> = (0..5).map(|tag| image(tag, 256, 128)).collect();
        let sheets = pack(&images, 256).unwrap();
        let sizes: Vec<(u32, u32, usize)> = sheets
            .iter()
            .map(|s| (s.image.width, s.image.height, s.cells.len()))
            .collect();
        assert_eq!(sizes, [(256, 256, 2), (256, 256, 2), (256, 128, 1)]);
        // The first image is the first sheet's top left corner; images keep their numbers across sheets.
        assert_eq!((sheets[0].cells[0].x, sheets[0].cells[0].y), (0, 0));
        assert_eq!((sheets[0].cells[1].x, sheets[0].cells[1].y), (0, 128));
        assert_eq!(sheets[2].cells[0].image, 4);
        holds(&sheets, &images);
        // A slot's worth each fills its slot alone.
        let full: Vec<Image> = (0..3).map(|tag| image(tag, 256, 256)).collect();
        let sheets = pack(&full, 256).unwrap();
        assert_eq!(sheets.len(), 3);
        holds(&sheets, &full);
    }

    #[test]
    fn images_of_several_sizes_share_the_cells_of_the_largest() {
        // A layer smaller than the coat it is drawn over (four looks of Cobblemon 1.8.1 have one).
        let images = [image(1, 128, 64), image(2, 64, 64), image(3, 32, 128)];
        let sheets = pack(&images, 256).unwrap();
        assert_eq!(sheets.len(), 1);
        // Cells of 128 by 128: two to a row.
        let places: Vec<(u32, u32)> = sheets[0].cells.iter().map(|c| (c.x, c.y)).collect();
        assert_eq!(places, [(0, 0), (128, 0), (0, 128)]);
        assert_eq!((sheets[0].image.width, sheets[0].image.height), (256, 256));
        holds(&sheets, &images);
        // Past the smaller image, its cell is empty.
        assert_eq!(texel(&sheets[0], 128 + 64, 0), [0; 4]);
        assert_eq!(texel(&sheets[0], 0, 64), [0; 4]);
    }

    #[test]
    fn nothing_is_packed_of_what_is_not_an_image_of_a_slot() {
        assert_eq!(pack(&[], 256), None);
        assert_eq!(pack(&[image(0, 257, 16)], 256), None);
        assert_eq!(pack(&[image(0, 16, 16), image(1, 16, 512)], 256), None);
        let mut short = image(0, 16, 16);
        short.rgba.pop();
        assert_eq!(pack(&[short], 256), None);
        let empty = Image {
            width: 0,
            height: 4,
            rgba: Vec::new(),
        };
        assert_eq!(pack(&[empty], 256), None);
    }

    #[test]
    fn a_cutout_texel_is_whole_or_gone() {
        let mut rgba = Vec::new();
        for alpha in [0, 1, 25, 26, 128, 254, 255] {
            rgba.extend([10, 20, 30, alpha]);
        }
        cutout(&mut rgba);
        let alphas: Vec<u8> = rgba.chunks_exact(4).map(|texel| texel[3]).collect();
        assert_eq!(alphas, [0, 0, 0, 255, 255, 255, 255]);
        // Its colour stays.
        assert!(rgba.chunks_exact(4).all(|texel| texel[..3] == [10, 20, 30]));
    }
}
