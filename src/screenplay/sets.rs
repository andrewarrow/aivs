use super::Screenplay;
use crate::sets::SceneSet;

impl Screenplay {
    pub(super) fn scene_set_ui(
        &mut self,
        ui: &mut egui::Ui,
        heading_id: u64,
        target: egui::Rect,
        sets: &[SceneSet],
    ) {
        let response = ui.interact(
            target,
            ui.id().with(("scene_set", heading_id)),
            egui::Sense::click(),
        );
        let assignment = self
            .document
            .sets
            .get(&heading_id)
            .map_or("Unassigned", |set| set.name.as_str())
            .to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Scene set: {assignment}"),
            )
        });
        if response.hovered()
            || response.has_focus()
            || response.dnd_hover_payload::<SceneSet>().is_some()
        {
            ui.painter().rect_stroke(
                target,
                2.0,
                (1.0, egui::Color32::from_rgb(35, 90, 165)),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(set) = response.dnd_release_payload::<SceneSet>() {
            self.document.sets.insert(heading_id, (*set).clone());
            ui.ctx().request_repaint();
        }
        egui::Popup::menu(&response).show(|ui| {
            ui.label("Scene set");
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .show(ui, |ui| {
                    for set in sets {
                        if ui
                            .selectable_label(
                                self.document.sets.get(&heading_id) == Some(set),
                                &set.name,
                            )
                            .clicked()
                        {
                            self.document.sets.insert(heading_id, set.clone());
                            ui.close();
                        }
                    }
                    if ui
                        .selectable_label(
                            !self.document.sets.contains_key(&heading_id),
                            "Unassigned",
                        )
                        .clicked()
                    {
                        self.document.sets.remove(&heading_id);
                        ui.close();
                    }
                });
        });
        response.on_hover_text(format!(
            "Set: {assignment}. Drop a set onto this heading or click to choose."
        ));
    }
}
