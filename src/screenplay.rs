use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::characters::CastCharacter;

mod editor;
mod fountain;
mod sets;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Element {
    Heading,
    Action,
    Character,
    Parenthetical,
    Dialogue,
    Transition,
}

impl Element {
    const ALL: [Self; 6] = [
        Self::Heading,
        Self::Action,
        Self::Character,
        Self::Parenthetical,
        Self::Dialogue,
        Self::Transition,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Heading => "Scene heading",
            Self::Action => "Action",
            Self::Character => "Character",
            Self::Parenthetical => "Parenthetical",
            Self::Dialogue => "Dialogue",
            Self::Transition => "Transition",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Heading => Self::Action,
            Self::Character | Self::Parenthetical => Self::Dialogue,
            Self::Dialogue => Self::Character,
            Self::Action => Self::Action,
            Self::Transition => Self::Heading,
        }
    }

    fn margins(self) -> (f32, f32) {
        match self {
            Self::Character => (252.0, 150.0),
            Self::Parenthetical => (216.0, 180.0),
            Self::Dialogue => (180.0, 252.0),
            Self::Transition => (432.0, 108.0),
            _ => (108.0, 432.0),
        }
    }
}

// The page and renderer share the same scene, including dialogue attribution.
const SCENE: &[(Element, &str)] = &[
    (Element::Heading, "EXT. DESERT ROAD - DUSK"),
    (
        Element::Action,
        "A two-lane road cuts through red rock. An old station wagon sits on the shoulder, its hood raised.",
    ),
    (
        Element::Action,
        "SUE, 30s, turns a battered postcard over in her hands. JOE, late 20s, emerges from behind the engine.",
    ),
    (Element::Character, "JOE"),
    (Element::Dialogue, "Good news. It's definitely the car."),
    (
        Element::Action,
        "Sue almost smiles. She looks down the empty road.",
    ),
    (Element::Character, "SUE"),
    (
        Element::Dialogue,
        "My dad said you could see the ocean from here.",
    ),
    (Element::Character, "JOE"),
    (Element::Parenthetical, "(looking around)"),
    (Element::Dialogue, "He was an optimistic man."),
    (
        Element::Action,
        "She hands him the postcard. The same road, forty years younger. On the back: KEEP GOING.",
    ),
    (Element::Character, "SUE"),
    (Element::Dialogue, "He made it sound closer."),
    (
        Element::Action,
        "Joe lowers the hood. Beyond the rocks, a thin line of gold catches the last of the sun.",
    ),
    (Element::Character, "JOE"),
    (Element::Dialogue, "We've still got some light."),
    (
        Element::Action,
        "Sue pockets the postcard. They start walking.",
    ),
    (Element::Transition, "FADE OUT."),
];

#[derive(Clone, Debug, PartialEq)]
struct Block {
    id: u64,
    kind: Element,
    text: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Document {
    blocks: Vec<Block>,
    cast: BTreeMap<String, CastCharacter>,
    sets: BTreeMap<u64, crate::sets::SceneSet>,
}

struct Snapshot {
    document: Document,
    active: u64,
}

pub struct Scene<'a> {
    pub id: u64,
    pub heading: &'a str,
}

pub struct Screenplay {
    document: Document,
    active: u64,
    next_id: u64,
    focus: Option<(u64, usize)>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    typing: Option<(u64, Instant)>,
    path: Option<PathBuf>,
    pending_save: Option<Instant>,
    save_error: Option<String>,
    load_failed: bool,
    rename: Option<(String, String)>,
}

impl Default for Screenplay {
    fn default() -> Self {
        Self {
            document: Document {
                blocks: SCENE
                    .iter()
                    .enumerate()
                    .map(|(id, &(kind, text))| Block {
                        id: id as u64,
                        kind,
                        text: text.into(),
                    })
                    .collect(),
                cast: BTreeMap::new(),
                sets: BTreeMap::new(),
            },
            active: 0,
            next_id: SCENE.len() as u64,
            focus: None,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: None,
            path: None,
            pending_save: None,
            save_error: None,
            load_failed: false,
            rename: None,
        }
    }
}

fn role_name(text: &str) -> String {
    text.trim().to_uppercase()
}

impl Screenplay {
    pub fn can_import(&self) -> bool {
        !self.load_failed
    }

    pub fn import_fountain(&mut self, path: &Path) -> Result<(), String> {
        if !self.can_import() {
            return Err("Reload the screenplay before importing.".into());
        }
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("fountain"))
        {
            return Err("Choose a .fountain screenplay to import.".into());
        }
        let source = fs::read_to_string(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let elements = fountain::parse(&source)?;
        let before = self.snapshot();
        self.document = Document {
            blocks: elements
                .into_iter()
                .map(|(kind, text)| {
                    let id = self.next_id;
                    self.next_id += 1;
                    Block { id, kind, text }
                })
                .collect(),
            cast: BTreeMap::new(),
            sets: BTreeMap::new(),
        };
        self.active = self.document.blocks[0].id;
        self.focus = Some((self.active, 0));
        self.rename = None;
        self.record(before, None);
        self.save();
        Ok(())
    }

    pub fn load(root: &Path) -> Self {
        let path = root.join("screenplay.json");
        let mut screenplay = Self::default();
        screenplay.path = Some(path.clone());
        match fs::read(&path) {
            Ok(bytes) => match Self::decode(&bytes) {
                Ok(document) => {
                    screenplay.next_id = document.blocks.len() as u64;
                    screenplay.document = document;
                }
                Err(error) => {
                    screenplay.save_error = Some(format!(
                        "Could not load screenplay.json: {error}. Original file preserved."
                    ));
                    screenplay.load_failed = true;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                screenplay.pending_save = Some(Instant::now());
            }
            Err(error) => {
                screenplay.save_error = Some(format!("Could not read screenplay.json: {error}"));
                screenplay.load_failed = true;
            }
        }
        screenplay
    }

    fn decode(bytes: &[u8]) -> Result<Document, String> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let invalid = || "unsupported or invalid screenplay document".to_owned();
        if value["version"] != 1 {
            return Err(invalid());
        }
        let blocks = value["blocks"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .enumerate()
            .map(|(id, item)| {
                Ok(Block {
                    id: id as u64,
                    kind: Element::ALL
                        .into_iter()
                        .find(|kind| item["type"] == kind.label())
                        .ok_or_else(invalid)?,
                    text: item["text"].as_str().ok_or_else(invalid)?.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if blocks.is_empty() {
            return Err(invalid());
        }
        let mut cast = BTreeMap::new();
        for item in value["cast"].as_array().ok_or_else(invalid)? {
            cast.insert(
                role_name(item["role"].as_str().ok_or_else(invalid)?),
                CastCharacter {
                    id: item["id"].as_str().ok_or_else(invalid)?.into(),
                    name: item["name"].as_str().ok_or_else(invalid)?.into(),
                },
            );
        }
        let mut sets = BTreeMap::new();
        if let Some(assignments) = value.get("sets") {
            for item in assignments.as_array().ok_or_else(invalid)? {
                let index = item["block"].as_u64().ok_or_else(invalid)?;
                let block = blocks
                    .iter()
                    .find(|block| block.id == index)
                    .filter(|block| block.kind == Element::Heading)
                    .ok_or_else(invalid)?;
                sets.insert(
                    block.id,
                    crate::sets::SceneSet {
                        id: item["id"].as_str().ok_or_else(invalid)?.into(),
                        name: item["name"].as_str().ok_or_else(invalid)?.into(),
                    },
                );
            }
        }
        Ok(Document { blocks, cast, sets })
    }

    fn save(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        if self.load_failed {
            return;
        }
        let value = serde_json::json!({
            "version": 1,
            "blocks": self.document.blocks.iter().map(|b| serde_json::json!({
                "type": b.kind.label(), "text": b.text,
            })).collect::<Vec<_>>(),
            "sets": self.document.blocks.iter().enumerate().filter_map(|(index, block)| {
                if block.kind != Element::Heading { return None; }
                self.document.sets.get(&block.id).map(|set| serde_json::json!({
                    "block": index, "id": set.id, "name": set.name,
                }))
            }).collect::<Vec<_>>(),
            "cast": self.document.cast.iter().map(|(role, c)| serde_json::json!({
                "role": role, "id": c.id, "name": c.name,
            })).collect::<Vec<_>>(),
        });
        let result = (|| -> Result<(), String> {
            let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
            let temporary = path.with_file_name(".screenplay.json.tmp");
            fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
            fs::rename(temporary, path).map_err(|e| e.to_string())
        })();
        self.pending_save = None;
        self.save_error = result
            .err()
            .map(|e| format!("Could not save screenplay: {e}"));
    }

    fn roles(&self) -> Vec<String> {
        let mut roles = Vec::new();
        for block in &self.document.blocks {
            if block.kind == Element::Character {
                let role = role_name(&block.text);
                if !role.is_empty() && !roles.contains(&role) {
                    roles.push(role);
                }
            }
        }
        roles
    }

    pub fn assign(&mut self, role: &str, character: CastCharacter) {
        let role = role_name(role);
        if self.roles().contains(&role) {
            self.document.cast.insert(role, character);
        }
    }

    pub fn scenes(&self) -> Vec<Scene<'_>> {
        self.document
            .blocks
            .iter()
            .filter(|block| block.kind == Element::Heading && !block.text.trim().is_empty())
            .map(|block| Scene {
                id: block.id,
                heading: block.text.trim(),
            })
            .collect()
    }

    pub fn render_request(&self, scene_id: u64) -> Result<String, String> {
        if self.load_failed {
            return Err("Reload the screenplay before generating.".into());
        }
        let blocks = &self.document.blocks;
        let start = blocks
            .iter()
            .position(|block| {
                block.id == scene_id
                    && block.kind == Element::Heading
                    && !block.text.trim().is_empty()
            })
            .ok_or_else(|| "Select a scene before generating.".to_owned())?;
        let end = blocks[start + 1..]
            .iter()
            .position(|block| block.kind == Element::Heading)
            .map_or(blocks.len(), |offset| start + 1 + offset);
        let mut script = String::new();
        let mut speaker = String::new();
        let mut cue = String::new();
        let mut roles = Vec::new();
        let mut has_content = false;
        for (offset, block) in blocks[start..end].iter().enumerate() {
            let index = start + offset;
            let text = block.text.trim();
            match block.kind {
                Element::Character => {
                    speaker = role_name(text);
                    cue.clear();
                    if !speaker.is_empty()
                        && (speaker.chars().count() > 128
                            || !speaker
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_alphanumeric() || c == '_')
                            || !speaker
                                .chars()
                                .all(|c| c.is_alphanumeric() || "_ .'-".contains(c)))
                    {
                        return Err(format!(
                            "Use letters, numbers, spaces, periods, apostrophes or hyphens in character name at element {}.",
                            index + 1
                        ));
                    }
                    if !speaker.is_empty() && !roles.contains(&speaker) {
                        roles.push(speaker.clone());
                    }
                }
                Element::Parenthetical => {
                    let instruction = text
                        .trim_matches(['(', ')'])
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    if instruction.contains(['(', ')']) {
                        return Err(format!(
                            "Remove nested parentheses at element {} before generating.",
                            index + 1
                        ));
                    }
                    if !instruction.is_empty() {
                        if !cue.is_empty() {
                            cue.push_str("; ");
                        }
                        cue.push_str(&instruction);
                    }
                }
                Element::Dialogue if !text.is_empty() => {
                    if speaker.is_empty() {
                        return Err(format!(
                            "Add a character before dialogue at element {}.",
                            index + 1
                        ));
                    }
                    if text.chars().count() > 4000 {
                        return Err(format!(
                            "Shorten dialogue at element {} to 4,000 characters before generating.",
                            index + 1
                        ));
                    }
                    script.push_str(&speaker);
                    if !cue.is_empty() {
                        script.push_str(&format!(" ({cue})"));
                    }
                    script.push_str(": \"");
                    script.push_str(&text.split_whitespace().collect::<Vec<_>>().join(" "));
                    script.push_str("\"\n\n");
                    cue.clear();
                    has_content = true;
                }
                Element::Dialogue => {}
                _ => {
                    speaker.clear();
                    cue.clear();
                    if !text.is_empty() {
                        script.push_str(text);
                        script.push_str("\n\n");
                        has_content |= block.kind == Element::Action;
                    }
                }
            }
        }
        if !has_content {
            return Err("Add action or dialogue before generating.".into());
        }
        if script.contains('\0') || script.chars().count() > 100_000 {
            return Err("Selected scene must contain no null characters and fit within 100,000 characters before generating.".into());
        }
        let missing = roles
            .iter()
            .filter(|role| !self.document.cast.contains_key(*role))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "Assign characters to {} before generating.",
                missing.join(" and ")
            ));
        }
        let cast = roles
            .iter()
            .map(|role| serde_json::json!({"role": role, "character": self.document.cast[role].id}))
            .collect::<Vec<_>>();
        serde_json::to_string(
            &serde_json::json!({"version": 1, "script": script.trim_end(), "cast": cast}),
        )
        .map_err(|error| format!("Could not serialize screenplay request: {error}"))
    }

    fn active_index(&self) -> usize {
        self.document
            .blocks
            .iter()
            .position(|b| b.id == self.active)
            .unwrap_or(0)
    }

    fn insert(&mut self, kind: Element) {
        let index = self.active_index() + 1;
        let id = self.next_id;
        self.next_id += 1;
        self.document.blocks.insert(
            index,
            Block {
                id,
                kind,
                text: if kind == Element::Parenthetical {
                    "()".into()
                } else {
                    String::new()
                },
            },
        );
        self.active = id;
        self.focus = Some((id, usize::from(kind == Element::Parenthetical)));
    }

    fn split(&mut self, selection: std::ops::Range<usize>) {
        let index = self.active_index();
        if self.document.blocks[index].kind == Element::Parenthetical {
            let instruction = self.document.blocks[index]
                .text
                .trim()
                .trim_matches(['(', ')'])
                .trim();
            self.document.blocks[index].text = format!("({instruction})");
            if let Some(next) = self.document.blocks.get(index + 1)
                && next.kind == Element::Dialogue
            {
                self.active = next.id;
                self.focus = Some((next.id, 0));
            } else {
                self.insert(Element::Dialogue);
            }
            return;
        }
        let block = &mut self.document.blocks[index];
        let kind = block.kind.next();
        let before = block.text.chars().take(selection.start).collect::<String>();
        let after = block.text.chars().skip(selection.end).collect::<String>();
        block.text = before;
        self.insert(kind);
        let index = self.active_index();
        self.document.blocks[index].text = after;
    }

    fn tab(&mut self, backward: bool, selection: std::ops::Range<usize>) {
        let index = self.active_index();
        let kind = self.document.blocks[index].kind;
        if backward {
            let current = Element::ALL.iter().position(|k| *k == kind).unwrap();
            self.document.blocks[index].kind = Element::ALL[(current + 5) % 6];
            if kind == Element::Parenthetical && self.document.blocks[index].text == "()" {
                self.document.blocks[index].text.clear();
                self.focus = Some((self.active, 0));
            }
            return;
        }
        if kind == Element::Parenthetical {
            self.split(selection);
            return;
        }
        if matches!(kind, Element::Character | Element::Dialogue)
            && selection.is_empty()
            && selection.end == self.document.blocks[index].text.chars().count()
            && let Some(next) = self.document.blocks.get(index + 1)
            && next.kind == Element::Parenthetical
        {
            self.active = next.id;
            self.focus = Some((next.id, usize::from(next.text.starts_with('('))));
            return;
        }
        if kind == Element::Dialogue && !self.document.blocks[index].text.is_empty() {
            // Preserve both sides of the caret when inserting an instruction
            // into an existing speech. Enter will resume the continuation.
            self.split(selection);
            let continuation_index = self.active_index();
            self.document.blocks[continuation_index].kind = Element::Dialogue;
            self.active = self.document.blocks[index].id;
            self.insert(Element::Parenthetical);
            if self.document.blocks[continuation_index + 1].text.is_empty() {
                self.document.blocks.remove(continuation_index + 1);
            }
            return;
        }
        let target = match kind {
            Element::Character | Element::Dialogue => Element::Parenthetical,
            Element::Action => Element::Character,
            _ => kind.next(),
        };
        if self.document.blocks[index].text.is_empty() {
            self.document.blocks[index].kind = target;
            if target == Element::Parenthetical {
                self.document.blocks[index].text = "()".into();
                self.focus = Some((self.active, 1));
            }
        } else {
            self.insert(target);
        }
    }

    fn delete(&mut self) {
        let index = self.active_index();
        if self.document.blocks.len() == 1 {
            self.document.blocks[0].text.clear();
            self.document.blocks[0].kind = Element::Action;
        } else {
            self.document.blocks.remove(index);
            self.active = self.document.blocks[index.min(self.document.blocks.len() - 1)].id;
        }
        self.focus = Some((self.active, 0));
    }

    fn backspace_empty(&mut self) {
        let index = self.active_index();
        if self.document.blocks[index].kind == Element::Heading
            && self.document.blocks[index].text.is_empty()
            && self.document.blocks.len() > 1
        {
            self.document.blocks.remove(index);
            let previous = &self.document.blocks[index.saturating_sub(1)];
            self.active = previous.id;
            self.focus = Some((previous.id, previous.text.chars().count()));
            return;
        }
        let block = &mut self.document.blocks[index];
        if block.kind == Element::Parenthetical && block.text == "()" {
            // Delete the automatic pair first. A subsequent Backspace on the
            // now-empty element changes its format.
            block.text.clear();
        } else if block.text.is_empty() {
            block.kind = match block.kind {
                Element::Parenthetical => Element::Dialogue,
                Element::Dialogue => Element::Character,
                Element::Character => Element::Action,
                Element::Action | Element::Transition | Element::Heading => Element::Heading,
            };
        }
        self.focus = Some((self.active, 0));
    }

    fn rename_role(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = role_name(new);
        if new.is_empty() {
            return Err("Enter a character name.".into());
        }
        if new != old && self.roles().contains(&new) {
            return Err("That character already exists.".into());
        }
        for block in &mut self.document.blocks {
            if block.kind == Element::Character && role_name(&block.text) == old {
                block.text.clone_from(&new);
            }
        }
        if let Some(character) = self.document.cast.remove(old) {
            self.document.cast.insert(new, character);
        }
        Ok(())
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            document: self.document.clone(),
            active: self.active,
        }
    }

    fn record(&mut self, before: Snapshot, typed: Option<u64>) {
        if before.document == self.document {
            return;
        }
        let coalesce = typed.is_some_and(|id| {
            self.typing.is_some_and(|(previous, at)| {
                id == previous && at.elapsed() < Duration::from_secs(1)
            })
        });
        if !coalesce {
            self.undo.push(before);
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing = typed.map(|id| (id, Instant::now()));
        self.pending_save = Some(Instant::now());
    }

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.document = snapshot.document;
            self.active = snapshot.active;
            self.restored();
        }
    }

    fn redo(&mut self) {
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.document = snapshot.document;
            self.active = snapshot.active;
            self.restored();
        }
    }

    fn restored(&mut self) {
        self.active = self.document.blocks[self.active_index()].id;
        self.focus = Some((
            self.active,
            self.document.blocks[self.active_index()]
                .text
                .chars()
                .count(),
        ));
        self.typing = None;
        self.pending_save = Some(Instant::now());
    }
}

impl Drop for Screenplay {
    fn drop(&mut self) {
        if self.pending_save.is_some() {
            self.save();
        }
    }
}

#[cfg(test)]
mod tests;
