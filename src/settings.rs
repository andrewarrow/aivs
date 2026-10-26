use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderFormat {
    #[default]
    Landscape,
    Portrait,
}

impl RenderFormat {
    pub fn id(self) -> &'static str {
        match self {
            Self::Landscape => "landscape",
            Self::Portrait => "portrait",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Landscape => "1280 × 720 · 16:9",
            Self::Portrait => "1080 × 1920 · 9:16",
        }
    }

    pub fn load(root: &Path) -> Self {
        let path = root.join("settings.json");
        let result = std::fs::read(&path).and_then(|bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            match value["render_format"].as_str() {
                Some("landscape") => Ok(Self::Landscape),
                Some("portrait") => Ok(Self::Portrait),
                _ => Err(std::io::Error::other("Unknown render format")),
            }
        });
        match result {
            Ok(format) => format,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Cannot load {}: {error}", path.display());
                }
                Self::default()
            }
        }
    }

    pub fn save(self, root: &Path) -> Result<(), String> {
        let temporary = root.join(".settings.json.tmp");
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({"render_format": self.id()}))
            .map_err(|error| error.to_string())?;
        std::fs::write(&temporary, bytes)
            .and_then(|()| std::fs::rename(&temporary, root.join("settings.json")))
            .map_err(|error| format!("Could not save settings: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_format_defaults_to_landscape_and_persists_both_options() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(RenderFormat::load(root.path()), RenderFormat::Landscape);
        for format in [RenderFormat::Portrait, RenderFormat::Landscape] {
            format.save(root.path()).unwrap();
            assert_eq!(RenderFormat::load(root.path()), format);
        }
        std::fs::write(root.path().join("settings.json"), "{}").unwrap();
        assert_eq!(RenderFormat::load(root.path()), RenderFormat::Landscape);
    }
}
