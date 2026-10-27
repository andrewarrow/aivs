//! Time-weighted progress and compatible history budgets; inference reports work.
use crate::{artifacts::read_json, continuity::Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const PHASES: [(&str, f64); 12] = [
    ("runtime", 3.0),
    ("cache", 1.0),
    ("pipeline", 20.0),
    ("prompt", 45.0),
    ("stage1", 150.0),
    ("upscale", 5.0),
    ("refine", 5.0),
    ("stage2", 180.0),
    ("decoders", 5.0),
    ("decode", 65.0),
    ("encode", 3.0),
    ("validate", 2.0),
];

fn defaults(profile: &str) -> Vec<(String, f64)> {
    PHASES
        .iter()
        .filter(|(key, _)| profile != "hq" || !matches!(*key, "refine" | "validate"))
        .map(|(key, seconds)| {
            let seconds = if profile == "hq" {
                match *key {
                    // res2s: two evaluations per step plus a terminal one;
                    // stage 1 has conditioned, CFG and modality-isolation passes.
                    "stage1" => seconds * (15.0 * 2.0 + 1.0) * 3.0 / 8.0,
                    "stage2" => seconds * (3.0 * 2.0 + 1.0) / 3.0,
                    "upscale" => 10.0,
                    _ => *seconds,
                }
            } else {
                *seconds
            };
            ((*key).into(), seconds)
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Estimate {
    // Array preserves pipeline order, including custom budgets, across JSON.
    budgets: Vec<(String, f64)>,
    started: f64,
    phase: Option<String>,
    phase_started: f64,
    fraction: f64,
    progress_at: f64,
    label: String,
    phase_seconds: BTreeMap<String, f64>,
    completed: BTreeSet<String>,
    last_fraction: f64,
}

impl Estimate {
    fn finish(&mut self, key: &str, now: f64) {
        if self.phase.as_deref() == Some(key) && !self.completed.contains(key) {
            self.phase_seconds
                .insert(key.into(), (now - self.phase_started).max(0.001));
            self.completed.insert(key.into());
            self.fraction = 1.0;
            self.progress_at = now;
        }
    }

    fn tick(&mut self, now: f64, enabled: bool) -> Option<Value> {
        let phase = self.phase.as_ref()?;
        if !enabled {
            return None;
        }
        let elapsed = (now - self.started).max(0.0);
        let active_elapsed = (now - self.phase_started).max(0.0);
        // Loading and cache checks vary independently of GPU work. Only
        // completed denoising stages calibrate later rendering stages.
        let observed_budget: f64 = self
            .budgets
            .iter()
            .filter(|(k, _)| {
                matches!(k.as_str(), "stage1" | "stage2") && self.phase_seconds.contains_key(k)
            })
            .map(|(_, v)| v)
            .sum();
        let scale = if observed_budget > 0.0 {
            self.phase_seconds
                .iter()
                .filter(|(k, _)| matches!(k.as_str(), "stage1" | "stage2"))
                .map(|(_, seconds)| seconds)
                .sum::<f64>()
                / observed_budget
        } else {
            1.0
        }
        .clamp(0.25, 4.0);
        let mut remaining: f64 = self
            .budgets
            .iter()
            .filter(|(k, _)| !self.completed.contains(k) && k != phase)
            .map(|(_, v)| v * scale)
            .sum();
        if !self.completed.contains(phase) {
            let mut budget = self.budgets.iter().find(|(k, _)| k == phase)?.1 * scale;
            remaining += if self.fraction > 0.0 {
                // A heartbeat is not newly completed work. Freeze measured
                // throughput at the last work update and count down from it.
                let measured = (self.progress_at - self.phase_started).max(0.001) / self.fraction;
                let confidence = (self.fraction * 3.0).min(1.0);
                budget = budget * (1.0 - confidence) + measured * confidence;
                (budget.max(measured) - active_elapsed).max(1.0)
            } else {
                (budget - active_elapsed).max(budget * 0.1)
            };
        }
        let fraction = if elapsed + remaining > 0.0 {
            elapsed / (elapsed + remaining)
        } else {
            0.0
        };
        self.last_fraction = self.last_fraction.max(fraction.min(0.99));
        Some(
            json!({"label": self.label, "fraction": self.last_fraction, "remaining_seconds": remaining.max(1.0)}),
        )
    }
}

fn directories(root: &Path) -> Vec<std::path::PathBuf> {
    let mut directories = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    directories.sort();
    directories.reverse();
    directories
}

fn history(args: &Value) -> Result<Value> {
    let directory = args["outputs"]
        .as_str()
        .ok_or("Missing estimate history directory.")?;
    let profile = args["profile"].as_str().unwrap_or("distilled");
    let audio = args["audio"].as_str().unwrap_or("generated");
    let defaults = defaults(profile);
    // Scan known report locations, not arbitrary storyboard/review subtrees.
    // Shot reports carry inference timings; scene reports carry aggregate work.
    let paths = directories(Path::new(directory))
        .into_iter()
        .flat_map(|run| {
            let mut paths = directories(&run.join("shots"))
                .into_iter()
                .map(|shot| shot.join("render.json"))
                .collect::<Vec<_>>();
            paths.push(run.join("render.json"));
            paths
        });
    let mut samples = Vec::new();
    for path in paths {
        let Ok(report) = read_json(&path) else {
            continue;
        };
        let runtime = &report["runtime"];
        // A completed inference receipt is useful even if later visual review
        // found issues or a subsequent production step failed.
        if report["width"] != args["width"]
            || report["height"] != args["height"]
            || runtime["output_frames"] != args["frames"]
            || runtime["runtime_revision"] != args["revision"]
            || runtime["profile"].as_str().unwrap_or("distilled") != profile
            || runtime["audio"].as_str().unwrap_or("generated") != audio
        {
            continue;
        }
        let Some(total) = numeric(&runtime["elapsed_seconds"]) else {
            continue;
        };
        let phases = runtime
            .get("phase_seconds")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !phases.is_object() {
            continue;
        }
        let observed: f64 = defaults
            .iter()
            .filter_map(|(key, _)| numeric(&phases[key]))
            .sum();
        let missing_budget: f64 = defaults
            .iter()
            .filter(|(key, _)| phases.get(key).is_none())
            .map(|(_, seconds)| seconds)
            .sum();
        let sample = defaults
            .iter()
            .map(|(key, default)| {
                phases.get(key).map(numeric).unwrap_or_else(|| {
                    Some(default * (total - observed).max(0.001) / missing_budget.max(0.001))
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(sample) = sample {
            samples.push(sample);
            if samples.len() == 5 {
                break;
            }
        }
    }
    if samples.is_empty() {
        return Ok(json!(defaults));
    }
    let budgets = defaults
        .iter()
        .enumerate()
        .map(|(index, (key, _))| {
            let mut values = samples.iter().map(|s| s[index]).collect::<Vec<_>>();
            values.sort_by(f64::total_cmp);
            let mid = values.len() / 2;
            let median = if values.len() % 2 == 0 {
                (values[mid - 1] + values[mid]) / 2.0
            } else {
                values[mid]
            };
            (key.to_owned(), median)
        })
        .collect::<Vec<_>>();
    Ok(json!(budgets))
}

fn numeric(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "render_estimate_defaults" => Ok(json!(defaults(
            args["profile"].as_str().unwrap_or("distilled")
        ))),
        "render_estimate_history" => history(args),
        "render_estimate_new" => {
            let budgets = if args["budgets"].is_null() {
                defaults(args["profile"].as_str().unwrap_or("distilled"))
            } else {
                serde_json::from_value::<Vec<(String, f64)>>(args["budgets"].clone())
                    .map_err(|e| e.to_string())?
            };
            let mut keys = BTreeSet::new();
            if budgets.is_empty()
                || budgets.iter().any(|(k, v)| {
                    k.is_empty() || !v.is_finite() || *v <= 0.0 || !keys.insert(k.clone())
                })
            {
                return Err(
                    "Render budgets need distinct phase names and positive finite seconds.".into(),
                );
            }
            let now = args["now"]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("Invalid estimate clock.")?;
            serde_json::to_value(Estimate {
                budgets,
                started: now,
                phase: None,
                phase_started: now,
                fraction: 0.0,
                progress_at: now,
                label: "Starting renderer".into(),
                phase_seconds: BTreeMap::new(),
                completed: BTreeSet::new(),
                last_fraction: 0.0,
            })
            .map_err(|e| e.to_string())
        }
        "render_estimate_step" => {
            let mut state: Estimate =
                serde_json::from_value(args["state"].clone()).map_err(|e| e.to_string())?;
            let now = args["now"]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("Invalid estimate clock.")?;
            let enabled = args["enabled"] == true;
            let mut events = Vec::new();
            match args["action"].as_str().ok_or("Missing estimate action.")? {
                "start" => {
                    let key = args["key"].as_str().ok_or("Missing estimate phase.")?;
                    let index = state
                        .budgets
                        .iter()
                        .position(|(k, _)| k == key)
                        .ok_or("Unknown render phase.")?;
                    if let Some(previous) = state.phase.clone() {
                        if !state.completed.contains(&previous) {
                            state.finish(&previous, now);
                            if let Some(event) = state.tick(now, enabled) {
                                events.push(event);
                            }
                        }
                    }
                    state
                        .completed
                        .extend(state.budgets[..index].iter().map(|(k, _)| k.clone()));
                    state.phase = Some(key.into());
                    state.phase_started = now;
                    state.progress_at = now;
                    state.fraction = 0.0;
                    state.label = args["label"]
                        .as_str()
                        .ok_or("Missing estimate label.")?
                        .into();
                }
                "update" => {
                    let fraction = args["fraction"]
                        .as_f64()
                        .filter(|v| v.is_finite())
                        .ok_or("Invalid work fraction.")?;
                    if fraction > state.fraction {
                        state.fraction = fraction.min(1.0);
                        state.progress_at = now;
                    }
                }
                "finish" => {
                    let key = args["key"].as_str().ok_or("Missing estimate phase.")?;
                    if state.phase.as_deref() != Some(key) || state.completed.contains(key) {
                        return Ok(json!({"state": state, "events": events}));
                    }
                    state.finish(key, now);
                }
                "tick" => (),
                _ => return Err("Unknown estimate action.".into()),
            }
            if let Some(event) = state.tick(now, enabled) {
                events.push(event);
            }
            Ok(json!({"state": state, "events": events}))
        }
        _ => Err("Unknown render estimate operation.".into()),
    }
}
