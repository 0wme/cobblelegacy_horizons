// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What the tests on real assets share: where the assets are.
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// The `assets/cobblemon` folders `COBBLEMON_ASSETS` names: one, or several as a path list (`;` on
/// Windows) that stack as resource packs do, the one that wins last. A server's own Pokémon are spread
/// that way: its resource pack, Cobblemon's resources, then the mods that add to them.
pub fn roots() -> Vec<PathBuf> {
    let list = std::env::var_os("COBBLEMON_ASSETS")
        .expect("COBBLEMON_ASSETS: the path of Cobblemon 1.8.1's assets/cobblemon");
    std::env::split_paths(&list).collect()
}

/// Every file under `sub` of each root whose name ends in `suffix`, sorted by its path under `sub` as a
/// resource listing is.
///
/// A root replaces what a root before it has: the file of the same path when `by_path` (resolvers, each
/// of which adds to its species), the files of the same name otherwise (models, posers and animations,
/// which Cobblemon registers by name). One root alone gives every file it has.
pub fn files(sub: &str, suffix: &str, by_path: bool) -> Vec<PathBuf> {
    fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
        // A root need not have every kind of file: a mod that only adds resolvers has no animation.
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, suffix, out);
            } else if path.to_string_lossy().ends_with(suffix) {
                out.push(path);
            }
        }
    }
    let key = |relative: &Path| -> String {
        if by_path {
            relative.to_string_lossy().replace('\\', "/")
        } else {
            relative.file_name().unwrap().to_string_lossy().into_owned()
        }
    };
    // Each file's path under `sub`, with where it is.
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for root in roots() {
        let dir = root.join(sub);
        let mut found = Vec::new();
        walk(&dir, suffix, &mut found);
        let found: Vec<(PathBuf, PathBuf)> = found
            .into_iter()
            .map(|path| (path.strip_prefix(&dir).unwrap().to_owned(), path))
            .collect();
        let replaced: HashSet<String> = found.iter().map(|(relative, _)| key(relative)).collect();
        kept.retain(|(relative, _)| !replaced.contains(&key(relative)));
        kept.extend(found);
    }
    kept.sort();
    kept.into_iter().map(|(_, path)| path).collect()
}
