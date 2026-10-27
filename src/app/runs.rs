use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::image_preview::ImagePreview;

#[derive(Default)]
pub(super) struct RunsPanel {
    project_root: PathBuf,
    names: Vec<String>,
    refreshed_at: Option<Instant>,
    load_failed: bool,
    selected: Option<String>,
    storyboards: Vec<ImagePreview>,
    storyboards_failed: bool,
}

impl RunsPanel {
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, project_root: &Path) {
        let refresh_interval = Duration::from_secs(2);
        if self.project_root != project_root {
            self.project_root = project_root.to_owned();
            self.selected = None;
            self.storyboards.clear();
            self.storyboards_failed = false;
            self.refreshed_at = None;
        }
        if self
            .refreshed_at
            .is_none_or(|last| last.elapsed() >= refresh_interval)
        {
            match read_runs(&project_root.join("runs")) {
                Ok(names) => {
                    self.names = names;
                    self.load_failed = false;
                }
                Err(_) => {
                    self.names.clear();
                    self.load_failed = true;
                }
            }
            if self
                .selected
                .as_ref()
                .is_some_and(|name| !self.names.contains(name))
            {
                self.selected = None;
                self.storyboards.clear();
                self.storyboards_failed = false;
            }
            self.refresh_storyboards();
            self.refreshed_at = Some(Instant::now());
        }
        ui.ctx().request_repaint_after(refresh_interval);

        ui.heading("Runs");
        if self.load_failed {
            ui.label("Could not read this project's runs.");
        } else if self.names.is_empty() {
            ui.label("No runs yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt(("project_runs", project_root))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut clicked = None;
                    for name in &self.names {
                        ui.add_space(6.0);
                        if ui
                            .add(
                                egui::Button::selectable(
                                    self.selected.as_ref() == Some(name),
                                    egui::RichText::new(name).monospace(),
                                )
                                .wrap()
                                .min_size(egui::vec2(0.0, 44.0)),
                            )
                            .on_hover_text(
                                project_root.join("runs").join(name).display().to_string(),
                            )
                            .clicked()
                        {
                            clicked = Some(name.clone());
                        }
                    }
                    if let Some(name) = clicked
                        && self.selected.as_ref() != Some(&name)
                    {
                        self.selected = Some(name);
                        self.storyboards.clear();
                        self.refresh_storyboards();
                    }
                    if self.selected.is_some() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Storyboards").strong());
                        if self.storyboards_failed {
                            ui.label("Could not read this run's storyboards.");
                        } else if self.storyboards.is_empty() {
                            ui.label("No storyboards yet.");
                        }
                        for storyboard in &mut self.storyboards {
                            storyboard.ui(ui);
                        }
                    }
                });
        }
    }

    fn refresh_storyboards(&mut self) {
        let Some(name) = &self.selected else {
            return;
        };
        let directory = self
            .project_root
            .join("runs")
            .join(name)
            .join("storyboards");
        self.storyboards_failed = read_storyboards(&directory, &mut self.storyboards).is_err();
    }
}

fn read_storyboards(directory: &Path, storyboards: &mut Vec<ImagePreview>) -> std::io::Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            storyboards.clear();
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        // Only finished images, excluding request files and generation work directories.
        if !entry.file_type()?.is_file()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        {
            continue;
        }
        let metadata = entry.metadata()?;
        let label = entry.file_name().to_string_lossy().into_owned();
        if let Some(storyboard) = storyboards.iter_mut().find(|item| item.path == path) {
            storyboard.refresh(label, &metadata);
        } else {
            storyboards.push(ImagePreview::new(path.clone(), label, &metadata));
        }
        paths.push(path);
    }
    storyboards.retain(|storyboard| paths.contains(&storyboard.path));
    storyboards.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

fn read_runs(directory: &Path) -> std::io::Result<Vec<String>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    // Run directory names begin with a sortable timestamp.
    names.sort_by(|a, b| b.cmp(a));
    Ok(names)
}
