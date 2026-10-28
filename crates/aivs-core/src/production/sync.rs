//! Review the delivered soundtrack and picture together, independently of visual review.
use super::*;

pub const POLICY: &str = "per-speaker-sync-v1";

pub fn turns(plan: &Value, index: usize) -> Result<Vec<Value>> {
    let ids = rows(&plan["shots"][index]["beat_ids"])?;
    Ok(rows(&plan["source"]["beats"])?
        .iter()
        .filter(|beat| ids.contains(&beat["id"]) && beat["kind"] == "dialogue")
        .map(|beat| json!({"beat_id":beat["id"],"speaker":beat["speaker"],"text":beat["text"]}))
        .collect())
}

pub fn pending(plan: &Value, index: usize) -> Result<Value> {
    Ok(if turns(plan, index)?.is_empty() {
        json!({"policy":POLICY,"status":"not_applicable","summary":"No scripted dialogue in this shot."})
    } else {
        json!({"policy":POLICY,"status":"unassessed","summary":"Lip sync has not been assessed against the delivered audio."})
    })
}

fn validate(result: &Value, expected: &[Value]) -> Result<()> {
    if result["policy"] != POLICY
        || !matches!(
            result["status"].as_str(),
            Some("passed" | "issues_detected" | "unassessed")
        )
    {
        return Err("Lip-sync reviewer returned an invalid report.".into());
    }
    let actual = rows(&result["turns"])?;
    if actual.len() != expected.len() {
        return Err("Lip-sync review omitted a scripted speaking turn.".into());
    }
    let mut issues = false;
    let mut unknown = false;
    for (turn, required) in actual.iter().zip(expected) {
        if ["beat_id", "speaker", "text"]
            .iter()
            .any(|key| turn[*key] != required[*key])
        {
            return Err("Lip-sync review belongs to different dialogue.".into());
        }
        match turn["status"].as_str() {
            Some("passed") => {
                let windows = rows(&turn["windows"])?;
                if windows.is_empty() || windows.iter().any(|window| window["status"] != "passed") {
                    return Err("Lip-sync pass has incomplete timing evidence.".into());
                }
            }
            Some("issues_detected") => issues = true,
            Some("unassessed") => unknown = true,
            _ => return Err("Lip-sync review has an invalid turn status.".into()),
        }
    }
    let status = if issues {
        "issues_detected"
    } else if unknown {
        "unassessed"
    } else {
        "passed"
    };
    if result["status"] != status {
        return Err("Lip-sync summary does not match its speaking turns.".into());
    }
    Ok(())
}

pub fn shot(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    delivery: &Path,
    delivery_hash: &str,
    start: u64,
    run: &Path,
) -> Result<Value> {
    let expected = turns(plan, index)?;
    if expected.is_empty() {
        let result = pending(plan, index)?;
        save(
            &run.join("shots")
                .join(format!("{:03}/sync-review/review.json", index + 1)),
            &result,
        )?;
        return Ok(result);
    }
    let file = run
        .join("shots")
        .join(format!("{:03}/sync-review/review.json", index + 1));
    let result = (|| -> Result<Value> {
        let implementation = [
            "sync_review.py",
            "sync_faces.py",
            "sync-assets.json",
            "requirements-sync.txt",
        ]
        .iter()
        .map(|name| artifacts::file_hash(&ctx.root.join("python").join(name)))
        .collect::<Result<Vec<_>>>()?;
        let assets = artifacts::file_hash(&ctx.root.join(".cache/sync-review/assets.json"))?;
        let key = ctx.digest(&json!([
            POLICY,
            implementation,
            assets,
            delivery_hash,
            start,
            plan["shots"][index]["seconds"],
            expected
        ]))?;
        if let Ok(cached) = read(&file) {
            if cached["cache_key"] == key
                && cached["status"] != "unassessed"
                && validate(&cached, &expected).is_ok()
            {
                return Ok(cached);
            }
        }
        let mut result = inference::run(
            &json!({"root":ctx.root,"worker_operation":"sync_review","timeout_seconds":300,
                "payload":{"clip":delivery,"start_seconds":start,
                "seconds":plan["shots"][index]["seconds"],"turns":expected}}),
            ctx.cancelled.clone(),
            &mut ctx.emit,
        )?;
        validate(&result, &expected)?;
        result["cache_key"] = json!(key);
        Ok(result)
    })();
    let mut result = match result {
        Ok(result) => result,
        Err(error) => {
            ctx.check()?;
            json!({"policy":POLICY,"status":"unassessed",
                "summary":format!("Lip-sync assessment unavailable: {error}. Retry application setup if review models are missing."),
                "turns":expected.iter().map(|turn| {
                    let mut turn = turn.clone();
                    turn["status"] = json!("unassessed");
                    turn
                }).collect::<Vec<_>>()})
        }
    };
    result["delivery_sha256"] = json!(delivery_hash);
    result["shot_start_seconds"] = json!(start);
    result["shot"] = json!(index + 1);
    save(&file, &result)?;
    Ok(result)
}
