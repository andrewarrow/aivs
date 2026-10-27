//! Reference consolidation and normalized output pixels, without Python/Pillow.
use crate::{
    artifacts::{self, *},
    continuity::Result,
};
use image::{
    DynamicImage, GenericImageView, ImageDecoder, ImageReader, Rgb, RgbImage,
    imageops::{self, FilterType},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

pub const MAX_INPUTS: usize = 5;

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    Ok(image)
}

fn png(image: RgbImage) -> Result<Vec<u8>> {
    let mut buffer = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buffer.into_inner())
}

/// Pillow's contain rounds the adjusted dimension and never crops content.
fn contain(image: &RgbImage, width: u32, height: u32) -> RgbImage {
    let ratio = image.width() as f64 / image.height() as f64;
    let (w, h) = if ratio > width as f64 / height as f64 {
        (
            width,
            (width as f64 / ratio).round_ties_even().max(1.0) as u32,
        )
    } else {
        (
            (height as f64 * ratio).round_ties_even().max(1.0) as u32,
            height,
        )
    };
    imageops::resize(image, w.min(width), h.min(height), FilterType::Lanczos3)
}

// Small fixed glyphs keep source-image numbers visible without system fonts.
pub(crate) fn label(canvas: &mut RgbImage, x: u32, y: u32, text: &str, color: Rgb<u8>, scale: u32) {
    for (index, c) in text.chars().enumerate() {
        let rows = match c {
            'I' => [31, 4, 4, 4, 4, 4, 31],
            'm' => [0, 0, 26, 21, 21, 21, 21],
            'a' => [0, 0, 14, 1, 15, 17, 15],
            'g' => [0, 15, 17, 17, 15, 1, 14],
            'e' => [0, 0, 14, 17, 31, 16, 14],
            '0' => [14, 17, 19, 21, 25, 17, 14],
            'S' => [15, 16, 16, 14, 1, 1, 30],
            'h' => [16, 16, 30, 17, 17, 17, 17],
            'o' => [0, 0, 14, 17, 17, 17, 14],
            't' => [4, 4, 31, 4, 4, 4, 3],
            's' => [0, 0, 15, 16, 14, 1, 30],
            'f' => [6, 8, 8, 28, 8, 8, 8],
            'r' => [0, 0, 22, 25, 16, 16, 16],
            '|' => [4; 7],
            '.' => [0, 0, 0, 0, 0, 12, 12],
            '1' => [4, 12, 4, 4, 4, 4, 14],
            '2' => [14, 17, 1, 2, 4, 8, 31],
            '3' => [30, 1, 1, 14, 1, 1, 30],
            '4' => [2, 6, 10, 18, 31, 2, 2],
            '5' => [31, 16, 16, 30, 1, 1, 30],
            '6' => [14, 16, 16, 30, 17, 17, 14],
            '7' => [31, 1, 2, 4, 8, 8, 8],
            '8' => [14, 17, 17, 14, 17, 17, 14],
            '9' => [14, 17, 17, 15, 1, 1, 14],
            _ => [0; 7],
        };
        for (dy, row) in rows.into_iter().enumerate() {
            for dx in 0..5 {
                if row & (1 << (4 - dx)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = x + (index as u32 * 6 + dx) * scale + sx;
                            let py = y + dy as u32 * scale + sy;
                            if px < canvas.width() && py < canvas.height() {
                                canvas.put_pixel(px, py, color);
                            }
                        }
                    }
                }
            }
        }
    }
}

pub struct Inputs {
    pub paths: Vec<PathBuf>,
    pub sources: Vec<Value>,
    pub mapping: String,
}

pub fn prepare(references: &[PathBuf], output: &Path, keep_first: bool) -> Result<Inputs> {
    let sources = references
        .iter()
        .map(|path| Ok(json!({"path": path, "sha256": file_hash(path)?})))
        .collect::<Result<Vec<_>>>()?;
    if references.len() <= MAX_INPUTS {
        return Ok(Inputs {
            paths: references.to_vec(),
            sources,
            mapping: if references.is_empty() {
                ""
            } else {
                "\nImage numbers in the brief match tool input numbers."
            }
            .into(),
        });
    }
    let minimum = usize::from(keep_first);
    let keep = (minimum..=3)
        .rev()
        .find(|keep| references.len() - *keep <= (MAX_INPUTS - *keep) * 4)
        .ok_or(
            "Too many essential references for five readable image inputs; simplify this shot.",
        )?;
    let folder = output
        .parent()
        .ok_or("Missing image directory.")?
        .join("reference-sheets");
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let mut paths = references[..keep].to_vec();
    let mut mapping = (1..=keep)
        .map(|n| format!("Image {n}: tool input {n}."))
        .collect::<Vec<_>>();
    for (group_index, group) in sources[keep..].chunks(4).enumerate() {
        let start = keep + group_index * 4;
        let path = folder.join(format!("{}.png", fingerprint(&json!(group))?));
        let metadata = path.with_extension("json");
        if path.exists() && !metadata.exists() {
            // A crash between image publication and its receipt cannot leave
            // this input permanently unusable. Preserve the unbound bytes.
            let interrupted = path.with_extension(format!("unverified-{}.png", timestamp()));
            fs::rename(&path, interrupted).map_err(|e| e.to_string())?;
        }
        if path.exists() {
            // Reuse original Pillow sheets verbatim. Re-encoding would change
            // their bytes and invalidate completed image-tool checkpoints.
            let saved = read_json(&metadata)?;
            if saved["sources"] != json!(group) || saved["sha256"] != file_hash(&path)? {
                return Err("A saved reference sheet changed.".into());
            }
        } else {
            let mut sheet = RgbImage::from_pixel(1536, 1600, Rgb([255; 3]));
            for (offset, source) in group.iter().enumerate() {
                let x = (offset % 2) as u32 * 768;
                let y = (offset / 2) as u32 * 800;
                let image = decode(
                    &fs::read(source["path"].as_str().ok_or("Missing reference path.")?)
                        .map_err(|e| e.to_string())?,
                )?
                .to_rgb8();
                let tile = contain(&image, 768, 768);
                imageops::replace(
                    &mut sheet,
                    &tile,
                    (x + (768 - tile.width()) / 2) as i64,
                    (y + 32 + (768 - tile.height()) / 2) as i64,
                );
                label(
                    &mut sheet,
                    x + 12,
                    y + 8,
                    &format!("Image {}", start + offset + 1),
                    Rgb([0; 3]),
                    2,
                );
            }
            let bytes = png(sheet)?;
            exclusive_bytes(&path, &bytes)?;
            atomic_json(
                &metadata,
                &json!({"sources": group, "sha256": hash(&bytes)}),
            )?;
        }
        for offset in 0..group.len() {
            mapping.push(format!(
                "Image {}: tool input {}, panel {} (panels read left to right, top to bottom).",
                start + offset + 1,
                paths.len() + 1,
                offset + 1
            ));
        }
        paths.push(path.canonicalize().map_err(|e| e.to_string())?);
    }
    Ok(Inputs { paths, sources, mapping: "\nReference mapping: all Image numbers in the brief identify source images, mapped to tool inputs/panels below. Sheets contain separate references; do not reproduce panels or labels.\n".to_owned() + &mapping.join("\n") })
}

pub fn normalize(bytes: &[u8], output: &Path, width: u32, height: u32) -> Result<Value> {
    let original = decode(bytes)?;
    let (source_width, source_height) = original.dimensions();
    let content = contain(&original.to_rgb8(), width, height);
    let (left, top) = (
        (width - content.width()) / 2,
        (height - content.height()) / 2,
    );
    let mut normalized = RgbImage::new(width, height);
    imageops::replace(&mut normalized, &content, left as i64, top as i64);
    let data = png(normalized)?;
    artifacts::exclusive_bytes(output, &data)?;
    Ok(
        json!({"normalized_content_box": [left, top, left+content.width(), top+content.height()],
        "source_width": source_width, "source_height": source_height, "sha256": hash(&data)}),
    )
}
