//! Reference selection, drawable state, and immutable storyboard asset validation.
use crate::{artifacts, continuity::Result, media, prop_reference};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const POLICY: &str = "per-shot-first-frame-v2";
const HIDDEN: &[&str] = &["unknown", "not-visible", "concealed", "offscreen", "absent"];

fn rows<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>> {
    value.as_array().ok_or_else(|| format!("Missing {label}."))
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
fn visible(value: &Value) -> bool {
    !HIDDEN.contains(&text(value))
}

pub fn resolve_image(binding: &Value, root: &Path) -> Result<PathBuf> {
    let asset = &binding["image"];
    if !asset.is_object() {
        return Err("Each storyboard shot needs an image asset.".into());
    }
    let filename = asset["file"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && !s.contains('\0') && !Path::new(s).is_absolute())
        .ok_or("Storyboard image paths must be relative to the run directory.")?;
    let digest = asset["sha256"]
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or("Storyboard images need a lowercase SHA-256 digest.")?;
    let root = crate::project::image_root(asset, root)?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let path = root
        .join(filename)
        .canonicalize()
        .map_err(|_| format!("Storyboard image is missing: {filename}"))?;
    if !path.starts_with(&root) {
        return Err("Storyboard images must remain inside their asset directory.".into());
    }
    if !path.is_file() {
        return Err(format!("Storyboard image is missing: {filename}"));
    }
    if artifacts::file_hash(&path)? != digest {
        return Err(format!("Storyboard image changed: {filename}"));
    }
    Ok(path)
}

fn prepare(args: &Value, root: &Path) -> Result<Value> {
    let manifest = &args["manifest"];
    if manifest["version"].as_u64() != Some(1) {
        return Err("Storyboard manifests must use version 1.".into());
    }
    let shots = rows(&args["plan"]["shots"], "planned shots")?;
    let entries = rows(&manifest["shots"], "storyboard shots")?;
    if entries.len() != shots.len() {
        return Err("A storyboard must provide exactly one frame for every planned shot.".into());
    }
    let mut bindings = BTreeMap::new();
    for entry in entries {
        let number = entry["shot"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0 && *n <= shots.len())
            .ok_or("Storyboard shot numbers must cover the plan once, starting at 1.")?;
        if bindings.contains_key(&number) {
            return Err("Storyboard shot numbers must cover the plan once, starting at 1.".into());
        }
        let fingerprint = &args["fingerprints"][number - 1];
        if !fingerprint.is_string() || entry["planned_shot_sha256"] != *fingerprint {
            return Err(format!(
                "Storyboard frame {number} belongs to a different planned shot."
            ));
        }
        let path = resolve_image(entry, root)?;
        let decoded = media::source_image(&path)?;
        if decoded["source_sha256"] != entry["image"]["sha256"] {
            return Err(format!(
                "Storyboard image changed while preparing shot {number}."
            ));
        }
        bindings.insert(number, json!({"shot": number, "planned_shot_sha256": fingerprint,
            "image": {"file": path.strip_prefix(root).map_err(|e| e.to_string())?.to_string_lossy(),
            "sha256": decoded["source_sha256"], "width": decoded["width"], "height": decoded["height"]}}));
    }
    Ok(json!(bindings.into_values().collect::<Vec<_>>()))
}

pub fn reference_specs(plan: &Value) -> Result<Value> {
    let world = &plan["world"];
    let mut specs = Vec::new();
    for item in rows(&world["environments"], "canonical environments")? {
        let mut spec = json!({"id": format!("environment:{}", item["scene_id"]), "kind": "environment", "definition": item,
            "prompt": format!("One clean cinematic environment plate. No people, body parts, loose garments or movable story props. {}", text(&item["reference_prompt"]))});
        if let Some(set) = plan["source"].get("set") {
            crate::sets::validate(set)?;
            spec["assigned_set"] = set.clone();
            spec["prompt"] = json!(format!(
                "Preserve the attached assigned set {} exactly: its architecture, permanent furniture, materials and spatial layout. {} Repair only image defects or remove people, loose garments and movable story props. Keep this same location. {}",
                text(&set["name"]),
                text(&set["description"]),
                text(&item["reference_prompt"])
            ));
        }
        specs.push(spec);
    }
    for item in rows(&world["props"], "canonical props")? {
        specs.push(json!({"id": format!("prop:{}", text(&item["id"])), "kind": "prop", "definition": item, "prompt": prop_reference::generation_prompt(item, None)?}));
    }
    for item in rows(&world["characters"], "canonical characters")? {
        if item["morphology"].as_array().is_some_and(|a| !a.is_empty()) {
            specs.push(json!({"id": format!("body:{}", text(&item["role"])), "kind": "body", "definition": item,
                "prompt": format!("Full body neutral character reference photograph. Preserve the attached performer's identity, plausible body proportions and neutral fitted clothing. {}", text(&item["reference_prompt"]))}));
        }
    }
    Ok(json!(specs))
}

fn prepare_references(args: &Value, root: &Path) -> Result<Value> {
    let specs = rows(&args["specs"], "canonical reference specifications")?;
    let references = rows(&args["boards"]["references"], "canonical references")?;
    let expected: BTreeSet<_> = specs.iter().map(|s| text(&s["id"])).collect();
    if references.len() != expected.len()
        || references
            .iter()
            .map(|r| text(&r["id"]))
            .collect::<BTreeSet<_>>()
            != expected
    {
        return Err("Storyboard references do not cover the canonical physical world.".into());
    }
    for entry in references {
        let id = text(&entry["id"]);
        let spec = specs
            .iter()
            .find(|s| s["id"] == entry["id"])
            .ok_or("Unknown physical reference.")?;
        validate_reference(args, spec, entry, root)?;
        if let Some(inputs) = entry["generation"]["inputs"].as_array() {
            for input in inputs {
                resolve_image(&json!({"image": input}), root)?;
            }
        }
        if id.is_empty() {
            return Err("Invalid physical reference ID.".into());
        }
    }
    Ok(json!(references))
}

fn validate_reference(args: &Value, spec: &Value, entry: &Value, root: &Path) -> Result<()> {
    let id = text(&spec["id"]);
    if entry["kind"] != spec["kind"] || entry["definition_sha256"] != args["spec_hashes"][id] {
        return Err("Storyboard reference belongs to a different physical definition.".into());
    }
    resolve_image(entry, root)?;
    if spec["kind"] == "prop" {
        prop_reference::require_accepted(spec, entry, &args["prop_bindings"][id])?;
    }
    Ok(())
}

pub fn visible_state_prompt(plan: &Value, shot: &Value) -> Result<String> {
    let state = &shot["continuity"]["opening"];
    let reveals: BTreeSet<_> = state["reveals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !visible(&r["value"]))
        .map(|r| text(&r["id"]))
        .collect();
    let mut clauses = Vec::new();
    for character in state["characters"].as_array().into_iter().flatten() {
        if character["presence"] != "visible" {
            continue;
        }
        let visibility: BTreeMap<_, _> = rows(&character["visibility"], "character visibility")?
            .iter()
            .map(|a| (text(&a["key"]), &a["value"]))
            .collect();
        let mut details: Vec<_> = ["costume", "posture", "location"]
            .into_iter()
            .filter(|k| visible(&character[*k]))
            .map(|k| format!("{k} {}", text(&character[k])))
            .collect();
        for category in ["appearance", "morphology"] {
            for attribute in rows(&character[category], "character attributes")? {
                let key = text(&attribute["key"]);
                let exposed = visibility
                    .get(format!("{category}.{key}").as_str())
                    .or_else(|| visibility.get(key))
                    .copied();
                if !visible(&attribute["value"])
                    || reveals.contains(key)
                    || exposed.is_some_and(|v| !visible(v))
                {
                    continue;
                }
                if category == "morphology" && exposed.is_none_or(|v| *v != "visible") {
                    continue;
                }
                details.push(format!("{key} {}", text(&attribute["value"])));
            }
        }
        clauses.push(format!(
            "{}: {}",
            text(&character["role"]),
            details.join("; ")
        ));
    }
    let mut canonical = BTreeMap::new();
    for prop in plan["world"]["props"].as_array().into_iter().flatten() {
        canonical.insert(text(&prop["id"]), (prop, prop));
        for component in rows(&prop["components"], "prop components")? {
            canonical.insert(text(&component["id"]), (component, prop));
        }
    }
    let props: Vec<_> = state["props"].as_array().into_iter().flatten().collect();
    let visible_props: BTreeSet<_> = props
        .iter()
        .filter(|p| p["visibility"] == "visible")
        .map(|p| text(&p["id"]))
        .collect();
    for prop in props {
        let id = text(&prop["id"]);
        if !visible_props.contains(id) {
            continue;
        }
        let mut physical: Vec<(String, String)> = Vec::new();
        if let Some((definition, _)) = canonical.get(id) {
            for key in ["type", "shape", "size", "color"] {
                if let Some(value) = definition.get(key) {
                    physical.push((key.into(), text(value).into()));
                }
            }
            if let Some(features) = definition["features"].as_array().filter(|a| !a.is_empty()) {
                physical.push((
                    "distinguishing_features".into(),
                    features.iter().map(text).collect::<Vec<_>>().join("; "),
                ));
            }
        }
        for attribute in rows(&prop["physical"], "prop physical facts")? {
            let key = text(&attribute["key"]);
            if let Some((_, value)) = physical.iter_mut().find(|(k, _)| k == key) {
                *value = text(&attribute["value"]).into();
            } else {
                physical.push((key.into(), text(&attribute["value"]).into()));
            }
        }
        let mut details: Vec<_> = physical
            .iter()
            .filter(|(_, v)| !HIDDEN.contains(&v.as_str()))
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        if let Some((_, parent)) = canonical.get(id) {
            if parent["id"] != prop["id"] && !visible_props.contains(text(&parent["id"])) {
                details.push(format!("component of {}", text(&parent["type"])));
            }
        }
        details.extend(
            ["owner", "support", "location", "state"]
                .into_iter()
                .filter(|k| visible(&prop[*k]))
                .map(|k| format!("{k} {}", text(&prop[k]))),
        );
        clauses.push(format!("{id}: {}", details.join("; ")));
    }
    for relation in state["relationships"].as_array().into_iter().flatten() {
        if visible_props.contains(text(&relation["subject"]))
            && visible_props.contains(text(&relation["object"]))
            && visible(&relation["value"])
        {
            clauses.push(format!(
                "{} {} {}={}",
                text(&relation["subject"]),
                text(&relation["relation"]),
                text(&relation["object"]),
                text(&relation["value"])
            ));
        }
    }
    Ok(clauses.join(". "))
}

fn frame_request(args: &Value, root: &Path) -> Result<Value> {
    let plan = &args["plan"];
    let shot = &args["shot"];
    let number = args["number"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("Invalid storyboard shot number.")?;
    let visible = rows(
        &shot["opening_visible_roles"],
        &format!("explicit opening_visible_roles for storyboard shot {number}"),
    )?;
    let cast = plan["source"]["cast"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let profiles = rows(&args["profiles"], "character profiles")?;
    let mut references = Vec::new();
    let mut instructions = Vec::new();
    if !args["prior"].is_null() {
        resolve_image(&json!({"image": args["prior"]}), root)?;
        let mut prior = args["prior"].clone();
        prior["purpose"] = json!("edit-target");
        references.push(prior);
        instructions
            .push("Image 1: rejected image to edit; preserve all correct regions.".to_owned());
    }
    for role in visible {
        let profile = cast
            .iter()
            .find(|c| c["role"] == *role)
            .and_then(|c| profiles.iter().find(|p| p["id"] == c["character"]))
            .ok_or_else(|| {
                format!(
                    "Storyboard shot {number} has an unknown visible performer: {}",
                    text(role)
                )
            })?;
        let asset = &profile["image"];
        if !asset.is_object() {
            return Err(format!(
                "{} needs a saved portrait for storyboard generation.",
                text(&profile["name"])
            ));
        }
        let reference = json!({"file": format!("characters/{}/{}", text(&profile["id"]), text(&asset["file"])),
            "sha256": asset["sha256"], "role": role, "character": profile["id"]});
        resolve_image(&json!({"image": reference}), root)?;
        references.push(reference);
        instructions.push(format!(
            "Image {}: {}'s face identity.",
            references.len(),
            text(role)
        ));
    }
    let specs = rows(&args["specs"], "canonical reference specifications")?;
    if args["library"].is_null() && !specs.is_empty() {
        return Err("The canonical shot-reference library is missing; regenerate references before image generation.".into());
    }
    if !args["library"].is_null() {
        let library = rows(
            &args["library"]["references"],
            "canonical reference library",
        )?;
        let mut needed: BTreeSet<String> = visible
            .iter()
            .map(|r| format!("body:{}", text(r)))
            .collect();
        needed.insert(format!("environment:{}", shot["scene_id"]));
        let visible_props: BTreeSet<_> =
            rows(&shot["continuity"]["opening"]["props"], "opening props")?
                .iter()
                .filter(|p| p["visibility"] == "visible")
                .map(|p| text(&p["id"]))
                .collect();
        for prop in rows(&plan["world"]["props"], "canonical props")? {
            if rows(&prop["scene_ids"], "prop scene IDs")?.contains(&shot["scene_id"])
                && (visible_props.contains(text(&prop["id"]))
                    || rows(&prop["components"], "prop components")?
                        .iter()
                        .any(|p| visible_props.contains(text(&p["id"]))))
            {
                needed.insert(format!("prop:{}", text(&prop["id"])));
            }
        }
        let missing: Vec<_> = needed
            .iter()
            .filter(|id| {
                specs.iter().any(|s| text(&s["id"]) == id.as_str())
                    && !library.iter().any(|r| text(&r["id"]) == id.as_str())
            })
            .collect();
        if !missing.is_empty() {
            return Err(format!("Missing essential shot references: {missing:?}"));
        }
        let mut shared_designs: BTreeMap<String, usize> = BTreeMap::new();
        for reference in library {
            let id = text(&reference["id"]);
            if !needed.contains(id) {
                continue;
            }
            let spec = specs
                .iter()
                .find(|s| s["id"] == reference["id"])
                .ok_or("Unknown physical reference.")?;
            validate_reference(args, spec, reference, root)?;
            let shared = args["shared_designs"][id].as_str();
            if let Some(image_number) = shared.and_then(|s| shared_designs.get(s)).copied() {
                let asset = &mut references[image_number - 1];
                if asset.get("reference_ids").is_none() {
                    asset["reference_ids"] = json!([asset["reference_id"]]);
                }
                asset["reference_ids"]
                    .as_array_mut()
                    .ok_or("Invalid shared reference IDs.")?
                    .push(json!(id));
                instructions[image_number - 1].push_str(&format!(
                    " Also the design for {}.",
                    text(&spec["definition"]["id"])
                ));
                continue;
            }
            let mut image = reference["image"].clone();
            image["purpose"] = reference["kind"].clone();
            image["reference_id"] = json!(id);
            references.push(image);
            let mut instruction = format!("Image {}: {id} physical reference.", references.len());
            if let Some(set) = spec.get("assigned_set") {
                instruction.push_str(&format!(
                    " Use the assigned set {} as this shot's location. Preserve the environment reference's architecture, permanent furniture, materials and spatial layout across camera angles; add only the staged cast, props and source-required changes.",
                    text(&set["name"])
                ));
            }
            instructions.push(instruction);
            if let Some(shared) = shared {
                shared_designs.insert(shared.into(), references.len());
            }
        }
    }
    let mut composition = text(&shot["storyboard_prompt"]).trim().to_owned();
    if composition.is_empty() {
        composition = text(&shot["start_state"]).trim().into();
        if composition.is_empty() {
            return Err(format!(
                "Shot {number} needs a storyboard_prompt or a static start_state."
            ));
        }
        if !text(&shot["scene"]).is_empty() {
            composition = format!("{}. {composition}", text(&shot["scene"]));
        }
    }
    let people = if visible.is_empty() {
        "none; an empty composition".into()
    } else {
        visible.iter().map(text).collect::<Vec<_>>().join(", ")
    };
    let mut presence = format!("Visible people at time zero: {people}. ");
    let absent: Vec<_> = cast
        .iter()
        .filter(|c| !visible.contains(&c["role"]))
        .map(|c| text(&c["role"]))
        .collect();
    if !absent.is_empty() {
        presence.push_str(&format!(
            "Do not depict {}; their entrances occur later. ",
            absent.join(", ")
        ));
    }
    let prompt = format!(
        "One cinematic live-action starting frame, a single photograph, no panels or lettering. One instance of each visible part; use current docking and placement. {presence}Keep concealed features covered; references do not override current clothing or visibility. {}\nVisible opening state: {}\nKeep faces, active hands and essential props clear of frame edges.",
        instructions.join(" "),
        visible_state_prompt(plan, shot)?
    );
    let seed = args["seed"].as_i64().ok_or("Invalid storyboard seed.")?;
    Ok(
        json!({"policy": POLICY, "source_sha256": plan["source_sha256"], "shot": number,
        "opening_state": shot["continuity"]["opening"], "world_sha256": args["world_sha256"],
        "prompt": prompt, "composition": composition.trim(), "references": references,
        "seed": (i128::from(seed) + i128::from(number) - 1).rem_euclid(1_i128 << 31) as u64,
        "width": args["width"], "height": args["height"], "backend": args["backend"]}),
    )
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    if operation == "storyboard_reference_specs" {
        return reference_specs(&args["plan"]);
    }
    if operation == "storyboard_visible_state" {
        return visible_state_prompt(&args["plan"], &args["shot"]).map(Value::String);
    }
    let root = PathBuf::from(
        args["run_dir"]
            .as_str()
            .ok_or("Missing storyboard run directory.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    match operation {
        "storyboard_resolve_image" => resolve_image(&args["binding"], &root).map(|p| json!(p)),
        "storyboard_prepare" => prepare(args, &root),
        "storyboard_prepare_references" => prepare_references(args, &root),
        "storyboard_frame_request" => frame_request(args, &root),
        _ => Err("Unknown storyboard operation.".into()),
    }
}
