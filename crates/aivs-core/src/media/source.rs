//! Validate inference inputs before a worker loads model weights.
use super::*;
use crate::artifacts::{file_hash, hash};
use image::ImageReader;
use std::io::{Cursor, Seek, SeekFrom};

pub(crate) fn image(source: &Path) -> Result<Value> {
    let bytes = fs::read(source)
        .map_err(|e| format!("Cannot read reference image {}: {e}", source.display()))?;
    let picture = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| format!("Cannot decode reference image {}: {e}", source.display()))?;
    Ok(json!({"source_file": source, "source_sha256": hash(&bytes),
        "width": picture.width(), "height": picture.height(),
        "conditioning": "whole-first-frame", "strength": 1.0}))
}

pub(super) fn anchors(args: &Value) -> Result<Value> {
    let anchors = args["anchors"]
        .as_array()
        .filter(|a| (2..=4).contains(&a.len()))
        .ok_or("Motion conditioning requires two to four image anchors.")?;
    let frames = integer(args, "frames")?;
    let width = integer(args, "width")?;
    let height = integer(args, "height")?;
    let mut result = Vec::<Value>::new();
    for anchor in anchors {
        let frame = anchor["frame"]
            .as_u64()
            .filter(|f| *f < frames)
            .ok_or("Image anchors need ordered, distinct output frames and strengths in (0, 1].")?;
        let strength = anchor["strength"]
            .as_f64()
            .filter(|s| s.is_finite() && *s > 0.0 && *s <= 1.0)
            .ok_or("Image anchors need ordered, distinct output frames and strengths in (0, 1].")?;
        if result
            .last()
            .is_some_and(|row| row["frame"].as_u64().unwrap_or(0) >= frame)
        {
            return Err(
                "Image anchors need ordered, distinct output frames and strengths in (0, 1]."
                    .into(),
            );
        }
        let mut info = image(&path(anchor, "file")?)?;
        // Preview reuses the validated full-resolution anchors; the runtime
        // resizes image conditioning to each inference stage's canvas.
        if args["allow_resize"] != true && (info["width"] != width || info["height"] != height) {
            return Err("Image anchors must match the model dimensions.".into());
        }
        if info["source_sha256"] != anchor["sha256"] {
            return Err("An image anchor changed before rendering.".into());
        }
        info["frame"] = json!(frame);
        info["strength"] = json!(strength);
        info["conditioning"] = json!(if frame == 0 { "replace" } else { "guide" });
        result.push(info);
    }
    if result[0]["frame"] != 0 || result.last().unwrap()["frame"] != frames - 1 {
        return Err("Image anchors must include frame zero and the final output frame.".into());
    }
    let mut hashes = std::collections::HashSet::new();
    if result
        .iter()
        .any(|row| !hashes.insert(row["source_sha256"].as_str().unwrap_or_default()))
    {
        return Err("Motion anchors must contain distinct images.".into());
    }
    Ok(json!(result))
}

/// Read RIFF chunk headers without holding a soundtrack in memory.
pub(super) fn audio(args: &Value) -> Result<Value> {
    let source = path(args, "source")?;
    let minimum = args["minimum_duration"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or("Invalid minimum audio duration.")?;
    let mut stream =
        fs::File::open(&source).map_err(|e| format!("Cannot read fixed audio: {e}"))?;
    let size = stream.metadata().map_err(|e| e.to_string())?.len();
    let mut header = [0_u8; 12];
    stream
        .read_exact(&mut header)
        .map_err(|_| "Cannot read fixed audio.")?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err("Fixed audio must be mono or stereo 16-bit PCM WAV.".into());
    }
    let mut format = None;
    let mut data = None;
    while stream.stream_position().map_err(|e| e.to_string())? + 8 <= size {
        let mut chunk = [0_u8; 8];
        stream.read_exact(&mut chunk).map_err(|e| e.to_string())?;
        let length = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as u64;
        let start = stream.stream_position().map_err(|e| e.to_string())?;
        if start + length > size {
            return Err("Fixed audio is truncated.".into());
        }
        if &chunk[..4] == b"fmt " {
            if length < 16 {
                return Err("Cannot read fixed audio format.".into());
            }
            let mut fmt = [0_u8; 16];
            stream.read_exact(&mut fmt).map_err(|e| e.to_string())?;
            let encoding = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            if encoding != 1 || !(1..=2).contains(&channels) || bits != 16 || rate == 0 {
                return Err("Fixed audio must be mono or stereo 16-bit PCM WAV.".into());
            }
            format = Some((rate, channels));
        } else if &chunk[..4] == b"data" {
            data = Some(length);
            break;
        }
        stream
            .seek(SeekFrom::Start(start + length + length % 2))
            .map_err(|e| e.to_string())?;
    }
    let (rate, channels) = format.ok_or("Cannot read fixed audio format.")?;
    let bytes = data.ok_or("Cannot read fixed audio data.")?;
    let block = u64::from(channels) * 2;
    if bytes % block != 0 {
        return Err("Fixed audio is truncated.".into());
    }
    let duration = (bytes / block) as f64 / f64::from(rate);
    if duration + 1.0 / f64::from(rate) < minimum {
        return Err(
            "Fixed audio is shorter than the requested video. Pad the shot track with silence."
                .into(),
        );
    }
    Ok(
        json!({"source_file": source, "source_sha256": file_hash(&source)?,
        "sample_rate": rate, "duration_seconds": duration, "conditioning": "frozen-audio"}),
    )
}
