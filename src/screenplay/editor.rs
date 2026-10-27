use std::time::Duration;

use crate::characters::CastCharacter;

use super::{Element, Screenplay, role_name};

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
                ui.label("Backspace on an empty heading: remove it and go back.");
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
