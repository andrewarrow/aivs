//! Fountain's body elements mapped to the editor's six editable element types.
//! See the local Fountain reference's FastFountainParser and FountainTests.

use super::Element;

pub(super) fn parse(source: &str) -> Result<Vec<(Element, String)>, String> {
    if source.contains('\0') {
        return Err(
            "The Fountain file contains null characters. Save it as UTF-8 and try again.".into(),
        );
    }
    let normalized = source
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let body = without_comments(&normalized);
    let lines: Vec<_> = body
        .trim_start_matches(char::is_whitespace)
        .split('\n')
        .collect();
    let mut index = 0;
    if lines.first().is_some_and(|line| title_field(line)) {
        while index < lines.len() && !lines[index].trim().is_empty() {
            index += 1;
        }
    }
    let mut elements = Vec::new();
    let mut separated = true;
    let mut dialogue = false;
    while index < lines.len() {
        let raw = lines[index];
        let line = raw.trim();
        index += 1;
        // Two spaces represent an intentional empty line within dialogue.
        if dialogue && raw == "  " {
            append(&mut elements, Element::Dialogue, "", true);
            continue;
        }
        if line.is_empty() {
            separated = true;
            dialogue = false;
            continue;
        }
        // Outline and pagination are authoring metadata, not story beats.
        if line.starts_with(['#', '=']) {
            separated = true;
            dialogue = false;
            continue;
        }
        if dialogue {
            let kind = if line.starts_with('(') {
                Element::Parenthetical
            } else {
                Element::Dialogue
            };
            let mut text = line.strip_prefix('~').unwrap_or(line).to_owned();
            if kind == Element::Parenthetical {
                while !text.ends_with(')') && index < lines.len() && !lines[index].trim().is_empty()
                {
                    text.push('\n');
                    text.push_str(lines[index].trim());
                    index += 1;
                }
            }
            append(
                &mut elements,
                kind,
                &plain_text(&text),
                kind == Element::Dialogue,
            );
            continue;
        }
        let text = plain_text(line);
        let next_nonempty = lines.get(index).is_some_and(|line| !line.trim().is_empty());
        let forced_heading = line.starts_with('.') && !line.starts_with("..");
        if forced_heading || (separated && scene_heading(&text) && !next_nonempty) {
            let heading = if forced_heading { &text[1..] } else { &text };
            let heading = without_scene_number(heading);
            append(&mut elements, Element::Heading, heading.trim(), false);
        } else if line.starts_with('!') {
            append(
                &mut elements,
                Element::Action,
                text[1..].trim_start(),
                false,
            );
        } else if line.starts_with('>') && line.ends_with('<') {
            append(
                &mut elements,
                Element::Action,
                text[1..text.len() - 1].trim(),
                false,
            );
        } else if line.starts_with('>') {
            append(&mut elements, Element::Transition, text[1..].trim(), false);
        } else if separated && !next_nonempty && transition(&text) {
            append(&mut elements, Element::Transition, &text, false);
        } else if line.starts_with('~') {
            append(&mut elements, Element::Action, &text[1..], !separated);
        } else if line.starts_with('@')
            || (separated && next_nonempty && !line.starts_with('\\') && character_cue(&text))
        {
            let cue = text.trim_start_matches('@').trim_end_matches('^').trim();
            let name = cue.split('(').next().unwrap_or(cue).trim();
            // The renderer's speaker grammar uses straight apostrophes.
            let name = name.replace(['’', '‘'], "'");
            if name.is_empty() {
                return Err("The Fountain file contains an empty character cue.".into());
            }
            append(&mut elements, Element::Character, &name, false);
            for extension in cue[name_prefix_len(cue)..].split('(').skip(1) {
                let extension = extension.trim_end_matches(')').trim();
                let normalized = extension.replace('’', "'").to_uppercase();
                if !extension.is_empty()
                    && !matches!(normalized.as_str(), "CONT'D" | "CONTINUED" | "CONT.")
                {
                    append(
                        &mut elements,
                        Element::Parenthetical,
                        &format!("({extension})"),
                        false,
                    );
                }
            }
            dialogue = true;
        } else {
            append(&mut elements, Element::Action, &text, !separated);
        }
        separated = false;
    }
    elements.retain(|(_, text)| !text.trim().is_empty());
    if elements.is_empty() {
        return Err("The Fountain file has no screenplay content to import.".into());
    }
    Ok(elements)
}

fn append(elements: &mut Vec<(Element, String)>, kind: Element, text: &str, merge: bool) {
    if merge
        && let Some((previous_kind, previous_text)) = elements.last_mut()
        && *previous_kind == kind
    {
        previous_text.push('\n');
        previous_text.push_str(text);
    } else {
        elements.push((kind, text.into()));
    }
}

fn title_field(line: &str) -> bool {
    line.split_once(':').is_some_and(|(key, _)| {
        matches!(
            key.trim().to_ascii_lowercase().as_str(),
            "title"
                | "credit"
                | "author"
                | "authors"
                | "source"
                | "notes"
                | "draft date"
                | "date"
                | "contact"
                | "copyright"
        )
    })
}

fn scene_heading(line: &str) -> bool {
    let upper = line.to_uppercase();
    [
        "INT./EXT",
        "INT/EXT",
        "INT./EXT.",
        "I./E",
        "I/E",
        "EXT",
        "INT",
        "EST",
    ]
    .iter()
    .any(|prefix| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(['.', '-', ' ', '\t']))
    })
}

fn without_scene_number(line: &str) -> &str {
    let line = line.trim();
    if let Some(prefix) = line.strip_suffix('#')
        && let Some((heading, _number)) = prefix.rsplit_once('#')
    {
        heading.trim_end()
    } else {
        line
    }
}

fn name_prefix_len(cue: &str) -> usize {
    cue.find('(').unwrap_or(cue.len())
}

fn character_cue(line: &str) -> bool {
    let name = &line[..name_prefix_len(line)];
    name.chars().any(char::is_alphabetic) && !name.chars().any(char::is_lowercase)
}

fn transition(line: &str) -> bool {
    (!line.chars().any(char::is_lowercase) && line.ends_with("TO:"))
        || matches!(
            line,
            "FADE IN:" | "FADE OUT." | "CUT TO BLACK." | "FADE TO BLACK."
        )
}

fn without_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut remaining = source;
    while !remaining.is_empty() {
        let closing = if remaining.starts_with("/*") {
            Some("*/")
        } else if remaining.starts_with("[[") {
            Some("]]")
        } else {
            None
        };
        if let Some(closing) = closing {
            let end = remaining[2..]
                .find(closing)
                .map_or(remaining.len(), |end| end + 4);
            result.extend(remaining[..end].chars().filter(|&c| c == '\n'));
            remaining = &remaining[end..];
        } else {
            let c = remaining.chars().next().unwrap();
            result.push(c);
            remaining = &remaining[c.len_utf8()..];
            if c == '\\'
                && let Some(escaped) = remaining.chars().next()
            {
                result.push(escaped);
                remaining = &remaining[escaped.len_utf8()..];
            }
        }
    }
    result
}

// Preserve literal punctuation and escaped markup while removing paired emphasis.
fn plain_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(c) = remaining.chars().next() {
        remaining = &remaining[c.len_utf8()..];
        if c == '\\'
            && let Some(escaped) = remaining.chars().next()
            && escaped.is_ascii_punctuation()
        {
            result.push(escaped);
            remaining = &remaining[escaped.len_utf8()..];
            continue;
        }
        if matches!(c, '*' | '_')
            && (c != '_' || !result.chars().last().is_some_and(char::is_alphanumeric))
        {
            let extra = if c == '*' {
                remaining
                    .chars()
                    .take_while(|&next| next == '*')
                    .count()
                    .min(2)
            } else {
                0
            };
            let delimiter = c.to_string().repeat(extra + 1);
            let content = &remaining[extra..];
            if !content.starts_with(char::is_whitespace)
                && let Some(end) = content.find(&delimiter)
                && end > 0
                && !content[..end].ends_with(['\\', ' ', '\t', '\n'])
            {
                result.push_str(&plain_text(&content[..end]));
                remaining = &content[end + delimiter.len()..];
                continue;
            }
        }
        result.push(c);
    }
    result
}
