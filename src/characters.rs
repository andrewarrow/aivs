use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct CharacterGallery {
    characters: Vec<Character>,
}

struct Character {
    name: String,
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
                let name = character_name(&folder).unwrap_or_else(|| {
                    folder
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Character")
                        .to_owned()
                });
                let image = character_image_path(&folder).and_then(|path| decode_image(&path).ok());
                Character {
                    name,
                    image,
                    texture: None,
                }
            })
            .collect::<Vec<_>>();
        characters.sort_by(|a, b| a.name.cmp(&b.name));
        Self { characters }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Characters");
        if self.characters.is_empty() {
            ui.label("No characters found.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for character in &mut self.characters {
                ui.add_space(6.0);
                ui.add(egui::Label::new(&character.name).wrap());
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
                    let scale = (ui.available_width() / size.x).min(260.0 / size.y).min(1.0);
                    ui.add(
                        egui::Image::new((texture.id(), size * scale))
                            .alt_text(format!("{} reference image", character.name)),
                    );
                } else {
                    ui.add(egui::Label::new("Reference image unavailable.").wrap());
                }
            }
        });
    }
}

fn character_name(folder: &Path) -> Option<String> {
    let data: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.join("character.json")).ok()?).ok()?;
    data.get("name")?.as_str().map(str::to_owned)
}

fn character_image_path(folder: &Path) -> Option<PathBuf> {
    let data: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.join("character.json")).ok()?).ok()?;
    let image_file = data
        .get("image")
        .and_then(|image| image.get("file"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("reference.png");
    Some(folder.join(image_file))
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
                "Ava", "Eli", "Iris", "Joe", "Lena", "Marcus", "Maya", "Nora", "Owen", "Sue",
                "Theo"
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
