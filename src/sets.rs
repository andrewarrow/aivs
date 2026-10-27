use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSet {
    pub id: String,
    pub name: String,
}

struct SetReference {
    set: SceneSet,
    description: String,
    image: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

pub struct SetGallery {
    sets: Vec<SetReference>,
    selected: Option<usize>,
    loading: Option<Receiver<Self>>,
    load_failed: bool,
}

impl SetGallery {
    pub fn load_in_background(root: &Path) -> Self {
        let root = root.to_owned();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut startup = crate::startup::Timer::new();
            let gallery = Self::load(&root);
            startup.mark("background set gallery loaded");
            let _ = sender.send(gallery);
        });
        Self {
            sets: Vec::new(),
            selected: None,
            loading: Some(receiver),
            load_failed: false,
        }
    }

    pub fn load(root: &Path) -> Self {
        let directory = if root.join("sets").is_dir() {
            root.join("sets")
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sets")
        };
        let mut sets = fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| {
                let folder = entry.path();
                let data = fs::read(folder.join("set.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .unwrap_or_default();
                let fallback = folder
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Set")
                    .to_owned();
                let file = data["image"]["file"].as_str().unwrap_or("reference.png");
                SetReference {
                    set: SceneSet {
                        id: data["id"].as_str().unwrap_or(&fallback).to_owned(),
                        name: data["name"].as_str().unwrap_or(&fallback).to_owned(),
                    },
                    description: data["description"].as_str().unwrap_or("").to_owned(),
                    image: crate::characters::decode_image(&folder.join(file)).ok(),
                    texture: None,
                }
            })
            .collect::<Vec<_>>();
        sets.sort_by(|a, b| a.set.name.cmp(&b.set.name));
        Self {
            sets,
            selected: None,
            loading: None,
            load_failed: false,
        }
    }

    pub fn choices(&self) -> Vec<SceneSet> {
        self.sets
            .iter()
            .map(|reference| reference.set.clone())
            .collect()
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.loading else {
            return;
        };
        match receiver.try_recv() {
            Ok(gallery) => {
                *self = gallery;
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(TryRecvError::Disconnected) => {
                self.loading = None;
                self.load_failed = true;
                ctx.request_repaint();
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sets");
        if self.load_failed {
            ui.label("Could not load sets.");
            return;
        }
        if self.loading.is_some() {
            ui.label("Loading sets…");
            return;
        }
        if self.sets.is_empty() {
            ui.label("No sets found.");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("set_gallery")
            .show(ui, |ui| {
                for (index, reference) in self.sets.iter_mut().enumerate() {
                    ui.add_space(6.0);
                    let selected = self.selected == Some(index);
                    let hint = format!(
                        "{}\nDrag onto a scene heading to assign this set.",
                        reference.description
                    );
                    let response = ui
                        .add(
                            egui::Button::selectable(selected, &reference.set.name)
                                .wrap()
                                .sense(egui::Sense::click_and_drag()),
                        )
                        .on_hover_text(&hint);
                    response.dnd_set_drag_payload(reference.set.clone());
                    if response.clicked() {
                        self.selected = Some(index);
                    }
                    if reference.texture.is_none()
                        && let Some(image) = reference.image.take()
                    {
                        reference.texture = Some(ui.ctx().load_texture(
                            format!("set-{}", reference.set.id),
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    if let Some(texture) = &reference.texture {
                        let size = texture.size_vec2();
                        let width = (ui.available_width() - 2.0 * ui.spacing().button_padding.x)
                            .clamp(1.0, 136.0);
                        let scale = (width / size.x).min(136.0 / size.y).min(1.0);
                        let response = ui
                            .add(
                                egui::Button::new(
                                    egui::Image::new((texture.id(), size * scale))
                                        .alt_text(format!("Select {}", reference.set.name)),
                                )
                                .selected(selected)
                                .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text(&hint);
                        response.dnd_set_drag_payload(reference.set.clone());
                        if response.clicked() {
                            self.selected = Some(index);
                        }
                    } else {
                        ui.add(egui::Label::new("Reference image unavailable.").wrap());
                    }
                }
            });
        if let Some(set) = egui::DragAndDrop::payload::<SceneSet>(ui.ctx()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(pos) = ui.ctx().pointer_interact_pos() {
                egui::Area::new(egui::Id::new("dragged_set"))
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .fixed_pos(pos + egui::vec2(16.0, 16.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(&set.name);
                        });
                    });
            }
        }
    }
}
