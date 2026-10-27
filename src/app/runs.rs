use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct RunsPanel {
    project_root: PathBuf,
    names: Vec<String>,
    refreshed_at: Option<Instant>,
    load_failed: bool,
}

impl RunsPanel {
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, project_root: &Path) {
        let refresh_interval = Duration::from_secs(2);
        if self.project_root != project_root
            || self
                .refreshed_at
                .is_none_or(|last| last.elapsed() >= refresh_interval)
        {
            self.project_root = project_root.to_owned();
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
                    for name in &self.names {
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(egui::RichText::new(name).monospace()).wrap())
                            .on_hover_text(project_root.join("runs").join(name).display().to_string());
                    }
                });
        }
    }
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
