//! Supplied PCM waveform identity after AAC encoding and channel conversion.
use super::*;

fn pcm(path: &Path, duration: f64, cancelled: &AtomicBool) -> Result<(Vec<u8>, u64, u64)> {
    let bytes = run(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,sample_rate,channels",
                "-of",
                "json",
            ])
            .arg(path),
        cancelled,
    )?;
    let probe: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let streams = probe["streams"]
        .as_array()
        .ok_or("Supplied audio has no streams.")?;
    if streams.len() != 1 || streams[0]["codec_name"] != "pcm_s16le" {
        return Err("Supplied audio must be uncompressed 16-bit little-endian PCM.".into());
    }
    let rate = probe_integer(&streams[0]["sample_rate"])
        .filter(|n| *n > 0)
        .ok_or("Invalid supplied audio sample rate.")?;
    let channels = probe_integer(&streams[0]["channels"])
        .filter(|n| *n > 0)
        .ok_or("Invalid supplied audio channel count.")?;
    let frames = (duration * rate as f64).round_ties_even();
    if !frames.is_finite() || frames <= 0.0 {
        return Err("Invalid supplied audio duration.".into());
    }
    // Trim by sample index, matching wave.readframes(round(duration * rate)).
    let reference = run(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(path)
            .args([
                "-map",
                "0:a:0",
                "-af",
                &format!("atrim=end_sample={frames:.0}"),
                "-f",
                "s16le",
                "pipe:1",
            ]),
        cancelled,
    )?;
    Ok((reference, rate, channels))
}

pub(super) fn validate(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let source = path(args, "source")?;
    let video = path(args, "video")?;
    let duration = args["duration"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .ok_or("Invalid supplied audio duration.")?;
    let (reference, rate, channels) = pcm(&source, duration, cancelled)?;
    let actual = run(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(video)
            .args([
                "-map",
                "0:a:0",
                "-t",
                &duration.to_string(),
                "-f",
                "s16le",
                "-ar",
                &rate.to_string(),
                "-ac",
                &channels.to_string(),
                "pipe:1",
            ]),
        cancelled,
    )?;
    if reference.is_empty() || actual.len() != reference.len() || reference.len() % 2 != 0 {
        return Err(
            "Fixed soundtrack validation failed: decoded duration differs from source.".into(),
        );
    }
    let (mut source_energy, mut output_energy, mut dot) = (0_i128, 0_i128, 0_i128);
    for (a, b) in reference.chunks_exact(2).zip(actual.chunks_exact(2)) {
        let a = i128::from(i16::from_le_bytes([a[0], a[1]]));
        let b = i128::from(i16::from_le_bytes([b[0], b[1]]));
        source_energy += a * a;
        output_energy += b * b;
        dot += a * b;
    }
    if source_energy == 0 {
        if output_energy != 0 {
            return Err("Fixed soundtrack validation failed: expected silence.".into());
        }
        return Ok(json!({"waveform_correlation": 1.0, "gain": 1.0, "samples": actual.len()/2}));
    }
    let correlation = if output_energy == 0 {
        0.0
    } else {
        dot as f64 / (source_energy as f64 * output_energy as f64).sqrt()
    };
    let gain = (output_energy as f64 / source_energy as f64).sqrt();
    if correlation < 0.995 || !(0.5..=2.0).contains(&gain) {
        return Err(
            "Fixed soundtrack validation failed: output differs from the supplied performance."
                .into(),
        );
    }
    Ok(json!({"waveform_correlation": correlation, "gain": gain, "samples": actual.len()/2}))
}
