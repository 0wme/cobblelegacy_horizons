//! Parsed resource models for one startup or resource-pack reload.
use crate::Result;
use horizon_assets::{
    model::CuboidModel,
    resolve::{AssetReader, ModelLibrary},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
pub struct ParsedModels {
    models: BTreeMap<String, Arc<CuboidModel>>,
    root: PathBuf,
}
/// Block model templates vanilla renamed after the packs for older versions were made, each
/// as a model whose parent is the template in its place and whose texture slots are the old
/// ones. A pack's own models still name the old template as their parent, and each of them drew
/// the missing model's cube: after 1.21.4 `template_farmland` became the wider
/// `template_cube_bottom_top_indented`.
const RENAMED_TEMPLATES: [(&str, &str); 1] = [(
    "minecraft:block/template_farmland",
    r##"{"parent":"minecraft:block/template_cube_bottom_top_indented","textures":{"bottom":"#dirt","side":"#dirt"}}"##,
)];

/// The model standing for the renamed template `name` ([`RENAMED_TEMPLATES`]).
fn renamed_template(name: &str) -> Option<Arc<CuboidModel>> {
    let (_, json) = RENAMED_TEMPLATES.iter().find(|(old, _)| *old == name)?;
    CuboidModel::from_json(json).ok().map(Arc::new)
}

/// Asset files under a content root, for models that name a file (obj).
struct RootAssets(PathBuf);
impl AssetReader for RootAssets {
    fn read_text(&self, location: &str) -> Option<String> {
        let (namespace, path) = location.split_once(':')?;
        // Only a resource location's characters, and no `.`/`..` step: an
        // .obj's `mtllib` names its .mtl as raw text, so it must not reach a
        // file outside the content root (`../..:x` as a namespace would).
        let valid = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c);
        if !namespace.bytes().all(valid)
            || matches!(namespace, "" | "." | "..")
            || !path.bytes().all(|c| valid(c) || c == b'/')
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return None;
        }
        horizon_core::prepared_files::read_to_string(
            &self.0.join(format!("assets/{namespace}/{path}")),
        )
        .ok()
    }
}
impl ParsedModels {
    /// Every `models/**.json` of a namespace directory, by identifier.
    pub fn load(root: &Path) -> Self {
        let Ok(namespaces) = fs::read_dir(root.join("assets")) else {
            return Self {
                root: root.to_owned(),
                ..Self::default()
            };
        };
        // Listed first, then read and parsed at once: a content pack has tens
        // of thousands of models.
        let mut files = Vec::new();
        for ns in namespaces.flatten() {
            let namespace = ns.file_name().to_string_lossy().into_owned();
            let base = ns.path().join("models");
            let mut stack = vec![base.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let path = e.path();
                    // The entry's own type saves a stat per file; a link
                    // (content roots link into the base) is followed.
                    let is_dir = match e.file_type() {
                        Ok(t) if !t.is_symlink() => t.is_dir(),
                        _ => path.is_dir(),
                    };
                    if is_dir {
                        stack.push(path);
                    } else if path.extension().is_some_and(|x| x == "json")
                        && let Ok(rel) = path.strip_prefix(&base)
                    {
                        let rel = rel.with_extension("").to_string_lossy().replace('\\', "/");
                        files.push((format!("{namespace}:{rel}"), path));
                    }
                }
            }
        }
        let parsed = crate::par::par_map(&files, |(_, path)| {
            let text = horizon_core::prepared_files::read_to_string(path).ok()?;
            CuboidModel::from_json(&text).ok().map(Arc::new)
        });
        let mut models: BTreeMap<String, Arc<CuboidModel>> = files
            .into_iter()
            .zip(parsed)
            .filter_map(|((id, _), model)| Some((id, model?)))
            .collect();
        // A template the root no longer has, for the packs whose models still name it.
        for (name, _) in RENAMED_TEMPLATES {
            if !models.contains_key(name)
                && let Some(model) = renamed_template(name)
            {
                models.insert(name.to_owned(), model);
            }
        }
        Self {
            models,
            root: root.to_owned(),
        }
    }

    /// Blocks require every referenced resource, including parents. Items instead
    /// skip broken resources and use the resolver's missing-model fallback.
    pub fn block_library(&self, root: &Path, pending: Vec<String>) -> Result<ModelLibrary> {
        self.block_models(root, pending, false)
    }
    /// [`Self::block_library`], but a model that does not read takes the
    /// built-in missing model, logged, as vanilla's model discovery does
    /// ("Missing block model"). Mods' content gets this; vanilla's resources
    /// always read.
    pub fn block_library_or_missing(
        &self,
        root: &Path,
        pending: Vec<String>,
    ) -> Result<ModelLibrary> {
        self.block_models(root, pending, true)
    }
    fn block_models(
        &self,
        root: &Path,
        mut pending: Vec<String>,
        or_missing: bool,
    ) -> Result<ModelLibrary> {
        let mut models = BTreeMap::new();
        let mut skipped = std::collections::BTreeSet::new();
        while let Some(name) = pending.pop() {
            if models.contains_key(&name) || skipped.contains(&name) {
                continue;
            }
            let model = match self.models.get(&name) {
                Some(model) => Arc::clone(model),
                None => {
                    let read = || -> Result<CuboidModel> {
                        let (namespace, path) =
                            name.split_once(':').ok_or("invalid model identifier")?;
                        let text = horizon_core::prepared_files::read_to_string(
                            &root.join(format!("assets/{namespace}/models/{path}.json")),
                        )?;
                        Ok(CuboidModel::from_json(&text)?)
                    };
                    match (read(), renamed_template(&name)) {
                        (Ok(model), _) => Arc::new(model),
                        (Err(_), Some(model)) => model,
                        (Err(e), None) if or_missing => {
                            log::warn!("Horizon: missing block model {name}: {e}");
                            skipped.insert(name);
                            continue;
                        }
                        (Err(e), None) => return Err(e),
                    }
                }
            };
            pending.extend(model.referenced_models());
            models.insert(name, model);
        }
        Ok(ModelLibrary::from_shared(models)?.with_assets(Arc::new(RootAssets(root.to_owned()))))
    }
    pub fn into_item_library(self) -> std::result::Result<Arc<ModelLibrary>, String> {
        let root = self.root;
        ModelLibrary::with_shared_item_generator(self.models)
            .map(|library| Arc::new(library.with_assets(Arc::new(RootAssets(root)))))
            .map_err(|e| format!("item models: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_assets::resolve::{GENERATED_MODEL, MISSING_MODEL};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn write(&self, name: &str, json: &str) {
            let (namespace, path) = name.split_once(':').unwrap();
            let path = self
                .0
                .join(format!("assets/{namespace}/models/{path}.json"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, json).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A model made for 1.21 whose parent is a template vanilla has renamed since draws as it
    /// did, through the template in its place.
    #[test]
    fn a_renamed_template_still_parents_a_packs_models() -> Result<()> {
        let fixture = Fixture(
            std::env::temp_dir().join(format!("horizon-renamed-models-{}", std::process::id())),
        );
        fixture.write(
            "minecraft:block/template_cube_bottom_top_indented",
            r##"{"textures":{"particle":"#side"},"elements":[{"from":[0,0,0],"to":[16,15,16],"faces":{"down":{"texture":"#bottom"},"up":{"texture":"#top"},"north":{"texture":"#side"}}}]}"##,
        );
        fixture.write(
            "minecraft:block/farmland1",
            r#"{"parent":"minecraft:block/template_farmland","textures":{"dirt":"minecraft:block/dirt2","top":"minecraft:block/farmland"}}"#,
        );
        let name = "minecraft:block/farmland1";
        // Parsed ahead, and read as the models are asked for.
        for models in [ParsedModels::load(&fixture.0), ParsedModels::default()] {
            let library = models.block_library(&fixture.0, vec![name.into()])?;
            let farmland = library.resolve(name)?;
            assert_eq!(farmland.elements().len(), 1);
            for (slot, sprite) in [
                ("side", "minecraft:block/dirt2"),
                ("bottom", "minecraft:block/dirt2"),
                ("top", "minecraft:block/farmland"),
                ("particle", "minecraft:block/dirt2"),
            ] {
                assert_eq!(farmland.material(slot)?.unwrap().sprite, sprite, "{slot}");
            }
        }
        // An item model over it resolves too.
        fixture.write(
            "minecraft:item/farmland",
            r#"{"parent":"minecraft:block/farmland1"}"#,
        );
        let items = ParsedModels::load(&fixture.0).into_item_library()?;
        assert_eq!(
            items.resolve("minecraft:item/farmland")?.elements().len(),
            1
        );
        // A root that has the template keeps its own.
        fixture.write(
            "minecraft:block/template_farmland",
            r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#top"}}},{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#dirt"}}}]}"##,
        );
        let own = ParsedModels::load(&fixture.0).block_library(&fixture.0, vec![name.into()])?;
        assert_eq!(own.resolve(name)?.elements().len(), 2);
        Ok(())
    }

    #[test]
    fn shared_views_preserve_resolution_errors_builtins_and_reload() -> Result<()> {
        let fixture = Fixture(
            std::env::temp_dir().join(format!("horizon-shared-models-{}", std::process::id())),
        );
        fixture.write("horizon:parent", r##"{"textures":{"side":"#all","all":"block/stone"},"elements":[{"from":[0,0,0],"to":[16,8,16],"faces":{"up":{"texture":"#side"}}}]}"##);
        fixture.write("horizon:nested/child", r#"{"parent":"horizon:parent","textures":{"all":"block/glass"},"display":{"gui":{"rotation":[30,45,0]}}}"#);
        fixture.write("horizon:broken", "{");
        fixture.write("horizon:dangling", r#"{"parent":"horizon:absent"}"#);
        fixture.write("horizon:a", r#"{"parent":"horizon:b"}"#);
        fixture.write("horizon:b", r#"{"parent":"horizon:a"}"#);
        fixture.write(
            GENERATED_MODEL,
            r#"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"particle"}}}],"gui_light":"side"}"#,
        );
        fixture.write(MISSING_MODEL, r#"{"elements":[]}"#);
        let parsed = ParsedModels::load(&fixture.0);
        let direct = ParsedModels::default();
        let names = vec![
            "horizon:nested/child".into(),
            GENERATED_MODEL.into(),
            MISSING_MODEL.into(),
            "horizon:a".into(),
        ];
        let old = direct.block_library(&fixture.0, names.clone())?;
        let shared = parsed.block_library(&fixture.0, names)?;
        for name in ["horizon:nested/child", GENERATED_MODEL, MISSING_MODEL] {
            assert_eq!(
                format!("{:?}", old.resolve(name)?),
                format!("{:?}", shared.resolve(name)?)
            );
        }
        assert!(shared.resolve("horizon:a").is_err());
        for name in ["horizon:broken", "horizon:absent", "horizon:dangling"] {
            let load = |models: &ParsedModels| {
                models
                    .block_library(&fixture.0, vec![name.into()])
                    .err()
                    .unwrap()
                    .to_string()
            };
            assert_eq!(load(&direct), load(&parsed));
            // Mods' content: vanilla's missing model instead of an error.
            let lenient = parsed.block_library_or_missing(&fixture.0, vec![name.into()])?;
            assert_eq!(
                format!("{:?}", lenient.resolve(name)?.elements()),
                format!("{:?}", lenient.resolve(MISSING_MODEL)?.elements()),
                "{name}"
            );
        }
        let items = parsed.into_item_library()?;
        let block = shared.resolve("horizon:nested/child")?;
        let item = items.resolve("horizon:nested/child")?;
        assert!(std::ptr::eq(
            block.elements().as_ptr(),
            item.elements().as_ptr()
        ));
        assert_eq!(
            item.material("side")?.unwrap().sprite,
            "minecraft:block/glass"
        );
        assert_eq!(shared.resolve(GENERATED_MODEL)?.elements().len(), 1);
        assert!(items.resolve(GENERATED_MODEL)?.elements().is_empty());
        assert!(items.resolve(GENERATED_MODEL)?.uses_item_generator());
        for name in [
            "horizon:broken",
            "horizon:absent",
            "horizon:dangling",
            MISSING_MODEL,
        ] {
            assert_eq!(items.resolve(name)?.elements().len(), 1);
        }
        assert!(items.resolve("horizon:a").is_err());
        fixture.write("horizon:parent", r#"{"elements":[]}"#);
        let reloaded = ParsedModels::load(&fixture.0).into_item_library()?;
        assert!(
            reloaded
                .resolve("horizon:nested/child")?
                .elements()
                .is_empty()
        );
        assert_eq!(items.resolve("horizon:nested/child")?.elements().len(), 1);
        // An unrelated bad item identifier must not make block preparation fail
        // (which would trigger resource-pack rollback at a different stage).
        fixture.write("horizon:INVALID", "{}");
        let parsed = ParsedModels::load(&fixture.0);
        assert!(
            parsed
                .block_library(&fixture.0, vec!["horizon:nested/child".into()])
                .is_ok()
        );
        assert!(parsed.into_item_library().is_err());
        Ok(())
    }

    /// An .obj's `mtllib` is raw text: no namespace or path step reaches a
    /// file outside the content root.
    #[test]
    fn root_assets_stay_inside_the_content_root() {
        let root = std::env::temp_dir().join(format!("horizon-root-assets-{}", std::process::id()));
        let inside = root.join("content");
        fs::create_dir_all(inside.join("assets/m/models")).unwrap();
        fs::write(inside.join("assets/m/models/a.mtl"), "newmtl a").unwrap();
        fs::write(root.join("secret.mtl"), "newmtl secret").unwrap();
        let assets = RootAssets(inside);
        assert_eq!(
            assets.read_text("m:models/a.mtl").as_deref(),
            Some("newmtl a")
        );
        for escape in [
            "../..:secret.mtl",
            "..:../secret.mtl",
            "m:../../../secret.mtl",
            "m:models/./a.mtl",
            "M:models/a.mtl",
            ":models/a.mtl",
        ] {
            assert_eq!(assets.read_text(escape), None, "{escape}");
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
