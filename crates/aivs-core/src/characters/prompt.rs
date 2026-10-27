use super::*;
use crate::dialogue;
use std::collections::{BTreeMap, BTreeSet};

static AT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@([A-Za-z][A-Za-z0-9_-]*)").unwrap());
static STYLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:cartoons?|anime|manga|animation|animated|illustrations?|stylized|stylised|cgi|3d|three[- ]dimensional|claymation|stop[- ]motion|comic[- ]book|graphic[- ]novel|watercolou?r|oil painting|pixel art|storybook|fantasy art)\b").unwrap()
});
static NEGATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:not|no|without|avoid|never|isn't|is not|aren't|are not)\s+(?:(?:a|an|the)\s+)?(?:(?:in|as)\s+)?$").unwrap()
});
static NEXT_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+\w").unwrap());

// Byte masks keep regex offsets and UTF-8 slicing aligned. Quotes and contractions
// retain the same rules as the former Python character selector.
fn basic_mask(prompt: &str) -> Vec<bool> {
    let mut mask = vec![false; prompt.len()];
    let chars = prompt.char_indices().collect::<Vec<_>>();
    let mut opened = None;
    for (i, (index, c)) in chars.iter().copied().enumerate() {
        let previous = i.checked_sub(1).map(|i| chars[i].1);
        let next = chars.get(i + 1).map(|(_, c)| *c);
        if let Some((quote, start)) = opened {
            mask[index..index + c.len_utf8()].fill(true);
            if c == quote {
                if matches!(quote, '\'' | '’') {
                    if next.is_some_and(char::is_alphanumeric)
                        && (previous.is_some_and(char::is_alphanumeric) || i == start + 1)
                    {
                        continue;
                    }
                    let remainder = prompt[index + c.len_utf8()..]
                        .split('\n')
                        .next()
                        .unwrap_or("");
                    let next_close = remainder.find(quote);
                    let next_open = remainder.find(if quote == '’' { '‘' } else { '\'' });
                    if previous.is_some_and(char::is_alphanumeric)
                        && NEXT_WORD.is_match(remainder)
                        && next_close.is_some_and(|close| next_open.is_none_or(|open| close < open))
                    {
                        continue;
                    }
                }
                opened = None;
            }
        } else if matches!(c, '"' | '“' | '‘')
            || (c == '\'' && !previous.is_some_and(char::is_alphanumeric))
        {
            opened = Some((
                match c {
                    '“' => '”',
                    '‘' => '’',
                    _ => c,
                },
                i,
            ));
            mask[index..index + c.len_utf8()].fill(true);
        }
    }
    mask
}

fn mask(prompt: &str) -> Vec<bool> {
    let unwrapped = dialogue::unwrap_prompt(prompt);
    if unwrapped == prompt {
        return basic_mask(prompt);
    }
    let start = unwrapped.as_ptr() as usize - prompt.as_ptr() as usize;
    let body = basic_mask(unwrapped);
    let mut mask = vec![false; prompt.len()];
    mask[start..start + body.len()].copy_from_slice(&body);
    mask
}

fn alternate_style(prompt: &str) -> bool {
    let mask = mask(prompt);
    STYLE.find_iter(prompt).any(|m| {
        let start = prompt[..m.start()]
            .char_indices()
            .rev()
            .nth(31)
            .map_or(0, |(i, _)| i);
        !mask[m.start()..m.end()].contains(&true) && !NEGATION.is_match(&prompt[start..m.start()])
    })
}

pub(super) fn resolve(args: &Value) -> Result<Value> {
    let prompt = text(args, "prompt")?;
    let root = path(args, "root")?;
    let library = root.join("characters");
    let known = if library.exists() {
        fs::read_dir(&library)
            .map_err(|e| e.to_string())?
            .map(|entry| {
                let entry = entry.map_err(|e| e.to_string())?;
                let id = entry.file_name().to_string_lossy().into_owned();
                Ok((id, entry.path().is_dir()))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(id, is_dir)| (is_dir && ID.is_match(&id)).then_some(id))
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let mut selected = Vec::new();
    let mut add = |id: String| {
        if !selected.contains(&id) {
            selected.push(id);
        }
    };
    let explicit = match &args["explicit"] {
        Value::Null => Vec::new(),
        Value::String(_) => vec![args["explicit"].clone()],
        Value::Array(values) => values.clone(),
        _ => return Err("Invalid character selectors.".into()),
    };
    for value in &explicit {
        let selector = if value.is_object() {
            value["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| value["name"].as_str())
        } else {
            value.as_str()
        }
        .ok_or_else(|| format!("Invalid character selector: {value}"))?;
        let id = selector.trim().to_lowercase();
        if !known.contains(&id) {
            return Err(format!("Unknown character: {selector}"));
        }
        add(id);
    }
    let mask = mask(prompt);
    let mut mentions = Vec::new();
    for captures in AT_NAME.captures_iter(prompt) {
        let whole = captures.get(0).unwrap();
        if mask[whole.start()..whole.end()].contains(&true) {
            continue;
        }
        let id = captures[1].to_ascii_lowercase();
        if !known.contains(&id) {
            return Err(format!("Unknown character reference: @{}", &captures[1]));
        }
        mentions.push((whole.start(), id));
    }
    if args["references_only"] != true {
        for id in known {
            let pattern =
                Regex::new(&format!("(?i){}", regex::escape(&id))).map_err(|e| e.to_string())?;
            for m in pattern.find_iter(prompt) {
                let before = prompt[..m.start()].chars().next_back();
                let after = prompt[m.end()..].chars().next();
                let boundary = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
                if before.is_some_and(|c| boundary(c) || c == '@')
                    || after.is_some_and(boundary)
                    || mask[m.start()..m.end()].contains(&true)
                {
                    continue;
                }
                mentions.push((m.start(), id.clone()));
            }
        }
    }
    mentions.sort_by_key(|(index, _)| *index);
    for (_, id) in mentions {
        add(id);
    }
    selected
        .iter()
        .map(|id| read_profile(&library.join(id).join("character.json")))
        .collect::<Result<Vec<_>>>()
        .map(|profiles| json!(profiles))
}

pub(super) fn replace(prompt: &str, profiles: &[Value]) -> Result<String> {
    let by_id = profiles
        .iter()
        .map(|profile| Ok((text(profile, "id")?.to_lowercase(), profile)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mask = mask(prompt);
    let mut output = String::new();
    let mut cursor = 0;
    for captures in AT_NAME.captures_iter(prompt) {
        let m = captures.get(0).unwrap();
        if mask[m.start()..m.end()].contains(&true) {
            continue;
        }
        if let Some(profile) = by_id.get(&captures[1].to_ascii_lowercase()) {
            output.push_str(&prompt[cursor..m.start()]);
            output.push_str(text(profile, "name")?);
            cursor = m.end();
        }
    }
    output.push_str(&prompt[cursor..]);
    Ok(output)
}

pub(super) fn expand(args: &Value) -> Result<Value> {
    let prompt = text(args, "prompt")?;
    let profiles = profiles(args)?;
    let result = replace(prompt, profiles)?;
    if profiles.is_empty() {
        return Ok(json!(result));
    }
    let mut definitions = Vec::new();
    for profile in profiles {
        let mut details = format!(
            "{} has a consistent appearance: {}",
            text(profile, "name")?,
            text(profile, "description")?
        );
        if let Some(voice) = profile["voice_description"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            details.push_str(&format!(". Voice quality: {voice}"));
        }
        definitions.push(details + ".");
    }
    let mut continuity = "Character continuity: ".to_owned() + &definitions.join(" ");
    if args["image_conditioned"] != true
        && !alternate_style(prompt)
        && !profiles
            .iter()
            .any(|p| p["description"].as_str().is_some_and(alternate_style))
    {
        continuity.push_str(" Visual style: live-action footage with natural lighting, realistic textures, and lifelike proportions.");
    }
    Ok(json!(continuity + "\n\n" + &result))
}
