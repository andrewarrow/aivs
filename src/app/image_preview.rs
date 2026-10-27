use std::{
    fs::Metadata,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, SystemTime},
};

pub(super) struct ImagePreview {
    pub(super) path: PathBuf,
    pub(super) label: String,
    stamp: (Option<SystemTime>, u64),
    texture: Option<egui::TextureHandle>,
    loading: Option<Receiver<Result<egui::ColorImage, String>>>,
}

impl ImagePreview {
    pub(super) fn new(path: PathBuf, label: String, metadata: &Metadata) -> Self {
        Self {
            loading: Some(load_image(path.clone())),
            path,
            label,
            stamp: (metadata.modified().ok(), metadata.len()),
            texture: None,
        }
    }

    pub(super) fn refresh(&mut self, label: String, metadata: &Metadata) {
        self.label = label;
        let stamp = (metadata.modified().ok(), metadata.len());
        if self.stamp != stamp || (self.texture.is_none() && self.loading.is_none()) {
            self.stamp = stamp;
            self.loading = Some(load_image(self.path.clone()));
        }
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(receiver) = &self.loading {
            match receiver.try_recv() {
                Ok(result) => {
                    self.texture = result.ok().map(|image| {
                        ui.ctx().load_texture(
                            self.path.display().to_string(),
                            image,
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                }
                Err(TryRecvError::Disconnected) => self.loading = None,
            }
        }
        ui.add_space(8.0);
        ui.add(egui::Label::new(&self.label).wrap())
            .on_hover_text(self.path.display().to_string());
        if let Some(texture) = &self.texture {
            let size = texture.size_vec2();
            let scale = (ui.available_width().max(1.0) / size.x).min(1.0);
            ui.add(egui::Image::new((texture.id(), size * scale)).alt_text(&self.label));
        } else if self.loading.is_some() {
            ui.label("Loading image…");
        } else {
            ui.label("Image unavailable.");
        }
    }
}

fn load_image(path: PathBuf) -> Receiver<Result<egui::ColorImage, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(crate::characters::decode_image(&path));
    });
    receiver
}
