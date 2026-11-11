//! Profile discovery and lazy loading.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use serde_json::Value;
use walkdir::WalkDir;

use crate::{
    error::{Error, Result},
    profiles::{
        definition::{find_definition, flatten_settings, load_definition},
        instance::load_instance,
        material::load_material,
        types::{
            Definition, InstanceProfile, MaterialProfile, ProfileCatalog, ProfileEntry, ProfileKind,
        },
    },
    workspace::sha256_hex,
};

const MAX_SCAN_DEPTH: usize = 12;

/// A profile index that scans directories on demand and caches the catalog.
#[derive(Debug)]
pub struct ProfileIndex {
    dirs: Vec<PathBuf>,
    custom_dirs: Vec<PathBuf>,
    max_profiles: usize,
    catalog: RwLock<Option<Arc<ProfileCatalog>>>,
}

impl ProfileIndex {
    /// Build an index over the configured directories.
    #[must_use]
    pub fn new(dirs: Vec<PathBuf>, custom_dirs: Vec<PathBuf>, max_profiles: usize) -> Self {
        Self {
            dirs,
            custom_dirs,
            max_profiles,
            catalog: RwLock::new(None),
        }
    }

    /// All scanned directories.
    #[must_use]
    pub fn dirs(&self) -> Vec<&Path> {
        self.dirs
            .iter()
            .chain(self.custom_dirs.iter())
            .map(PathBuf::as_path)
            .collect()
    }

    /// Return the cached catalog, scanning on first use.
    pub fn catalog(&self) -> Result<Arc<ProfileCatalog>> {
        if let Some(catalog) = self
            .catalog
            .read()
            .map_err(|_| Error::Profile("profile index lock poisoned".into()))?
            .as_ref()
        {
            return Ok(Arc::clone(catalog));
        }
        self.refresh()
    }

    /// Force a rescan.
    pub fn refresh(&self) -> Result<Arc<ProfileCatalog>> {
        let mut catalog = ProfileCatalog::default();
        let mut seen_paths = std::collections::HashSet::new();

        for dir in self.dirs.iter().chain(self.custom_dirs.iter()) {
            if !dir.is_dir() {
                continue;
            }
            catalog.scanned_dirs.push(dir.to_string_lossy().to_string());
            for entry in WalkDir::new(dir)
                .max_depth(MAX_SCAN_DEPTH)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
                })
            {
                if catalog.entries.len() >= self.max_profiles {
                    catalog.warnings.push(format!(
                        "profile limit of {} reached; remaining files were skipped",
                        self.max_profiles
                    ));
                    break;
                }
                let Ok(entry) = entry else {
                    continue;
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if !seen_paths.insert(path.to_path_buf()) {
                    continue;
                }
                if let Some(kind) = classify(path) {
                    match build_entry(path, kind) {
                        Ok(entry) => {
                            if kind == ProfileKind::Definition {
                                if let Some(definition) = &entry.definition {
                                    catalog
                                        .definitions
                                        .insert(definition.clone(), entry.path.clone());
                                }
                                catalog
                                    .definitions
                                    .insert(entry.name.clone(), entry.path.clone());
                            }
                            catalog.entries.push(entry);
                        }
                        Err(err) => catalog.warnings.push(format!(
                            "skipped {}: {}",
                            path.display(),
                            err.public_message()
                        )),
                    }
                }
            }
        }

        catalog.entries.sort_by(|left, right| {
            left.kind.as_str().cmp(right.kind.as_str()).then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
        });

        let catalog = Arc::new(catalog);
        *self
            .catalog
            .write()
            .map_err(|_| Error::Profile("profile index lock poisoned".into()))? =
            Some(Arc::clone(&catalog));
        Ok(catalog)
    }

    /// Load a definition by catalog id, name, or path.
    pub fn definition(&self, id_or_name: &str) -> Result<Definition> {
        let catalog = self.catalog()?;
        let path = self.resolve_path(&catalog, id_or_name, ProfileKind::Definition)?;
        let search = self
            .dirs
            .iter()
            .chain(self.custom_dirs.iter())
            .cloned()
            .collect::<Vec<_>>();
        load_definition(&path, &search)
    }

    /// Load an instance profile by id or path.
    pub fn instance(&self, id_or_name: &str) -> Result<InstanceProfile> {
        let catalog = self.catalog()?;
        let path = self.resolve_path(&catalog, id_or_name, ProfileKind::Custom)?;
        load_instance(&path)
    }

    /// Load a material by id or path.
    pub fn material(&self, id_or_name: &str) -> Result<MaterialProfile> {
        let catalog = self.catalog()?;
        let path = self.resolve_path(&catalog, id_or_name, ProfileKind::Material)?;
        load_material(&path)
    }

    /// Load any `.inst.cfg` profile by id, name, or path, regardless of whether
    /// it is a quality, material, intent, variant, user, or custom profile.
    pub fn instance_any(&self, id_or_name: &str) -> Result<InstanceProfile> {
        let catalog = self.catalog()?;
        if let Some(entry) = catalog.get(id_or_name) {
            return load_instance(Path::new(&entry.path));
        }
        let direct = PathBuf::from(id_or_name);
        if direct.is_file() {
            return load_instance(&direct);
        }
        if let Some(entry) = catalog
            .entries
            .iter()
            .find(|entry| {
                matches!(
                    entry.kind,
                    ProfileKind::Quality
                        | ProfileKind::QualityChanges
                        | ProfileKind::Material
                        | ProfileKind::Variant
                        | ProfileKind::Intent
                        | ProfileKind::Extruder
                        | ProfileKind::User
                        | ProfileKind::Custom
                        | ProfileKind::Unknown
                ) && entry.name.eq_ignore_ascii_case(id_or_name)
            })
            .or_else(|| {
                catalog
                    .entries
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(id_or_name))
            })
        {
            return load_instance(Path::new(&entry.path));
        }
        Err(Error::Profile(format!(
            "profile '{id_or_name}' was not found; call cura_profile_refresh to rescan"
        )))
    }

    /// Path of any discovered profile entry by id.
    pub fn entry_path(&self, id: &str) -> Result<PathBuf> {
        let catalog = self.catalog()?;
        catalog
            .get(id)
            .map(|entry| PathBuf::from(&entry.path))
            .ok_or_else(|| Error::Profile(format!("profile '{id}' was not found")))
    }

    /// Find a discovered entry, preferring an exact id match then a name match.
    #[must_use]
    pub fn find_entry<'a>(
        catalog: &'a ProfileCatalog,
        id_or_name: &str,
    ) -> Option<&'a ProfileEntry> {
        catalog.get(id_or_name).or_else(|| {
            catalog
                .entries
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(id_or_name))
        })
    }

    fn resolve_path(
        &self,
        catalog: &ProfileCatalog,
        id_or_name: &str,
        preferred: ProfileKind,
    ) -> Result<PathBuf> {
        if let Some(entry) = catalog.get(id_or_name) {
            return Ok(PathBuf::from(&entry.path));
        }
        let direct = PathBuf::from(id_or_name);
        if direct.is_file() {
            return Ok(direct);
        }
        if let Some(path) = catalog
            .entries
            .iter()
            .find(|entry| entry.kind == preferred && entry.name.eq_ignore_ascii_case(id_or_name))
            .or_else(|| {
                catalog
                    .entries
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(id_or_name))
            })
        {
            return Ok(PathBuf::from(&path.path));
        }
        if let Some(path) = catalog.definitions.get(id_or_name) {
            return Ok(PathBuf::from(path));
        }
        for dir in self.dirs.iter().chain(self.custom_dirs.iter()) {
            if let Some(path) = find_definition(id_or_name, dir, std::slice::from_ref(dir)) {
                return Ok(path);
            }
        }
        Err(Error::Profile(format!(
            "profile '{id_or_name}' was not found; call cura_profile_refresh to rescan or use a profile id"
        )))
    }
}

fn classify(path: &Path) -> Option<ProfileKind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".def.json") {
        return Some(ProfileKind::Definition);
    }
    if name.ends_with(".inst.cfg") {
        return Some(ProfileKind::Custom);
    }
    if name.ends_with(".xml.fdm_material") || name.ends_with(".fdm_material") {
        return Some(ProfileKind::Material);
    }
    None
}

fn build_entry(path: &Path, kind: ProfileKind) -> Result<ProfileEntry> {
    let path_string = path.to_string_lossy().to_string();
    let id = format!("prf_{}", &sha256_hex(path_string.as_bytes())[..24]);
    match kind {
        ProfileKind::Definition => {
            let text = std::fs::read_to_string(path)?;
            let root: Value = serde_json::from_str(&text)
                .map_err(|err| Error::Profile(format!("invalid definition JSON: {err}")))?;
            let name = root
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("definition")
                })
                .to_string();
            let setting_version = root
                .get("metadata")
                .and_then(|metadata| metadata.get("setting_version"))
                .map(Value::to_string);
            Ok(ProfileEntry {
                id,
                kind,
                name: name.clone(),
                path: path_string,
                definition: Some(name),
                quality_type: None,
                material: None,
                variant: None,
                intent: None,
                setting_version,
                value_count: flatten_settings(&root).len(),
            })
        }
        ProfileKind::Material => {
            let material = load_material(path)?;
            Ok(ProfileEntry {
                id,
                kind: ProfileKind::Material,
                name: material.name,
                path: path_string,
                definition: None,
                quality_type: None,
                material: material.material_type,
                variant: None,
                intent: None,
                setting_version: None,
                value_count: material.settings.len(),
            })
        }
        _ => {
            let instance = load_instance(path)?;
            let kind = instance.kind;
            Ok(ProfileEntry {
                id,
                kind,
                name: instance.name,
                path: path_string,
                definition: instance.definition,
                quality_type: instance.metadata.get("quality_type").cloned(),
                material: instance.metadata.get("material").cloned(),
                variant: instance.metadata.get("variant").cloned(),
                intent: instance.metadata.get("intent").cloned(),
                setting_version: instance.metadata.get("setting_version").cloned(),
                value_count: instance.values.len(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_finds_profiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("base.def.json"),
            serde_json::json!({
                "name": "base",
                "settings": {"a": {"type": "category", "children": {
                    "machine_width": {"type": "float", "default_value": 100}
                }}}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("fine.inst.cfg"),
            "[general]\ndefinition = base\nname = Fine\n\n[metadata]\ntype = quality\nquality_type = fine\n\n[values]\nlayer_height = 0.1\n",
        )
        .unwrap();
        let index = ProfileIndex::new(vec![dir.path().to_path_buf()], Vec::new(), 100);
        let catalog = index.catalog().unwrap();
        assert_eq!(catalog.entries.len(), 2);
        assert!(catalog.definitions.contains_key("base"));
        let definition = index.definition("base").unwrap();
        assert_eq!(
            definition.setting("machine_width").unwrap().default_value,
            Some(serde_json::json!(100))
        );
    }
}
