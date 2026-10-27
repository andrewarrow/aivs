//! Numbered visual evidence sampled at 4fps, including the exact inherited frame.
use super::*;
use crate::{artifacts::atomic_json, render_formats, storyboard_images::label};
use image::{Rgb, RgbImage, imageops};
use std::io::Write;

pub(super) fn sheets(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let clip = path(args, "clip")?;
    let directory = path(args, "directory")?;
    let seconds = args["seconds"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .ok_or("Invalid visual sampling duration.")?;
    let total = (seconds * FPS as f64) as u64;
    if total == 0 {
        return Err("Visual sampling needs at least one frame.".into());
    }
    let shot = integer(args, "shot_number")?;
    let frames_dir = directory.join("frames");
    fs::create_dir_all(&frames_dir).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(&frames_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 7
            && name.ends_with(".png")
            && name.as_bytes()[..3].iter().all(u8::is_ascii_digit)
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    let spec = if args["render_format"].is_null() {
        None
    } else {
        Some(render_formats::resolve(&args["render_format"])?)
    };
    let mut indices = (0..total).step_by(6).collect::<Vec<_>>();
    if indices.last() != Some(&(total - 1)) {
        indices.push(total - 1);
    }
    let crop = spec.as_ref().map_or(String::new(), |s| {
        format!("crop={}:{},", s["width"], s["height"])
    });
    let filter = format!(
        "select=lt(n\\,{total})*(not(mod(n\\,6))+eq(n\\,{})),{crop}scale=640:640:force_original_aspect_ratio=decrease:force_divisible_by=2",
        total - 1
    );
    run_timeout(
        ffmpeg()
            .arg("-i")
            .arg(&clip)
            .args(["-vf", &filter, "-fps_mode", "vfr"])
            .arg(frames_dir.join("%03d.png")),
        cancelled,
        Some(Duration::from_secs(30)),
    )?;
    let files = (1..=indices.len())
        .map(|number| frames_dir.join(format!("{number:03}.png")))
        .collect::<Vec<_>>();
    if !files.iter().all(|path| path.is_file()) {
        return Err("Visual sampling did not produce the expected frame sequence.".into());
    }
    let full_final = directory.join("full-final-frame.png");
    run_timeout(
        ffmpeg()
            .arg("-i")
            .arg(&clip)
            .args([
                "-vf",
                &format!("select=eq(n\\,{})", total - 1),
                "-frames:v",
                "1",
                "-fps_mode",
                "passthrough",
            ])
            .arg(&full_final),
        cancelled,
        Some(Duration::from_secs(30)),
    )?;
    if !full_final.is_file() {
        return Err("The inherited final frame could not be extracted.".into());
    }
    let opening = image::open(&files[0]).map_err(|e| e.to_string())?;
    let (width, height) = (opening.width(), opening.height());
    let mut sheets = Vec::new();
    for (page, subset) in files.chunks(12).enumerate() {
        let rows = subset.len().div_ceil(3) as u32;
        let mut sheet =
            RgbImage::from_pixel(3 * (width + 4) + 4, rows * (height + 32) + 4, Rgb([17; 3]));
        for (local, path) in subset.iter().enumerate() {
            let (x, y) = (
                4 + (local % 3) as u32 * (width + 4),
                4 + (local / 3) as u32 * (height + 32),
            );
            let frame = indices[page * 12 + local];
            label(
                &mut sheet,
                x + 5,
                y + 3,
                &format!(
                    "Shot {shot} | {:.3}s | frame {frame}",
                    frame as f64 / FPS as f64
                ),
                Rgb([255; 3]),
                2,
            );
            let frame_image = image::open(path).map_err(|e| e.to_string())?.to_rgb8();
            imageops::replace(&mut sheet, &frame_image, x as i64, (y + 28) as i64);
        }
        let target = directory.join(format!("contact-{:02}.jpg", page + 1));
        let mut temporary =
            tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut temporary, 92)
            .encode_image(&sheet)
            .map_err(|e| e.to_string())?;
        temporary.flush().map_err(|e| e.to_string())?;
        temporary.persist(&target).map_err(|e| e.to_string())?;
        sheets.push(target);
    }
    let evidence = json!({"sample_fps": FPS as f64/6.0, "contact_sheets": sheets, "final_frame": files.last(),
        "full_final_frame": full_final, "delivery_crop": spec.map(|s| json!({"width": s["width"], "height": s["height"]})),
        "frames": indices.iter().zip(&files).map(|(frame,path)| json!({"frame": frame, "seconds": *frame as f64/FPS as f64, "file": path})).collect::<Vec<_>>()});
    atomic_json(&directory.join("frames.json"), &evidence)?;
    Ok(evidence)
}
