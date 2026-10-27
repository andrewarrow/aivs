use std::path::PathBuf;

use super::{App, AppCommand};
use crate::{project::Project, screenplay::Screenplay};

#[derive(Clone, Copy)]
pub(crate) enum SaveChangesChoice {
    Save,
    Discard,
    Cancel,
}

pub(super) enum ProjectAction {
    New,
    Open(PathBuf),
    Quit,
}

impl App {
    fn project_has_changes(&self) -> bool {
        self.project.is_untitled() || self.screenplay.has_changes()
    }

    pub(crate) fn window_title(&self) -> String {
        format!(
            "{}{} — AIVS",
            self.project.name(),
            if self.screenplay.has_changes() {
                " *"
            } else {
                ""
            }
        )
    }

    pub(crate) fn request_close(&mut self) {
        if self.pending_project_action.is_none() {
            self.request_project_action(ProjectAction::Quit);
        }
    }

    pub(super) fn request_project_action(&mut self, action: ProjectAction) {
        if self.project_has_changes() {
            self.pending_project_action = Some(action);
        } else {
            self.perform_project_action(action);
        }
    }

    fn perform_project_action(&mut self, action: ProjectAction) {
        let result = match action {
            ProjectAction::Quit => {
                self.quit = true;
                return;
            }
            ProjectAction::New => Project::untitled(&self.root, false),
            ProjectAction::Open(path) => Project::open(&path),
        };
        match result {
            Ok(project) => {
                let mut screenplay = if project.root().join("screenplay.json").is_file() {
                    Screenplay::load(project.root())
                } else {
                    Screenplay::untitled(project.root())
                };
                if let Err(error) = screenplay.project_document() {
                    self.import_error = Some(error);
                    return;
                }
                screenplay.set_project(project.root(), project.is_untitled());
                self.screenplay = screenplay;
                self.video.set_project(project.root(), true);
                self.characters = crate::characters::CharacterGallery::new(project.root());
                self.sets = crate::sets::SetGallery::new(project.root());
                self.project = project;
                self.import_error = None;
                if self.codex_started {
                    self.reconnect();
                }
            }
            Err(error) => self.import_error = Some(error),
        }
    }

    pub(super) fn save_project(&mut self, save_as: bool) -> Result<bool, String> {
        if self.project.is_untitled() || save_as {
            let Some(destination) = crate::project::choose_save(&self.project)? else {
                return Ok(false);
            };
            let document = self.screenplay.project_document()?;
            let old = self.project.root().to_owned();
            if destination == old {
                self.screenplay.flush()?;
                self.project.save()?;
            } else {
                self.project.save_as(&destination, &document)?;
                self.screenplay.set_project(self.project.root(), false);
                self.video.relocate_project(&old, self.project.root());
                self.characters = crate::characters::CharacterGallery::new(self.project.root());
                self.sets = crate::sets::SetGallery::new(self.project.root());
                if self.codex_started {
                    self.reconnect();
                }
            }
        } else {
            self.screenplay.flush()?;
            self.project.save()?;
        }
        self.screenplay.mark_saved();
        self.import_error = None;
        Ok(true)
    }

    pub(super) fn project_shortcuts(&mut self, ctx: &egui::Context) {
        for (modifiers, key, command) in [
            (
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
                AppCommand::SaveProjectAs,
            ),
            (
                egui::Modifiers::COMMAND,
                egui::Key::S,
                AppCommand::SaveProject,
            ),
            (
                egui::Modifiers::COMMAND,
                egui::Key::O,
                AppCommand::OpenProject,
            ),
            (
                egui::Modifiers::COMMAND,
                egui::Key::N,
                AppCommand::NewProject,
            ),
        ] {
            if ctx.input_mut(|input| input.consume_key(modifiers, key)) {
                self.execute_command(command);
            }
        }
    }

    pub(super) fn project_confirmation(&mut self, ctx: &egui::Context) {
        if self.pending_project_action.is_none() {
            return;
        }
        #[cfg(target_os = "macos")]
        let choice = {
            let can_save = !self.busy && (!self.project.is_untitled() || !self.video.has_job());
            let choice = crate::macos::confirm_save_changes(&self.project.name(), can_save);
            // AppKit runs its own modal loop; redraw after it returns.
            ctx.request_repaint();
            Some(choice)
        };
        #[cfg(not(target_os = "macos"))]
        let choice = self.save_changes_ui(ctx);
        match choice {
            Some(SaveChangesChoice::Save) => match self.save_project(false) {
                Ok(true) => {
                    if let Some(action) = self.pending_project_action.take() {
                        self.perform_project_action(action);
                    }
                }
                Ok(false) => self.pending_project_action = None,
                Err(error) => {
                    #[cfg(target_os = "macos")]
                    {
                        self.pending_project_action = None;
                        crate::macos::show_error("Could not save project", &error);
                    }
                    self.import_error = Some(error);
                }
            },
            Some(SaveChangesChoice::Discard) => {
                if let Some(action) = self.pending_project_action.take() {
                    self.perform_project_action(action);
                }
            }
            Some(SaveChangesChoice::Cancel) => self.pending_project_action = None,
            None => {}
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn save_changes_ui(&self, ctx: &egui::Context) -> Option<SaveChangesChoice> {
        let mut choice = None;
        egui::Modal::new(egui::Id::new("save_project_changes")).show(ctx, |ui| {
            ui.set_max_width((ctx.content_rect().width() - 64.0).clamp(240.0, 400.0));
            ui.heading(format!("Save changes to {}?", self.project.name()));
            ui.label("Your unsaved changes will be lost if you don't save them.");
            if let Some(error) = &self.import_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().interact_size.y = 44.0;
                let can_save = !self.busy && (!self.project.is_untitled() || !self.video.has_job());
                if ui
                    .add_enabled(can_save, egui::Button::new("Save"))
                    .on_disabled_hover_text("Stop the current job before saving this project.")
                    .clicked()
                {
                    choice = Some(SaveChangesChoice::Save);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(SaveChangesChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(SaveChangesChoice::Cancel);
                }
            });
        });
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            choice = Some(SaveChangesChoice::Cancel);
        }
        choice
    }
}
