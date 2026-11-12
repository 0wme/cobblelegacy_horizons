// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon 1.8.1's own Pokémon assets, read whole. They are not in this repository: point
//! `COBBLEMON_ASSETS` at a checkout's `common/src/main/resources/assets/cobblemon` and run with
//! `-- --ignored --nocapture`. Several folders stack (see `common::roots`).
use cobblelegacy_cobblemon::{
    coded_posers::coded_poser,
    geo::{Bone, Geometry, LOCATOR_PREFIX},
    resolver::{ResolverFile, poser_root_bone, resolve},
};
use std::{collections::BTreeMap, fs, path::Path};

mod common;
use common::files;

/// A file's id as Cobblemon registers it: its name without `.json`, in Cobblemon's namespace.
fn id(path: &Path) -> String {
    let name = path.file_name().unwrap().to_string_lossy();
    format!("cobblemon:{}", name.strip_suffix(".json").unwrap())
}

#[derive(Default)]
struct Totals {
    bones: usize,
    rotated_cubes: usize,
    locators: usize,
    cubes: usize,
    quads: usize,
    depth: usize,
    bone_depth: usize,
}

fn count(part: &Bone, texture_size: [u32; 2], depth: usize, totals: &mut Totals) {
    if part.name.starts_with(LOCATOR_PREFIX) {
        totals.locators += 1;
    } else if part.name.starts_with('%') {
        totals.rotated_cubes += 1;
    } else {
        totals.bones += 1;
        totals.bone_depth = totals.bone_depth.max(depth);
    }
    totals.depth = totals.depth.max(depth);
    totals.cubes += part.cubes.len();
    for cube in &part.cubes {
        let quads = cube.quads(texture_size);
        assert!(
            quads
                .iter()
                .all(|quad| quad.vertices.iter().flatten().all(|n| n.is_finite())),
            "{}: {cube:?}",
            part.name
        );
        totals.quads += quads.len();
    }
    for child in &part.children {
        count(child, texture_size, depth + 1, totals);
    }
}

#[test]
#[ignore = "needs COBBLEMON_ASSETS, the path of Cobblemon 1.8.1's assets/cobblemon"]
fn every_pokemon_model_and_resolver_of_cobblemon_reads() {
    let read = |path: &Path| fs::read_to_string(path).unwrap();
    let mut failures = Vec::new();

    let mut models = BTreeMap::new();
    let mut totals = Totals::default();
    let geometry_files = files("bedrock/pokemon/models", ".geo.json", false);
    for path in &geometry_files {
        match Geometry::parse(&read(path)) {
            Ok(geometry) => {
                for part in &geometry.roots {
                    count(part, geometry.texture_size, 1, &mut totals);
                }
                models.insert(id(path), geometry);
            }
            Err(e) => failures.push(format!("{}: {}", path.display(), e.0)),
        }
    }
    println!(
        "geometry: {} files, {} bones, {} rotated cubes as parts, {} locators as parts, {} cubes, \
         {} quads, deepest part {}, deepest bone {}",
        geometry_files.len(),
        totals.bones,
        totals.rotated_cubes,
        totals.locators,
        totals.cubes,
        totals.quads,
        totals.depth,
        totals.bone_depth
    );

    let mut posers = BTreeMap::new();
    let poser_files = files("bedrock/pokemon/posers", ".json", false);
    for path in &poser_files {
        match poser_root_bone(&read(path)) {
            Ok(root_bone) => {
                posers.insert(id(path), root_bone);
            }
            Err(e) => failures.push(format!("{}: {}", path.display(), e.0)),
        }
    }
    println!(
        "posers: {} files, {} with a rootBone",
        poser_files.len(),
        posers.values().flatten().count()
    );

    let mut species: BTreeMap<String, Vec<ResolverFile>> = BTreeMap::new();
    let resolver_files = files("bedrock/pokemon/resolvers", ".json", true);
    let mut variations = 0;
    for path in &resolver_files {
        match ResolverFile::parse(&read(path)) {
            Ok(file) => {
                variations += file.variations.len();
                species.entry(file.species.clone()).or_default().push(file);
            }
            Err(e) => failures.push(format!("{}: {}", path.display(), e.0)),
        }
    }
    println!(
        "resolvers: {} files, {} species, {variations} variations",
        resolver_files.len(),
        species.len()
    );

    // Every species with no aspect at all, then with each of its variations' aspects, taken through to
    // the part that would be drawn.
    let (mut picks, mut with_poser_file, mut with_coded_poser, mut by_position) = (0, 0, 0, 0);
    let mut several_roots = Vec::new();
    let mut without_poser = Vec::new();
    for (name, files) in &species {
        let asked = files.iter().flat_map(|file| &file.variations);
        let mut aspect_sets: Vec<&[String]> = asked.map(|v| v.aspects.as_slice()).collect();
        aspect_sets.push(&[]);
        aspect_sets.sort();
        aspect_sets.dedup();
        for aspects in aspect_sets {
            picks += 1;
            let Some(pick) = resolve(files, aspects) else {
                failures.push(format!("{name} {aspects:?}: nothing resolved"));
                continue;
            };
            let Some(geometry) = models.get(&pick.model) else {
                failures.push(format!("{name} {aspects:?}: no model {}", pick.model));
                continue;
            };
            // A poser without a file is one of those Cobblemon builds in code; one that is neither
            // leaves the model still.
            let root_bone = posers.get(&pick.poser);
            with_poser_file += usize::from(root_bone.is_some());
            let poser = pick.poser.split_once(':').unwrap().1;
            if root_bone.is_none() {
                match coded_poser(poser) {
                    Some(_) => with_coded_poser += 1,
                    None => without_poser.push(format!("{name} {aspects:?}: {}", pick.poser)),
                }
            }
            let root_bone = root_bone.and_then(|name| name.as_deref());
            let Some(drawn) = geometry.drawn_root(root_bone, poser) else {
                failures.push(format!("{name} {aspects:?}: no part to draw"));
                continue;
            };
            // Found by neither name, the part is the model's first root: Cobblemon's own first, from a
            // hash map, is only sure to be the same when the model has one root besides its locator.
            if root_bone != Some(&drawn.name) && drawn.name != poser {
                by_position += 1;
                if geometry.roots.len() > 2 {
                    several_roots.push(format!("{name} {aspects:?}"));
                }
            }
        }
    }
    println!(
        "picks: {picks} sets of aspects, each resolved to a model that is there and a part to draw; \
         {with_poser_file} to a poser that is a file; {by_position} to a part found by neither name, \
         of those in a model of several roots: {several_roots:?}"
    );
    println!(
        "posers of the picks: {with_poser_file} a file, {with_coded_poser} one Cobblemon builds in code, \
         {} neither: {:?}",
        without_poser.len(),
        without_poser.iter().take(12).collect::<Vec<_>>()
    );

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
