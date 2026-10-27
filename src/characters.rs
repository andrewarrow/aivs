use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

pub struct CharacterGallery {
    characters: Vec<Character>,
    selected: Option<usize>,
    loading: Option<Receiver<Self>>,
    load_failed: bool,
    deferred_root: Option<PathBuf>,
}

pub struct Character {
    pub name: String,
    id: String,
    image: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CastCharacter {
    pub id: String,
    pub name: String,
}

impl Character {
    fn cast_character(&self) -> CastCharacter {
        CastCharacter {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
}

impl CharacterGallery {
    pub fn new(root: &Path) -> Self {
        Self {
            characters: Vec::new(),
            selected: None,
            loading: None,
            load_failed: false,
            deferred_root: Some(root.to_owned()),
        }
    }

    pub fn load_if_needed(&mut self) {
        if let Some(root) = self.deferred_root.take() {
            *self = Self::load_in_background(&root);
        }
    }

    pub fn load_in_background(root: &Path) -> Self {
        let root = root.to_owned();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut startup = crate::startup::Timer::new();
            let gallery = Self::load(&root);
            startup.mark("background character gallery loaded");
            let _ = sender.send(gallery);
        });
        Self {
            characters: Vec::new(),
            selected: None,
            loading: Some(receiver),
            load_failed: false,
            deferred_root: None,
        }
    }

    pub fn load(root: &Path) -> Self {
        let directory = if root.join("characters").is_dir() {
            root.join("characters")
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("characters")
        };

        let mut characters = fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| {
                let folder = entry.path();
                let data = fs::read(folder.join("character.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .unwrap_or_default();
                let name = data["name"].as_str().map(str::to_owned).unwrap_or_else(|| {
                    folder
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Character")
                        .to_owned()
                });
                let image_file = data["image"]["file"].as_str().unwrap_or("reference.png");
                let image = decode_image(&folder.join(image_file)).ok();
                Character {
                    id: data["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| name.to_lowercase()),
                    name,
                    image,
                    texture: None,
                }
            })
            .collect::<Vec<_>>();
        characters.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            characters,
            selected: None,
            loading: None,
            load_failed: false,
            deferred_root: None,
        }
    }

    pub fn cast_choices(&self) -> Vec<CastCharacter> {
        self.characters
            .iter()
            .map(Character::cast_character)
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

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<&Character> {
        ui.heading("Characters");
        if self.load_failed {
            ui.label("Could not load characters.");
            return None;
        }
        if self.loading.is_some() {
            ui.label("Loading characters…");
            return None;
        }
        if self.characters.is_empty() {
            ui.label("No characters found.");
            return None;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, character) in self.characters.iter_mut().enumerate() {
                ui.add_space(6.0);
                let selected = self.selected == Some(index);
                let payload = character.cast_character();
                let response = ui
                    .add(
                        egui::Button::selectable(selected, &character.name)
                            .wrap()
                            .sense(egui::Sense::click_and_drag()),
                    )
                    .on_hover_text("Drag onto a screenplay role to cast this character.");
                response.dnd_set_drag_payload(payload.clone());
                if response.clicked() {
                    clicked = Some(index);
                }
                if character.texture.is_none()
                    && let Some(image) = character.image.take()
                {
                    character.texture = Some(ui.ctx().load_texture(
                        format!("character-{}", character.name),
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                if let Some(texture) = &character.texture {
                    let size = texture.size_vec2();
                    let width = (ui.available_width() - 2.0 * ui.spacing().button_padding.x)
                        .clamp(1.0, 136.0);
                    let scale = (width / size.x).min(136.0 / size.y).min(1.0);
                    let response = ui
                        .add(
                            egui::Button::new(
                                egui::Image::new((texture.id(), size * scale))
                                    .alt_text(format!("Select {}", character.name)),
                            )
                            .selected(selected)
                            .sense(egui::Sense::click_and_drag()),
                        )
                        .on_hover_text("Drag onto a screenplay role to cast this character.");
                    response.dnd_set_drag_payload(payload);
                    if response.clicked() {
                        clicked = Some(index);
                    }
                } else {
                    ui.add(egui::Label::new("Reference image unavailable.").wrap());
                }
            }
        });
        if let Some(character) = egui::DragAndDrop::payload::<CastCharacter>(ui.ctx()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(pos) = ui.ctx().pointer_interact_pos() {
                egui::Area::new(egui::Id::new("dragged_character"))
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .fixed_pos(pos + egui::vec2(16.0, 16.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(&character.name);
                        });
                    });
            }
        }
        if let Some(index) = clicked {
            self.selected = Some(index);
            Some(&self.characters[index])
        } else {
            None
        }
    }
}

pub(crate) fn decode_image(path: &Path) -> Result<egui::ColorImage, String> {
    let image = image::ImageReader::open(path)
        .map_err(|_| "could not open image")?
        .decode()
        .map_err(|_| "could not decode image")?
        .thumbnail(768, 768)
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        image.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_characters_have_decodable_reference_images_in_name_order() {
        let gallery = CharacterGallery::load(Path::new("/workspace-without-characters"));
        let names = gallery
            .characters
            .iter()
            .map(|character| character.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "Ava", "Eli", "Iris", "Lena", "Marcus", "Maya", "Nora", "Owen", "Theo"
            ]
        );
        assert!(
            gallery
                .characters
                .iter()
                .all(|character| character.image.is_some())
        );
    }

    #[test]
    fn missing_or_invalid_reference_images_remain_unavailable_without_panicking() {
        let temp = tempfile::tempdir().unwrap();
        let characters = temp.path().join("characters");
        let folder = characters.join("example");
        let broken_folder = characters.join("broken");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&broken_folder).unwrap();
        fs::write(
            folder.join("character.json"),
            r#"{"name":"Example","image":{"file":"missing.png"}}"#,
        )
        .unwrap();
        fs::write(
            broken_folder.join("character.json"),
            r#"{"name":"Broken","image":{"file":"reference.png"}}"#,
        )
        .unwrap();
        fs::write(broken_folder.join("reference.png"), b"not a PNG").unwrap();

        let gallery = CharacterGallery::load(temp.path());
        assert_eq!(gallery.characters.len(), 2);
        assert_eq!(gallery.characters[0].name, "Broken");
        assert_eq!(gallery.characters[1].name, "Example");
        assert!(
            gallery
                .characters
                .iter()
                .all(|character| character.image.is_none())
        );
    }
}
