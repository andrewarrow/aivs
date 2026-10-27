use super::*;

mod casting;
mod editing;
mod imports;
mod persistence;
mod rendering;

fn character(name: &str) -> CastCharacter {
    CastCharacter {
        id: name.to_lowercase(),
        name: name.into(),
    }
}

struct Editor {
    ctx: egui::Context,
    time: f64,
}

impl Editor {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
        }
    }

    fn frame(&mut self, screenplay: &mut Screenplay, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 0.05;
        self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 1000.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| screenplay.ui(ui, &[character("Ava"), character("Eli")], &[]),
        )
    }

    fn key(&mut self, screenplay: &mut Screenplay, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(
            screenplay,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        );
        self.frame(
            screenplay,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            }],
        );
    }

    fn focus(&mut self, screenplay: &mut Screenplay, index: usize, cursor: usize) {
        screenplay.focus = Some((screenplay.document.blocks[index].id, cursor));
        self.frame(screenplay, vec![]);
        self.frame(screenplay, vec![]);
    }

    fn type_text(&mut self, screenplay: &mut Screenplay, text: &str) {
        self.frame(screenplay, vec![egui::Event::Text(text.into())]);
    }

    fn click(&mut self, screenplay: &mut Screenplay, pos: egui::Pos2) {
        self.frame(screenplay, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(
                screenplay,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }
}
