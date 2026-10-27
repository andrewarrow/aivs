//! Delivery dimensions, aligned model canvases, and matching framing guidance.
use crate::continuity::Result;
use serde_json::{Value, json};

pub fn resolve(name: &Value) -> Result<Value> {
    let (width, height, aspect) = match name.as_str() {
        Some("landscape") => (1280_u64, 720_u64, "16:9"),
        Some("portrait") => (1080_u64, 1920_u64, "9:16"),
        _ => {
            return Err(
                "Render format must be landscape (1280×720) or portrait (1080×1920).".into(),
            );
        }
    };
    Ok(json!({
        "name": name, "width": width, "height": height, "aspect_ratio": aspect,
        "model_width": width.div_ceil(64) * 64,
        "model_height": height.div_ceil(64) * 64,
    }))
}

/// Keep delivery aspect ratios while reducing inference, not just the encoded file.
pub fn for_profile(name: &Value, profile: &str) -> Result<Value> {
    let mut spec = resolve(name)?;
    if profile == "distilled" {
        let (width, height) = if name == "portrait" {
            (180_u64, 320_u64)
        } else {
            (320_u64, 180_u64)
        };
        spec["width"] = json!(width);
        spec["height"] = json!(height);
        spec["model_width"] = json!(width.div_ceil(64) * 64);
        spec["model_height"] = json!(height.div_ceil(64) * 64);
    }
    Ok(spec)
}

pub fn framing(name: &Value) -> Value {
    json!(if name == "portrait" {
        "Compose for a vertical 9:16 portrait frame. Keep faces, active hands, and essential props inside the narrow central frame. "
    } else {
        "Compose for a horizontal 16:9 landscape frame. Keep faces, active hands, and essential props clear of the top and bottom edges. "
    })
}
