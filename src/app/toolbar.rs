pub(super) enum GalleryIcon {
    Characters,
    Sets,
}

impl GalleryIcon {
    pub(super) fn toggle(self, ui: &mut egui::Ui, visible: &mut bool) {
        let label = match self {
            Self::Characters => "Characters",
            Self::Sets => "Sets",
        };
        let mut response =
            ui.add(egui::Button::selectable(*visible, "").min_size(egui::vec2(44.0, 44.0)));
        if response.clicked() {
            *visible = !*visible;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), *visible, label)
        });
        let stroke = egui::Stroke::new(
            1.5,
            ui.style()
                .interact_selectable(&response, *visible)
                .fg_stroke
                .color,
        );
        let painter = ui.painter();
        let center = response.rect.center();
        let point = |x, y| center + egui::vec2(x, y);
        match self {
            Self::Characters => {
                painter.circle_stroke(point(-4.0, -5.0), 3.5, stroke);
                painter.circle_stroke(point(5.0, -5.0), 3.0, stroke);
                painter.add(egui::Shape::line(
                    vec![
                        point(-11.0, 8.0),
                        point(-11.0, 5.0),
                        point(-9.0, 2.0),
                        point(-4.0, 1.0),
                        point(1.0, 2.0),
                        point(3.0, 5.0),
                        point(3.0, 8.0),
                    ],
                    stroke,
                ));
                painter.add(egui::Shape::line(
                    vec![
                        point(5.0, 1.0),
                        point(8.0, 2.0),
                        point(10.0, 4.0),
                        point(10.0, 8.0),
                    ],
                    stroke,
                ));
            }
            Self::Sets => {
                painter.rect_stroke(
                    egui::Rect::from_center_size(center, egui::vec2(22.0, 18.0)),
                    2.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
                painter.circle_stroke(point(-5.0, -4.0), 2.0, stroke);
                painter.add(egui::Shape::line(
                    vec![
                        point(-8.0, 6.0),
                        point(-3.0, 0.0),
                        point(1.0, 4.0),
                        point(5.0, 0.0),
                        point(8.0, 6.0),
                    ],
                    stroke,
                ));
            }
        }
        response.on_hover_text(format!(
            "{} {label}",
            if *visible { "Hide" } else { "Show" }
        ));
    }
}
