use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::image_preview::ImagePreview;

#[derive(Default)]
pub(super) struct ReferencesPanel {
    project_root: PathBuf,
    references: Vec<ImagePreview>,
    refreshed_at: Option<Instant>,
    load_failed: bool,
}

impl ReferencesPanel {
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, project_root: &Path) {
        if self.project_root != project_root {
            self.project_root = project_root.to_owned();
            self.references.clear();
            self.refreshed_at = None;
        }
        let interval = Duration::from_secs(2);
        if self
            .refreshed_at
            .is_none_or(|last| last.elapsed() >= interval)
        {
            self.load_failed = self.refresh().is_err();
            self.refreshed_at = Some(Instant::now());
        }
        ui.ctx().request_repaint_after(interval);

        ui.heading("References");
        if self.load_failed {
            ui.label("Could not read this project's references.");
        } else if self.references.is_empty() {
            ui.label("No references yet.");
        }
        egui::ScrollArea::vertical()
            .id_salt(("project_references", project_root))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for reference in &mut self.references {
                    reference.ui(ui);
                }
            });
    }

    fn refresh(&mut self) -> std::io::Result<()> {
        let directory = self.project_root.join("references");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.references.clear();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("reference.png");
            let metadata = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let data = fs::read(entry.path().join("reference.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .unwrap_or_default();
            let label = data["name"]
                .as_str()
                .or_else(|| data["id"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            if let Some(reference) = self.references.iter_mut().find(|item| item.path == path) {
                reference.refresh(label, &metadata);
            } else {
                self.references
                    .push(ImagePreview::new(path.clone(), label, &metadata));
            }
            paths.push(path);
        }
        self.references
            .retain(|reference| paths.contains(&reference.path));
        self.references
            .sort_by(|a, b| a.label.cmp(&b.label).then(a.path.cmp(&b.path)));
        Ok(())
    }
}
