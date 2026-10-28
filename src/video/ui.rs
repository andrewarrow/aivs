use std::{path::Path, process::Command};

use super::{Setup, Task, VideoWorkspace, remaining_time};

impl VideoWorkspace {
    pub fn setup_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            if self.setup == Setup::Checking {
                ui.set_max_width(480.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.add(egui::Label::new(&self.status).wrap());
                });
                if self.started_at.elapsed().as_secs() >= 3 {
                    ui.label(format!(
                        "{} elapsed",
                        remaining_time(self.started_at.elapsed().as_secs() as f64)
                    ));
                }
                self.activity_ui(ui, 120.0);
                if ui
                    .add_enabled(
                        self.job.is_some() && !self.stopping,
                        egui::Button::new(if self.stopping { "Stopping…" } else { "Stop" })
                            .min_size(egui::vec2(64.0, 44.0)),
                    )
                    .clicked()
                {
                    if let Some(job) = &self.job {
                        job.stop();
                    }
                    self.stopping = true;
                }
            }
        });
        // A ready install goes directly from the startup check to its workspace.
        if self.setup == Setup::Checking {
            return;
        }
        let ctx = ui.ctx().clone();
        let width = (ctx.content_rect().width() - 64.0).clamp(240.0, 480.0);
        let max_height = (ctx.content_rect().height() - 64.0).clamp(120.0, 560.0);
        egui::Modal::new(egui::Id::new("video_setup")).show(&ctx, |ui| {
            ui.set_width(width);
            // Areas remember their previous size. Give the body room to grow
            // when a short progress message becomes a long setup error.
            ui.set_max_height(max_height);
            egui::ScrollArea::vertical()
                .id_salt("video_setup_body")
                .auto_shrink([false, true])
                .max_height((max_height - 60.0).max(1.0))
                .show(ui, |ui| {
                    ui.heading("Set up AIVS");
                    ui.add_space(8.0);
                    if self.setup == Setup::NeedsToken {
                        ui.label("Get an access token from Hugging Face, then create a .env file in this workspace with HF_API set to your token.");
                        ui.hyperlink_to("Get a Hugging Face access token", "https://huggingface.co/settings/tokens");
                        ui.label(egui::RichText::new("HF_API=your_token").monospace());
                        ui.add(egui::Label::new(format!("File: {}", self.root.join(".env").display())).wrap());
                    } else {
                        ui.label("Python, the Apple GPU renderer, and the model cache must be ready before you can continue.");
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(&self.status).wrap());
                        if self.job.is_some() {
                            let mut progress = egui::ProgressBar::new(self.fraction.unwrap_or(0.0))
                                .animate(self.fraction.is_none())
                                .desired_width(width);
                            if self.fraction.is_some() {
                                progress = progress.show_percentage();
                            }
                            ui.add(progress);
                            ui.label("Setup checks space for the video models before downloading weights. Codex generates storyboard images. Cached files are reused. Keep AIVS open.");
                        }
                        self.error_ui(ui);
                        self.activity_ui(ui, 120.0);
                    }
                });
            // Recovery stays outside the scrollable diagnostics.
            let retry_label = match self.setup {
                Setup::NeedsToken => Some("Check again"),
                Setup::Failed => Some("Retry setup"),
                _ => None,
            };
            if let Some(label) = retry_label {
                ui.add_space(8.0);
                if ui.add(egui::Button::new(label).min_size(egui::vec2(100.0, 44.0))).clicked() {
                    self.start(Task::Setup);
                }
            }
            // Escape and backdrop clicks deliberately do not unlock setup.
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, screenplay: &crate::screenplay::Screenplay) {
        let scenes = screenplay.scenes();
        if !scenes
            .iter()
            .any(|scene| Some(scene.id) == self.selected_scene)
        {
            self.selected_scene = scenes.first().map(|scene| scene.id);
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(760.0));
                ui.heading("Talking video");
                ui.add(
                    egui::Label::new(format!(
                        "LTX-2.5 · {} · 24 fps · generated speech and video",
                        match self.render_profile {
                            crate::settings::RenderProfile::Distilled => match self.render_format {
                                crate::settings::RenderFormat::Landscape => "320 × 180 · 16:9",
                                crate::settings::RenderFormat::Portrait => "180 × 320 · 9:16",
                            },
                            crate::settings::RenderProfile::Hq => self.render_format.label(),
                        }
                    ))
                    .wrap(),
                );
                ui.add_space(16.0);
                let scene_label = ui.label("Scene");
                let selected_text = scenes
                    .iter()
                    .enumerate()
                    .find(|(_, scene)| Some(scene.id) == self.selected_scene)
                    .map_or_else(
                        || "No scenes".to_owned(),
                        |(index, scene)| format!("{}. {}", index + 1, scene.heading),
                    );
                ui.add_enabled_ui(self.job.is_none() && !scenes.is_empty(), |ui| {
                    ui.spacing_mut().interact_size.y = 44.0;
                    egui::ComboBox::from_id_salt("render_scene")
                        .selected_text(&selected_text)
                        .width(ui.available_width())
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (index, scene) in scenes.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected_scene,
                                    Some(scene.id),
                                    format!("{}. {}", index + 1, scene.heading),
                                )
                                .on_hover_text(scene.heading);
                            }
                        })
                        .response
                        .labelled_by(scene_label.id)
                        .on_hover_text(&selected_text);
                });
                ui.add_space(8.0);
                ui.add_enabled_ui(self.job.is_none(), |ui| {
                    let mut profile = self.render_profile;
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("Quality");
                        ui.spacing_mut().interact_size.y = 44.0;
                        egui::ComboBox::from_id_salt("render_profile")
                            .selected_text(profile.label())
                            .show_ui(ui, |ui| {
                                for option in [
                                    crate::settings::RenderProfile::Hq,
                                    crate::settings::RenderProfile::Distilled,
                                ] {
                                    ui.selectable_value(&mut profile, option, option.label());
                                }
                            })
                            .response
                            .labelled_by(label.id)
                            .on_hover_text(
                                "Preview renders a small 180p video for speed. Final uses full resolution. Both use the same storyboards, shot timing, and audio workflow; generated details may differ.",
                            );
                    });
                    if profile != self.render_profile {
                        match profile.save(self.project_root.as_deref().unwrap_or(&self.root)) {
                            Ok(()) => self.render_profile = profile,
                            Err(error) => self.error = Some(error),
                        }
                    }
                });
                ui.add_space(8.0);
                let request = self
                    .selected_scene
                    .ok_or_else(|| "Add a scene heading before generating.".to_owned())
                    .and_then(|scene_id| self.render_request(screenplay, scene_id));
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.job.is_none() && request.is_ok(),
                            egui::Button::new("Generate video").min_size(egui::vec2(128.0, 44.0)),
                        )
                        .clicked()
                        && let Ok(request) = &request
                    {
                        self.start(Task::Render(request.clone()));
                    }
                    if self.rendering
                        && ui
                            .add_enabled(
                                !self.stopping,
                                egui::Button::new("Stop").min_size(egui::vec2(64.0, 44.0)),
                            )
                            .clicked()
                    {
                        if let Some(job) = &self.job {
                            job.stop();
                        }
                        self.stopping = true;
                    }
                    if let Some(path) = self.output.clone()
                        && ui
                            .add(egui::Button::new("Show files").min_size(egui::vec2(96.0, 44.0)))
                            .clicked()
                        && let Some(parent) = path.parent()
                    {
                        self.open(parent);
                    }
                });
                if let Err(message) = &request {
                    ui.add(egui::Label::new(message).wrap());
                }
                ui.add_space(8.0);
                if self.rendering
                    && let Some(fraction) = self.fraction
                {
                    let fraction = fraction.min(0.99);
                    let text = self.remaining_seconds.map_or_else(
                        || "Estimating time remaining…".into(),
                        |seconds| {
                            let seconds =
                                seconds - self.remaining_updated_at.elapsed().as_secs_f64();
                            if seconds <= 0.0 {
                                return "Updating time estimate…".into();
                            }
                            format!(
                                "≈{:.0}% · About {} left",
                                fraction * 100.0,
                                remaining_time(seconds.max(1.0))
                            )
                        },
                    );
                    ui.add(egui::ProgressBar::new(fraction).text(text));
                }
                if self.rendering && self.fraction.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let status = if self.stopping {
                            "Stopping…"
                        } else {
                            &self.status
                        };
                        ui.add(
                            egui::Label::new(format!(
                                "{status} · {} elapsed",
                                remaining_time(self.started_at.elapsed().as_secs() as f64)
                            ))
                            .wrap(),
                        );
                    });
                } else if self.stopping && self.rendering {
                    ui.label("Stopping…");
                } else if !self.status.is_empty() {
                    ui.add(egui::Label::new(&self.status).wrap());
                }
                self.error_ui(ui);
                self.activity_ui(ui, 240.0);
                if let Some(path) = self.output.clone() {
                    ui.add_space(8.0);
                    let response = ui.add(
                        egui::Button::new("Reveal in Finder").min_size(egui::vec2(0.0, 44.0)),
                    );
                    if self.reveal_output {
                        response.scroll_to_me(Some(egui::Align::Center));
                        self.reveal_output = false;
                    }
                    if response.clicked() {
                        match Command::new("open").arg("-R").arg(&path).status() {
                            Ok(status) if status.success() => {}
                            _ => {
                                self.error = Some("Could not reveal the video in Finder.".into());
                            }
                        }
                    }
                }
            });
    }

    fn error_ui(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.add(egui::Label::new(format!("Error: {error}")).wrap());
        }
    }

    fn activity_ui(&self, ui: &mut egui::Ui, max_height: f32) {
        if !self.logs.is_empty() {
            egui::CollapsingHeader::new("Activity")
                .id_salt(("video_activity", self.started_at))
                .default_open(self.rendering)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("video_activity")
                        .max_height(max_height)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &self.logs {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(line).monospace()).wrap(),
                                );
                            }
                        });
                });
        }
    }

    fn open(&mut self, path: &Path) {
        if let Err(error) = Command::new("open").arg(path).spawn() {
            self.error = Some(format!("Could not open output: {error}"));
        }
    }
}
