use std::collections::BTreeMap;

use crate::characters::CastCharacter;

#[derive(Clone, Copy, PartialEq)]
enum Element {
    Heading,
    Action,
    Character,
    Parenthetical,
    Dialogue,
    Transition,
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

#[derive(Default)]
pub struct Screenplay {
    cast: BTreeMap<&'static str, CastCharacter>,
}

fn roles() -> Vec<&'static str> {
    let mut roles = Vec::new();
    for &(element, text) in SCENE {
        if element == Element::Character && !roles.contains(&text) {
            roles.push(text);
        }
    }
    roles
}

impl Screenplay {
    pub fn assign(&mut self, role: &str, character: CastCharacter) {
        if let Some(role) = roles().into_iter().find(|name| *name == role) {
            self.cast.insert(role, character);
        }
    }

    pub fn render_request(&self) -> Result<String, String> {
        let roles = roles();
        let missing = roles
            .iter()
            .filter(|role| !self.cast.contains_key(**role))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "Assign characters to {} before generating.",
                missing.join(" and ")
            ));
        }
        let mut script = String::new();
        let mut speaker = "";
        let mut cue = "";
        for &(element, text) in SCENE {
            match element {
                Element::Character => {
                    speaker = text;
                    cue = "";
                }
                Element::Parenthetical => cue = text,
                Element::Dialogue => {
                    script.push_str(speaker);
                    if !cue.is_empty() {
                        script.push(' ');
                        script.push_str(cue);
                    }
                    script.push_str(": \"");
                    script.push_str(text);
                    script.push_str("\"\n\n");
                    cue = "";
                }
                _ => {
                    script.push_str(text);
                    script.push_str("\n\n");
                }
            }
        }
        let cast = roles
            .iter()
            .map(|role| serde_json::json!({"role": role, "character": self.cast[role].id}))
            .collect::<Vec<_>>();
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "script": script.trim_end(),
            "cast": cast,
        }))
        .map_err(|error| format!("Could not serialize screenplay request: {error}"))
    }

    fn role_ui(
        &mut self,
        ui: &mut egui::Ui,
        role: &'static str,
        script_text: &str,
        index: usize,
        target: egui::Rect,
        characters: &[CastCharacter],
    ) {
        let response = ui.interact(target, ui.id().with(("role", index)), egui::Sense::click());
        let assignment = self
            .cast
            .get(role)
            .map_or("Uncast", |character| character.name.as_str())
            .to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Cast {role}: {assignment}. {script_text}"),
            )
        });
        if egui::DragAndDrop::has_payload_of_type::<CastCharacter>(ui.ctx())
            || response.has_focus()
            || response.hovered()
        {
            let color = if response.dnd_hover_payload::<CastCharacter>().is_some()
                || response.has_focus()
            {
                egui::Color32::from_rgb(35, 90, 165)
            } else {
                egui::Color32::from_gray(150)
            };
            ui.painter()
                .rect_stroke(target, 2.0, (1.0, color), egui::StrokeKind::Inside);
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
                        let selected = self.cast.get(role) == Some(character);
                        if ui.selectable_label(selected, &character.name).clicked() {
                            self.assign(role, character.clone());
                            ui.close();
                        }
                    }
                    if ui
                        .selectable_label(!self.cast.contains_key(role), "Uncast")
                        .clicked()
                    {
                        self.cast.remove(role);
                        ui.close();
                    }
                });
        });
        response.on_hover_text(format!(
            "{role}: {assignment}. Drop a character here or click to choose."
        ));
    }

    // Letter paper with conventional screenplay margins.
    pub fn ui(&mut self, ui: &mut egui::Ui, characters: &[CastCharacter]) {
        ui.horizontal(|ui| {
            ui.strong("Screenplay");
            ui.weak("Last Light · 1 page");
        });
        egui::Frame::new()
            .fill(egui::Color32::from_gray(48))
            .inner_margin(12)
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .id_salt("screenplay_page")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Fit ordinary panes; on very narrow windows, scroll the paper
                        // horizontally rather than making the script too small to read.
                        let scale = (ui.available_width() / 612.0).clamp(0.85, 1.0);
                        let size = egui::vec2(612.0, 792.0) * scale;
                        let left = ((ui.available_width() - size.x) * 0.5).max(0.0);
                        let origin = ui.cursor().min + egui::vec2(left, 0.0);
                        let page = egui::Rect::from_min_size(origin, size);
                        ui.painter().rect_filled(page, 0.0, egui::Color32::WHITE);
                        let font = egui::FontId::monospace(12.0 * scale);
                        ui.painter().text(
                            origin + egui::vec2(540.0, 36.0) * scale,
                            egui::Align2::RIGHT_TOP,
                            "1.",
                            font.clone(),
                            egui::Color32::BLACK,
                        );

                        let mut y = 72.0 * scale;
                        let mut role = "";
                        let mut role_text = String::new();
                        let mut role_top = y;
                        for (index, &(element, text)) in SCENE.iter().enumerate() {
                            if element == Element::Character {
                                role = text;
                                role_text.clear();
                                role_top = y;
                            }
                            let (indent, width) = match element {
                                Element::Character => (252.0, 288.0),
                                Element::Parenthetical => (216.0, 180.0),
                                Element::Dialogue => (180.0, 252.0),
                                Element::Transition => (432.0, 108.0),
                                _ => (108.0, 432.0),
                            };
                            let galley = ui.painter().layout(
                                text.to_owned(),
                                font.clone(),
                                egui::Color32::BLACK,
                                width * scale,
                            );
                            let text_size = galley.size();
                            let height = text_size.y;
                            // Character cues and parentheticals stay with the dialogue.
                            let gap =
                                if matches!(element, Element::Character | Element::Parenthetical) {
                                    0.0
                                } else {
                                    12.0 * scale
                                };
                            let pos = origin + egui::vec2(indent * scale, y);
                            if matches!(
                                element,
                                Element::Character | Element::Parenthetical | Element::Dialogue
                            ) {
                                if element != Element::Character {
                                    role_text.push_str(text);
                                    role_text.push('\n');
                                }
                                ui.painter().galley(pos, galley, egui::Color32::BLACK);
                            } else {
                                ui.put(
                                    egui::Rect::from_min_size(pos, galley.size()),
                                    egui::Label::new(galley).selectable(true),
                                );
                            }
                            if element == Element::Character {
                                let assignment = self.cast.get(role).map_or_else(
                                    || "Uncast".to_owned(),
                                    |character| format!("played by {}", character.name),
                                );
                                let annotation = ui.painter().layout(
                                    assignment,
                                    egui::FontId::proportional(10.0 * scale),
                                    egui::Color32::from_gray(85),
                                    (540.0 - indent) * scale - text_size.x - 8.0 * scale,
                                );
                                ui.painter().galley(
                                    pos + egui::vec2(text_size.x + 8.0 * scale, 0.0),
                                    annotation,
                                    egui::Color32::from_gray(85),
                                );
                            }
                            if element == Element::Dialogue {
                                // One target includes the cue, parenthetical, and spoken line.
                                // Every target for a role reads and updates the same cast entry.
                                let target = egui::Rect::from_min_max(
                                    origin + egui::vec2(180.0 * scale, role_top - 4.0 * scale),
                                    origin + egui::vec2(540.0 * scale, y + height + 4.0 * scale),
                                );
                                self.role_ui(ui, role, &role_text, index, target, characters);
                            }
                            y += height + gap;
                        }
                        ui.allocate_rect(page, egui::Sense::hover());
                    });
            });
    }
}

#[cfg(test)]
mod tests;
