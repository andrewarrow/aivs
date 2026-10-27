use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

pub struct Project {
    root: PathBuf,
    temporary: Option<tempfile::TempDir>,
}

impl Project {
    #[cfg(test)]
    pub(crate) fn for_test(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            temporary: None,
        }
    }

    pub fn untitled(workspace: &Path, import_legacy: bool) -> Result<Self, String> {
        let temporary = tempfile::Builder::new()
            .prefix("aivs-untitled-")
            .tempdir()
            .map_err(|e| format!("Could not create an untitled project: {e}"))?;
        let project = Self {
            root: temporary.path().canonicalize().map_err(|e| e.to_string())?,
            temporary: Some(temporary),
        };
        // Give each project its own editable library, independent of the checkout.
        for name in ["characters", "sets"] {
            let source = if workspace.join(name).is_dir() {
                workspace.join(name)
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name)
            };
            if source.is_dir() {
                copy_directory(&source, &project.root.join(name))?;
            }
        }
        for name in ["settings.json", "screenplay.json"] {
            if (name != "screenplay.json" || import_legacy) && workspace.join(name).is_file() {
                fs::copy(workspace.join(name), project.root.join(name))
                    .map_err(|e| e.to_string())?;
            }
        }
        for name in ["references", "runs"] {
            fs::create_dir_all(project.root.join(name)).map_err(|e| e.to_string())?;
        }
        if !project.root.join("screenplay.json").exists() {
            crate::screenplay::Screenplay::untitled(project.root()).flush()?;
        }
        project.save()?;
        Ok(project)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let root = if path.is_file() {
            if path.file_name().is_none_or(|name| name != "project.json") {
                return Err("Choose a project directory or its project.json file.".into());
            }
            path.parent().ok_or("Missing project directory.")?
        } else {
            path
        }
        .canonicalize()
        .map_err(|e| format!("Could not open project: {e}"))?;
        let manifest = aivs_core::artifacts::read_json(&root.join("project.json"))?;
        if manifest["kind"] != "aivs-project" || manifest["version"] != 1 {
            return Err("This is not a supported AIVS project.".into());
        }
        if !root.join("screenplay.json").is_file() {
            return Err("The project's screenplay.json is missing.".into());
        }
        Ok(Self {
            root,
            temporary: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_untitled(&self) -> bool {
        self.temporary.is_some()
    }

    pub fn name(&self) -> String {
        if self.is_untitled() {
            "Untitled".into()
        } else {
            self.root
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }
    }

    fn manifest(&self) -> Value {
        json!({"kind":"aivs-project", "version":1, "name":self.name(),
            "screenplay":"screenplay.json", "characters":"characters", "sets":"sets",
            "references":"references", "runs":"runs", "settings":"settings.json",
            "preproduction":"preproduction"})
    }

    pub fn save(&self) -> Result<(), String> {
        aivs_core::artifacts::atomic_json(&self.root.join("project.json"), &self.manifest())
    }

    pub fn save_as(&mut self, destination: &Path, screenplay: &Value) -> Result<(), String> {
        let parent = destination
            .parent()
            .ok_or("Choose a project location.")?
            .canonicalize()
            .map_err(|e| format!("Could not open the save location: {e}"))?;
        let destination = parent.join(destination.file_name().ok_or("Enter a project name.")?);
        if destination.exists() {
            return Err(
                "A directory or file already exists with that name. Choose a new project name."
                    .into(),
            );
        }
        let source = self.root.canonicalize().map_err(|e| e.to_string())?;
        if parent.starts_with(&source) {
            return Err("Save the project outside its current directory.".into());
        }
        // Stage the whole project first. Failed copies leave the original intact.
        let staging = tempfile::Builder::new()
            .prefix(".aivs-save-")
            .tempdir_in(&parent)
            .map_err(|e| format!("Could not prepare project save: {e}"))?;
        copy_directory(&source, staging.path())?;
        aivs_core::artifacts::atomic_json(&staging.path().join("screenplay.json"), screenplay)?;
        let mut manifest = self.manifest();
        manifest["name"] = json!(
            destination
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
        );
        aivs_core::artifacts::atomic_json(&staging.path().join("project.json"), &manifest)?;
        fs::rename(staging.path(), &destination)
            .map_err(|e| format!("Could not save project: {e}"))?;
        self.root = destination;
        self.temporary = None;
        Ok(())
    }
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(format!(
                "Project files must be stored inside the project: {}",
                entry.path().display()
            ));
        } else if kind.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)
                .map_err(|e| format!("Could not copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn documents_directory() -> PathBuf {
    use objc2_foundation::{
        NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains,
    };
    NSSearchPathForDirectoriesInDomains(
        NSSearchPathDirectory::DocumentDirectory,
        NSSearchPathDomainMask::UserDomainMask,
        true,
    )
    .firstObject()
    .map(|path| PathBuf::from(path.to_string()))
    .unwrap_or_else(std::env::temp_dir)
}

#[cfg(not(target_os = "macos"))]
pub fn documents_directory() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join("Documents"))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(target_os = "macos")]
pub fn choose_save(project: &Project) -> Result<Option<PathBuf>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSSavePanel};
    use objc2_foundation::{NSString, NSURL};
    let panel =
        NSSavePanel::savePanel(MainThreadMarker::new().ok_or("Save requires the main thread.")?);
    panel.setTitle(Some(&NSString::from_str("Save project")));
    panel.setCanCreateDirectories(true);
    panel.setShowsTagField(false);
    panel.setNameFieldStringValue(&NSString::from_str(&format!("{}.aivs", project.name())));
    let directory = if project.is_untitled() {
        documents_directory()
    } else {
        project.root.parent().unwrap_or(project.root()).to_owned()
    };
    if let Some(url) = NSURL::from_file_path(&directory) {
        panel.setDirectoryURL(Some(&url));
    }
    match panel.runModal() {
        response if response == NSModalResponseOK => panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or_else(|| "Could not read the selected project location.".into()),
        response if response == NSModalResponseCancel => Ok(None),
        _ => Err("Could not open the save dialog.".into()),
    }
}

#[cfg(target_os = "macos")]
pub fn choose_open() -> Result<Option<PathBuf>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSString, NSURL};
    let panel =
        NSOpenPanel::openPanel(MainThreadMarker::new().ok_or("Open requires the main thread.")?);
    panel.setTitle(Some(&NSString::from_str("Open project")));
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    if let Some(url) = NSURL::from_file_path(documents_directory()) {
        panel.setDirectoryURL(Some(&url));
    }
    match panel.runModal() {
        response if response == NSModalResponseOK => panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or_else(|| "Could not read the selected project.".into()),
        response if response == NSModalResponseCancel => Ok(None),
        _ => Err("Could not open the project chooser.".into()),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn choose_save(_project: &Project) -> Result<Option<PathBuf>, String> {
    Err("The project save dialog requires macOS.".into())
}

#[cfg(not(target_os = "macos"))]
pub fn choose_open() -> Result<Option<PathBuf>, String> {
    Err("The project chooser requires macOS.".into())
}
