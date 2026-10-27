//! Time-weighted progress and compatible history budgets; inference reports work.
use crate::{artifacts::read_json, continuity::Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
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

fn defaults() -> Vec<(String, f64)> {
    PHASES
        .iter()
        .map(|(key, seconds)| ((*key).into(), *seconds))
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
        }
    }

    fn tick(&mut self, now: f64, enabled: bool) -> Option<Value> {
        let phase = self.phase.as_ref()?;
        if !enabled {
            return None;
        }
        let elapsed = (now - self.started).max(0.0);
        let active_elapsed = (now - self.phase_started).max(0.0);
        let observed_budget: f64 = self
            .budgets
            .iter()
            .filter(|(k, _)| self.phase_seconds.contains_key(k))
            .map(|(_, v)| v)
            .sum();
        let scale = if observed_budget > 0.0 {
            self.phase_seconds.values().sum::<f64>() / observed_budget
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
                let measured = active_elapsed / self.fraction;
                let confidence = (self.fraction * 3.0).min(1.0);
                budget = budget * (1.0 - confidence) + measured * confidence;
                (budget - active_elapsed).max(measured * (1.0 - self.fraction))
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

fn history(args: &Value) -> Result<Value> {
    let directory = args["outputs"]
        .as_str()
        .ok_or("Missing estimate history directory.")?;
    let mut paths = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("render.json"))
        .filter(|p| p.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths.reverse();
    let total_default: f64 = PHASES.iter().map(|(_, v)| v).sum();
    let mut samples = Vec::new();
    for path in paths {
        let Ok(report) = read_json(&path) else {
            continue;
        };
        let runtime = &report["runtime"];
        if report["status"] != "complete"
            || report["width"] != args["width"]
            || report["height"] != args["height"]
            || runtime["output_frames"] != args["frames"]
            || runtime["runtime_revision"] != args["revision"]
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
        let sample = PHASES
            .iter()
            .map(|(key, default)| {
                phases
                    .get(*key)
                    .map(numeric)
                    .unwrap_or(Some(default * total / total_default))
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
        return Ok(json!(defaults()));
    }
    let budgets = PHASES
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
            ((*key).to_owned(), median)
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
        "render_estimate_defaults" => Ok(json!(defaults())),
        "render_estimate_history" => history(args),
        "render_estimate_new" => {
            let budgets = if args["budgets"].is_null() {
                defaults()
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
                    state.fraction = state.fraction.max(fraction.min(1.0));
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
