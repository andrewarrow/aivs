use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct CharacterGallery {
    characters: Vec<Character>,
    selected: Option<usize>,
}

pub struct Character {
    pub name: String,
    image: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

impl CharacterGallery {
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
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<&Character> {
        ui.heading("Characters");
        if self.characters.is_empty() {
            ui.label("No characters found.");
            return None;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, character) in self.characters.iter_mut().enumerate() {
                ui.add_space(6.0);
                let selected = self.selected == Some(index);
                if ui
                    .add(egui::Button::selectable(selected, &character.name).wrap())
                    .clicked()
                {
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
                    if ui
                        .add(
                            egui::Button::new(
                                egui::Image::new((texture.id(), size * scale))
                                    .alt_text(format!("Select {}", character.name)),
                            )
                            .selected(selected),
                        )
                        .clicked()
                    {
                        clicked = Some(index);
                    }
                } else {
                    ui.add(egui::Label::new("Reference image unavailable.").wrap());
                }
            }
        });
        if let Some(index) = clicked {
            self.selected = Some(index);
            Some(&self.characters[index])
        } else {
            None
        }
    }
}

fn decode_image(path: &Path) -> Result<egui::ColorImage, String> {
    let image = image::ImageReader::open(path)
        .map_err(|_| "could not open image")?
        .decode()
        .map_err(|_| "could not decode image")?
        .to_rgba8();
    let image = image::imageops::thumbnail(&image, 768, 768);
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
