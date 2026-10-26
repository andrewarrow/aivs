use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::characters::CastCharacter;

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
}

struct Snapshot {
    document: Document,
    active: u64,
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

// egui's consume_key intentionally ignores extra Shift/Alt modifiers. Paragraph
// commands require an exact match to preserve line breaks and text selection.
fn consume_exact(input: &mut egui::InputState, modifiers: egui::Modifiers, key: egui::Key) -> bool {
    let mut consumed = false;
    input.events.retain(|event| {
        let matches = matches!(event, egui::Event::Key { key: pressed_key, pressed: true, modifiers: pressed_modifiers, .. }
            if *pressed_key == key && pressed_modifiers.matches_exact(modifiers));
        consumed |= matches;
        !matches
    });
    consumed
}

impl Screenplay {
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
        Ok(Document { blocks, cast })
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

    pub fn render_request(&self) -> Result<String, String> {
        if self.load_failed {
            return Err("Reload the screenplay before generating.".into());
        }
        let mut script = String::new();
        let mut speaker = String::new();
        let mut cue = String::new();
        let mut has_content = false;
        for (index, block) in self.document.blocks.iter().enumerate() {
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
            return Err("Screenplay must contain no null characters and fit within 100,000 characters before generating.".into());
        }
        let roles = self.roles();
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

    fn role_ui(
        &mut self,
        ui: &mut egui::Ui,
        role: &str,
        script_text: &str,
        index: usize,
        target: egui::Rect,
        characters: &[CastCharacter],
    ) {
        let response = ui.interact(target, ui.id().with(("role", index)), egui::Sense::click());
        let assignment = self
            .document
            .cast
            .get(role)
            .map_or("Uncast", |c| c.name.as_str())
            .to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Cast {role}: {assignment}. {script_text}"),
            )
        });
        if response.hovered()
            || response.has_focus()
            || response.dnd_hover_payload::<CastCharacter>().is_some()
        {
            ui.painter().rect_stroke(
                target,
                2.0,
                (1.0, egui::Color32::from_rgb(35, 90, 165)),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(character) = response.dnd_release_payload::<CastCharacter>() {
            self.assign(role, (*character).clone());
            ui.ctx().request_repaint();
        }
        egui::Popup::menu(&response).show(|ui| {
            ui.label(format!("Cast {role}"));
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .show(ui, |ui| {
                    for character in characters {
                        if ui
                            .selectable_label(
                                self.document.cast.get(role) == Some(character),
                                &character.name,
                            )
                            .clicked()
                        {
                            self.assign(role, character.clone());
                            ui.close();
                        }
                    }
                    if ui
                        .selectable_label(!self.document.cast.contains_key(role), "Uncast")
                        .clicked()
                    {
                        self.document.cast.remove(role);
                        ui.close();
                    }
                });
        });
        response.on_hover_text(format!(
            "{role}: {assignment}. Drop a character here or click to choose."
        ));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, characters: &[CastCharacter]) {
        if self.load_failed {
            ui.strong("Screenplay");
            ui.colored_label(
                ui.visuals().error_fg_color,
                self.save_error
                    .as_deref()
                    .unwrap_or("Could not load screenplay."),
            );
            if ui.button("Retry loading screenplay").clicked() {
                let root = self.path.as_ref().unwrap().parent().unwrap().to_owned();
                *self = Self::load(&root);
            }
            return;
        }
        let editor_id = ui.id().with("screenplay_editor");
        let before = self.snapshot();
        let mut typed = None;
        let mut history_navigation = false;
        let editor_focused = ui.memory(|m| m.focused()).is_some_and(|id| {
            self.document
                .blocks
                .iter()
                .any(|b| id == editor_id.with(("text", b.id)))
        });
        if editor_focused {
            // A newly focused text widget has no focus filter until the next
            // frame. Suppress egui's spatial navigation while we own these keys.
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        if editor_focused && self.rename.is_none() {
            if ui.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
            }) {
                self.redo();
                history_navigation = true;
            } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)) {
                self.undo();
                history_navigation = true;
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save();
        }
        ui.horizontal_wrapped(|ui| {
            ui.strong("Screenplay");
            if self.path.is_some() {
                let status = if self.save_error.is_some() {
                    "Not saved"
                } else if self.pending_save.is_some() {
                    "Saving…"
                } else {
                    "Saved locally"
                };
                ui.weak(status).on_hover_text(
                    "Saved to screenplay.json in your workspace. ⌘/Ctrl+S saves now.",
                );
            }
            ui.menu_button("Help", |ui| {
                ui.label("Click any text to edit.");
                ui.label("Enter: next screenplay element");
                ui.label("Shift+Enter: line break within an element");
                ui.label("Tab in dialogue: add (instructions)");
                ui.label("Enter after instructions: continue dialogue");
                ui.label("Tab after a character: add (instructions) before dialogue");
                ui.label("Shift+Tab: previous element type");
                ui.label("Backspace on empty: instructions → dialogue → character");
                ui.label("Keep pressing to reach action, then a scene heading.");
                ui.label("Up / Down at the start / end: previous / next element");
                ui.label("⌘/Ctrl+Z: undo · ⌘/Ctrl+Shift+Z: redo");
                ui.label("Use Edit → Rename character to change every cue.");
                ui.label("Click a cast label or drop a gallery character onto a cue or dialogue.");
            });
        });
        ui.horizontal_wrapped(|ui| {
            let index = self.active_index();
            let kind = self.document.blocks[index].kind;
            egui::ComboBox::from_id_salt("element_type")
                .selected_text(kind.label())
                .width(116.0)
                .show_ui(ui, |ui| {
                    for candidate in Element::ALL {
                        if ui
                            .selectable_value(
                                &mut self.document.blocks[index].kind,
                                candidate,
                                candidate.label(),
                            )
                            .changed()
                        {
                            self.focus = Some((
                                self.active,
                                self.document.blocks[index].text.chars().count(),
                            ));
                        }
                    }
                });
            ui.menu_button("+ Insert", |ui| {
                for kind in Element::ALL {
                    if ui.button(kind.label()).clicked() {
                        self.insert(kind);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Insert an element after the selected paragraph.");
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(index > 0, egui::Button::new("Move up"))
                    .clicked()
                {
                    self.document.blocks.swap(index, index - 1);
                    self.focus = Some((self.active, 0));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        index + 1 < self.document.blocks.len(),
                        egui::Button::new("Move down"),
                    )
                    .clicked()
                {
                    self.document.blocks.swap(index, index + 1);
                    self.focus = Some((self.active, 0));
                    ui.close();
                }
                if ui.button("Delete element").clicked() {
                    self.delete();
                    ui.close();
                }
                let role = self.document.blocks[..=index]
                    .iter()
                    .rev()
                    .take_while(|b| {
                        !matches!(
                            b.kind,
                            Element::Action | Element::Heading | Element::Transition
                        )
                    })
                    .find(|b| b.kind == Element::Character)
                    .map(|b| role_name(&b.text));
                if ui
                    .add_enabled(
                        role.as_ref().is_some_and(|r| !r.is_empty()),
                        egui::Button::new("Rename character…"),
                    )
                    .clicked()
                {
                    let role = role.unwrap();
                    self.rename = Some((role.clone(), role));
                    ui.close();
                }
            });
            if ui
                .add_enabled(!self.undo.is_empty(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
                history_navigation = true;
            }
            if ui
                .add_enabled(!self.redo.is_empty(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
                history_navigation = true;
            }
        });
        if let Some(error) = self.save_error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, error);
                if ui.button("Retry save").clicked() {
                    self.save();
                }
            });
        }

        // Continuous paper grows with the script. Elements retain stable widget IDs
        // through insertions and moves, so cursor state stays attached to the text.
        let mut split = None;
        let mut tab = None;
        let mut backspace_empty = false;
        egui::Frame::new()
            .fill(egui::Color32::from_gray(48))
            .inner_margin(12)
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .id_salt("screenplay_page")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let scale = (ui.available_width() / 612.0).clamp(0.85, 1.0);
                        let width = 612.0 * scale;
                        let left = ((ui.available_width() - width) * 0.5).max(0.0);
                        let origin = ui.cursor().min + egui::vec2(left, 0.0);
                        let page_shape = ui.painter().add(egui::Shape::Noop);
                        let mut y = 72.0 * scale;
                        let mut role = String::new();
                        let mut role_top = y;
                        for index in 0..self.document.blocks.len() {
                            let id = self.document.blocks[index].id;
                            let kind = self.document.blocks[index].kind;
                            let widget_id = editor_id.with(("text", id));
                            let focused = ui.memory(|m| m.has_focus(widget_id));
                            let mut enter = false;
                            let mut tab_pressed = None;
                            if focused {
                                let range = egui::TextEdit::load_state(ui.ctx(), widget_id)
                                    .and_then(|state| state.cursor.char_range())
                                    .unwrap_or_default()
                                    .as_sorted_char_range();
                                if range.is_empty() {
                                    if (self.document.blocks[index].text.is_empty()
                                        || (kind == Element::Parenthetical
                                            && self.document.blocks[index].text == "()"
                                            && range.start.0 == 1))
                                        && ui.input_mut(|i| {
                                            consume_exact(
                                                i,
                                                egui::Modifiers::NONE,
                                                egui::Key::Backspace,
                                            )
                                        })
                                    {
                                        backspace_empty = true;
                                    }
                                    let previous = range.start.0 == 0
                                        && index > 0
                                        && ui.input_mut(|i| {
                                            consume_exact(
                                                i,
                                                egui::Modifiers::NONE,
                                                egui::Key::ArrowUp,
                                            )
                                        });
                                    let next = range.end.0
                                        == self.document.blocks[index].text.chars().count()
                                        && index + 1 < self.document.blocks.len()
                                        && ui.input_mut(|i| {
                                            consume_exact(
                                                i,
                                                egui::Modifiers::NONE,
                                                egui::Key::ArrowDown,
                                            )
                                        });
                                    if previous {
                                        let block = &self.document.blocks[index - 1];
                                        self.focus = Some((block.id, block.text.chars().count()));
                                    } else if next {
                                        self.focus = Some((self.document.blocks[index + 1].id, 0));
                                    }
                                }
                                ui.input_mut(|input| {
                                    enter = consume_exact(
                                        input,
                                        egui::Modifiers::NONE,
                                        egui::Key::Enter,
                                    );
                                    let backward = consume_exact(
                                        input,
                                        egui::Modifiers::SHIFT,
                                        egui::Key::Tab,
                                    );
                                    let forward =
                                        consume_exact(input, egui::Modifiers::NONE, egui::Key::Tab);
                                    if backward || forward {
                                        tab_pressed = Some(backward);
                                    }
                                });
                            }
                            if let Some((focus_id, cursor)) = self.focus
                                && focus_id == id
                            {
                                ui.memory_mut(|m| m.request_focus(widget_id));
                                let mut state = egui::TextEdit::load_state(ui.ctx(), widget_id)
                                    .unwrap_or_default();
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::one(
                                        egui::text::CCursor::new(cursor),
                                    )));
                                state.store(ui.ctx(), widget_id);
                            }
                            let (indent, text_width) = kind.margins();
                            let pos = origin + egui::vec2(indent * scale, y);
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt(("block", id))
                                    .max_rect(egui::Rect::from_min_size(
                                        pos,
                                        egui::vec2(text_width * scale, 10_000.0),
                                    ))
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            child.visuals_mut().text_cursor.stroke.color = egui::Color32::BLACK;
                            child.visuals_mut().selection.bg_fill =
                                egui::Color32::from_rgb(185, 215, 250);
                            child.visuals_mut().selection.stroke.color = egui::Color32::BLACK;
                            let output =
                                egui::TextEdit::multiline(&mut self.document.blocks[index].text)
                                    .id(widget_id)
                                    .font(egui::FontId::monospace(12.0 * scale))
                                    .text_color(egui::Color32::BLACK)
                                    .frame(egui::Frame::NONE)
                                    .margin(0)
                                    .desired_rows(1)
                                    .desired_width(text_width * scale)
                                    .lock_focus(true)
                                    .return_key(egui::KeyboardShortcut::new(
                                        egui::Modifiers::SHIFT,
                                        egui::Key::Enter,
                                    ))
                                    .hint_text(
                                        egui::RichText::new(kind.label())
                                            .color(egui::Color32::from_gray(110)),
                                    )
                                    .show(&mut child);
                            output.response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::TextEdit,
                                    true,
                                    kind.label(),
                                )
                            });
                            if output.response.has_focus() {
                                self.active = id;
                            }
                            if output.response.changed() {
                                typed = Some(id);
                            }
                            if self.focus.is_some_and(|(focus_id, _)| focus_id == id) {
                                output.response.scroll_to_me(None);
                                self.focus = None;
                            }
                            if output.response.has_focus() {
                                let range = output
                                    .cursor_range
                                    .unwrap_or_default()
                                    .as_sorted_char_range();
                                let range = range.start.0..range.end.0;
                                if enter {
                                    split = Some(range.clone());
                                }
                                if let Some(backward) = tab_pressed {
                                    tab = Some((backward, range));
                                }
                            }
                            let height = output.response.rect.height();
                            if self.active == id {
                                ui.painter().line_segment(
                                    [
                                        origin + egui::vec2(94.0 * scale, y),
                                        origin + egui::vec2(94.0 * scale, y + height),
                                    ],
                                    (2.0, egui::Color32::from_rgb(35, 90, 165)),
                                );
                            }
                            match kind {
                                Element::Character => {
                                    role = role_name(&self.document.blocks[index].text);
                                    role_top = y;
                                    if !role.is_empty() {
                                        let annotation = self.document.cast.get(&role).map_or_else(
                                            || "Uncast".into(),
                                            |c| format!("played by {}", c.name),
                                        );
                                        let label_pos = origin + egui::vec2(412.0 * scale, y);
                                        let galley = ui.painter().layout(
                                            annotation,
                                            egui::FontId::proportional(10.0 * scale),
                                            egui::Color32::from_gray(85),
                                            128.0 * scale,
                                        );
                                        let target = egui::Rect::from_min_size(
                                            label_pos - egui::vec2(2.0, 2.0),
                                            galley.size() + egui::vec2(4.0, 4.0),
                                        );
                                        ui.painter().galley(
                                            label_pos,
                                            galley,
                                            egui::Color32::from_gray(85),
                                        );
                                        self.role_ui(ui, &role, "", index, target, characters);
                                    }
                                }
                                Element::Heading | Element::Action | Element::Transition => {
                                    role.clear()
                                }
                                _ => {}
                            }
                            // Casting overlays exist only during gallery drags; ordinary clicks
                            // belong to the editor. The small cast label remains a picker.
                            if kind == Element::Dialogue
                                && !role.is_empty()
                                && egui::DragAndDrop::has_payload_of_type::<CastCharacter>(ui.ctx())
                            {
                                let target = egui::Rect::from_min_max(
                                    origin + egui::vec2(180.0 * scale, role_top - 2.0),
                                    origin + egui::vec2(540.0 * scale, y + height + 2.0),
                                );
                                self.role_ui(
                                    ui,
                                    &role,
                                    &self.document.blocks[index].text.clone(),
                                    index + self.document.blocks.len(),
                                    target,
                                    characters,
                                );
                            }
                            y += height
                                + if matches!(kind, Element::Character | Element::Parenthetical) {
                                    0.0
                                } else {
                                    12.0 * scale
                                };
                        }
                        let page = egui::Rect::from_min_size(
                            origin,
                            egui::vec2(width, (y + 72.0 * scale).max(792.0 * scale)),
                        );
                        ui.painter().set(
                            page_shape,
                            egui::Shape::rect_filled(page, 0.0, egui::Color32::WHITE),
                        );
                        ui.allocate_rect(page, egui::Sense::hover());
                    });
            });
        if let Some(range) = split {
            self.split(range);
            typed = None;
        }
        if let Some((backward, range)) = tab {
            self.tab(backward, range);
            typed = None;
        }
        if backspace_empty {
            self.backspace_empty();
            typed = None;
        }
        self.rename_ui(ui);
        if !history_navigation {
            self.record(before, typed);
        }
        if self.focus.is_some() {
            ui.ctx().request_repaint();
        }
        if let Some(at) = self.pending_save {
            if at.elapsed() >= Duration::from_millis(600) {
                self.save();
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_millis(600).saturating_sub(at.elapsed()));
            }
        }
    }

    fn rename_ui(&mut self, ui: &mut egui::Ui) {
        let Some((old, mut new)) = self.rename.clone() else {
            return;
        };
        let mut open = true;
        let mut applied = false;
        egui::Window::new("Rename character")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(format!("Rename every {old} cue and keep its cast."));
                ui.label("Character name");
                ui.text_edit_singleline(&mut new);
                let invalid = new.trim().is_empty()
                    || (role_name(&new) != old && self.roles().contains(&role_name(&new)));
                if invalid {
                    ui.label("Use a nonempty name that is not already in the script.");
                }
                if ui
                    .add_enabled(!invalid, egui::Button::new("Rename"))
                    .clicked()
                {
                    applied = self.rename_role(&old, &new).is_ok();
                }
            });
        self.rename = if open && !applied {
            Some((old, new))
        } else {
            None
        };
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
