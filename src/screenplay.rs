// A single sample scene, using Letter paper and conventional screenplay margins.
// Keep this first pass a document preview; rendering still uses the context prompt.
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
                    for (indent, width, text) in [
                        (108.0, 432.0, "EXT. DESERT ROAD - DUSK"),
                        (108.0, 432.0, "A two-lane road cuts through red rock. An old station wagon sits on the shoulder, its hood raised."),
                        (108.0, 432.0, "AVA, 30s, turns a battered postcard over in her hands. ELI, 30s, emerges from behind the engine."),
                        (252.0, 288.0, "ELI"),
                        (180.0, 252.0, "Good news. It's definitely the car."),
                        (108.0, 432.0, "Ava almost smiles. She looks down the empty road."),
                        (252.0, 288.0, "AVA"),
                        (180.0, 252.0, "My dad said you could see the ocean from here."),
                        (252.0, 288.0, "ELI"),
                        (216.0, 180.0, "(looking around)"),
                        (180.0, 252.0, "He was an optimistic man."),
                        (108.0, 432.0, "She hands him the postcard. The same road, forty years younger. On the back: KEEP GOING."),
                        (252.0, 288.0, "AVA"),
                        (180.0, 252.0, "He made it sound closer."),
                        (108.0, 432.0, "Eli lowers the hood. Beyond the rocks, a thin line of gold catches the last of the sun."),
                        (252.0, 288.0, "ELI"),
                        (180.0, 252.0, "We've still got some light."),
                        (108.0, 432.0, "Ava pockets the postcard. They start walking."),
                        (432.0, 108.0, "FADE OUT."),
                    ] {
                        let galley = ui.painter().layout(
                            text.to_owned(),
                            font.clone(),
                            egui::Color32::BLACK,
                            width * scale,
                        );
                        let height = galley.size().y;
                        // Character cues and parentheticals stay with the dialogue.
                        let gap = if indent == 252.0 || indent == 216.0 { 0.0 } else { 12.0 * scale };
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
