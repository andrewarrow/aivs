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

pub fn render_prompt() -> String {
    let mut cast = Vec::new();
    for &(element, text) in SCENE {
        if element == Element::Character && !cast.contains(&text) {
            cast.push(text);
        }
    }
    let references = cast
        .iter()
        .map(|name| format!("@{}", name.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" and ");
    let mut prompt = format!(
        "A single continuous cinematic shot featuring {} saved characters: {references}. \
         Frame this as a locked-off wide shot with every character's entire head and torso visible \
         down to the waist, leaving landscape around them throughout. A fixed tripod camera at eye \
         level holds a constant focal length and camera-to-subject distance for the entire conversation. \
         Each character stays at the same distance while speaking, with only natural head and body motion. \
         Keep everyone visible together, with breathing room above their heads. No zoom, push-in, \
         close-up, reframing, or cuts. \
         Preserve each character's saved appearance and distinct voice throughout the scene. \
         Play the following action and dialogue in order, at a natural conversational pace. \
         Only the person delivering a line speaks and moves their mouth; everyone else listens \
         with natural reactions. Each character's mouth follows their own voice. \
         Scene headings, action, character names, and parentheticals are directions, not spoken words. \
         No narration, subtitles, captions, or on-screen text.\n\n",
        cast.len()
    );
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
                prompt.push_str(speaker);
                if !cue.is_empty() {
                    prompt.push(' ');
                    prompt.push_str(cue);
                }
                prompt.push_str(": \"");
                prompt.push_str(text);
                prompt.push_str("\"\n\n");
                cue = "";
            }
            _ => {
                prompt.push_str(text);
                prompt.push_str("\n\n");
            }
        }
    }
    prompt
}

// Letter paper with conventional screenplay margins.
pub fn ui(ui: &mut egui::Ui) {
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
                    for &(element, text) in SCENE {
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
                        let height = galley.size().y;
                        // Character cues and parentheticals stay with the dialogue.
                        let gap = if matches!(element, Element::Character | Element::Parenthetical)
                        {
                            0.0
                        } else {
                            12.0 * scale
                        };
                        ui.put(
                            egui::Rect::from_min_size(
                                origin + egui::vec2(indent * scale, y),
                                galley.size(),
                            ),
                            egui::Label::new(galley).selectable(true),
                        );
                        y += height + gap;
                    }
                    ui.allocate_rect(page, egui::Sense::hover());
                });
        });
}
