// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The posers Cobblemon 1.8.1 builds in code, as `tools/cobblelegacy/posers_from_kotlin.py` wrote them
//! (`coded_posers`): each reads, and, against Cobblemon's own assets, names animations and parts that are
//! there. The assets are not in this repository: for the second test, point `COBBLEMON_ASSETS` at a
//! checkout's `common/src/main/resources/assets/cobblemon` and run with `-- --ignored --nocapture`.
use cobblelegacy_cobblemon::{
    animation::AnimationFile,
    coded_posers::{coded_poser, names},
    geo::{Bone, Geometry},
    molang,
    poser::{Animation, Argument, Expression, Poser, Visible},
    resolver::{ResolverFile, resolve},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The functions the converter writes a pose's animations with, in the order of [`kind`].
const KINDS: [&str; 7] = [
    "bedrock",
    "look",
    "pitch_tilt",
    "biped_walk",
    "quadruped_walk",
    "bimanual_swing",
    "sine_wing_flap",
];

/// Which of [`KINDS`] an animation is, None for one that is none of them.
fn kind(animation: &Animation) -> Option<&'static str> {
    Some(match animation {
        Animation::Bedrock { .. } => KINDS[0],
        Animation::Look(_) => KINDS[1],
        Animation::PitchTilt(_) => KINDS[2],
        Animation::BipedWalk(_) => KINDS[3],
        Animation::QuadrupedWalk(_) => KINDS[4],
        Animation::BimanualSwing(_) => KINDS[5],
        Animation::SineWingFlap(_) => KINDS[6],
        Animation::Other(_) => return None,
    })
}

/// Every coded poser, read.
fn posers() -> Vec<(&'static str, Poser)> {
    names()
        .map(|name| {
            let json = coded_poser(name).unwrap_or_else(|| panic!("{name}: named and not there"));
            let poser = Poser::parse(json).unwrap_or_else(|e| panic!("{name}: {}", e.0));
            (name, poser)
        })
        .collect()
}

#[test]
fn every_coded_poser_reads() {
    let listed: Vec<&str> = names().collect();
    assert!(!listed.is_empty());
    assert!(
        listed.windows(2).all(|pair| pair[0] < pair[1]),
        "the names are sorted, each once"
    );
    assert_eq!(coded_poser("no_such_poser"), None);
    assert_eq!(coded_poser("cobblemon:gengar"), None);
    for (name, poser) in posers() {
        assert!(!poser.poses.is_empty(), "{name}: no pose");
        // The converter writes nothing Cobblemon would read past, and no animation it would drop.
        assert!(poser.oddities.is_empty(), "{name}: {:?}", poser.oddities);
        // A coded poser always names its root part.
        assert!(poser.root_bone.is_some(), "{name}: no rootBone");
        for pose in &poser.poses {
            assert!(
                !pose.pose_types.is_empty(),
                "{name}, {}: no pose type",
                pose.name
            );
            // `registerPose` gives every pose both, where a file may leave the second out.
            assert!(
                pose.transform_to_ticks.is_some(),
                "{name}, {}: no transformToTicks",
                pose.name
            );
            for entry in &pose.animations {
                assert!(
                    kind(&entry.animation).is_some(),
                    "{name}, {}: {:?}",
                    pose.name,
                    entry.animation
                );
            }
            for quirk in &pose.quirks {
                let written = matches!(quirk, Expression::Call(call)
                    if matches!(call.function.as_str(), "bedrock_quirk" | "bedrock_primary_quirk")
                        && call.arguments.len() == 5);
                assert!(written, "{name}, {}: {quirk:?}", pose.name);
            }
        }
    }
}

/// Every file under `dir` whose name ends in `suffix`, sorted by path as a resource listing is.
fn files(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, suffix, out);
            } else if path.to_string_lossy().ends_with(suffix) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, suffix, &mut out);
    out.sort();
    out
}

/// A file's name without `suffix`.
fn stem(path: &Path, suffix: &str) -> String {
    let name = path.file_name().unwrap().to_string_lossy();
    name.strip_suffix(suffix).unwrap().to_owned()
}

/// The names of a part and of all under it.
fn part_names<'a>(part: &'a Bone, out: &mut BTreeSet<&'a str>) {
    out.insert(&part.name);
    for child in &part.children {
        part_names(child, out);
    }
}

/// The strings between single quotes in MoLang.
fn quoted(source: &str) -> Vec<String> {
    source
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// The Bedrock animations MoLang asks for by name: for each `q.bedrock...(` in it, the group and the
/// animation, which are the first two strings after it.
fn bedrock_calls_in(source: &str) -> Vec<(String, String)> {
    source
        .split("q.bedrock")
        .skip(1)
        .filter_map(|call| {
            let mut strings = quoted(call).into_iter();
            Some((strings.next()?, strings.next()?))
        })
        .collect()
}

/// The Bedrock animations a named animation, a quirk or a transition asks for by name, and whether all
/// it asks for was found written out.
fn bedrock_calls(expression: &Expression) -> (Vec<(String, String)>, bool) {
    match expression {
        Expression::Call(call) if call.function.starts_with("bedrock") => {
            match (call.arguments.first(), call.arguments.get(1)) {
                (Some(Argument::String(group)), Some(Argument::String(name))) => {
                    (vec![(group.clone(), name.clone())], true)
                }
                _ => (Vec::new(), false),
            }
        }
        Expression::Call(_) => (Vec::new(), false),
        Expression::Molang(source) => {
            let calls = bedrock_calls_in(source);
            let all = !calls.is_empty();
            (calls, all)
        }
    }
}

/// Adds the Bedrock animations `expression` asks for by name to `wanted`, each with the `place` it is
/// asked in.
fn ask(
    poser: &str,
    place: String,
    expression: &Expression,
    wanted: &mut Vec<(String, String, String)>,
    mismatches: &mut Vec<String>,
) {
    let (calls, all) = bedrock_calls(expression);
    if !all {
        mismatches.push(format!(
            "{poser}, {place}: not every animation of {expression:?} is written out"
        ));
    }
    wanted.extend(calls.into_iter().map(|(g, n)| (place.clone(), g, n)));
}

/// What the test below finds and Cobblemon 1.8.1 itself explains: nothing else may come up. Each is what
/// the lines the test prints for it start and end with, and the explanation.
const EXPLAINED: [(&str, &str, &str); 1] = [(
    "unfezant, ",
    ": no such group",
    "`UnfezantModel` asks for the animation group `unfezant`, where Cobblemon has `unfezant_male` and \
     `unfezant_female`: it throws as it builds that model. Nothing asks it to: its resolvers name the \
     posers `unfezant_male` and `unfezant_female`, which are files",
)];

/// The explanation of a line the test prints, None for one that has none.
fn explained(line: &str) -> Option<&'static str> {
    EXPLAINED
        .iter()
        .find(|(start, end, _)| line.starts_with(start) && line.ends_with(end))
        .map(|(_, _, why)| *why)
}

#[test]
#[ignore = "needs COBBLEMON_ASSETS, the path of Cobblemon 1.8.1's assets/cobblemon"]
fn every_coded_poser_names_what_cobblemon_has() {
    let assets = std::env::var_os("COBBLEMON_ASSETS")
        .expect("COBBLEMON_ASSETS: the path of Cobblemon 1.8.1's assets/cobblemon");
    // The other tests on real assets take a stack of folders (a pack, Cobblemon, mods). This one is
    // about the classes of Cobblemon and so about Cobblemon's own assets: in a stack, a pack's poser file
    // takes a class's place and its models differ.
    if std::env::split_paths(&assets).count() != 1 {
        println!("coded posers: not checked against a stack of folders, name Cobblemon's alone");
        return;
    }
    let root = PathBuf::from(assets).join("bedrock/pokemon");
    let read = |path: &Path| fs::read_to_string(path).unwrap();
    let mut mismatches: Vec<String> = Vec::new();

    // The animations of each group: a group is a file's name, and a later file of the same name takes
    // the earlier one's place (`BedrockAnimationRepository.loadAnimations`).
    let mut groups: BTreeMap<String, AnimationFile> = BTreeMap::new();
    for path in files(&root.join("animations"), ".animation.json") {
        let file = AnimationFile::parse(&read(&path))
            .unwrap_or_else(|e| panic!("{}: {}", path.display(), e.0));
        groups.insert(stem(&path, ".animation.json"), file);
    }
    let animation = |group: &str, name: &str| {
        groups
            .get(group)
            .and_then(|file| file.get(&format!("animation.{group}.{name}")))
    };

    let mut models = BTreeMap::new();
    for path in files(&root.join("models"), ".geo.json") {
        let geometry =
            Geometry::parse(&read(&path)).unwrap_or_else(|e| panic!("{}: {}", path.display(), e.0));
        models.insert(format!("cobblemon:{}", stem(&path, ".json")), geometry);
    }

    // The posers that are files: a coded poser of the same name is never the poser.
    let poser_files: BTreeSet<String> = files(&root.join("posers"), ".json")
        .iter()
        .map(|path| stem(path, ".json"))
        .collect();

    // The models each poser is drawn with: every species with no aspect at all, then with each of its
    // variations' aspects, as `tests/real_assets.rs` resolves them.
    let mut species: BTreeMap<String, Vec<ResolverFile>> = BTreeMap::new();
    for path in files(&root.join("resolvers"), ".json") {
        let file = ResolverFile::parse(&read(&path))
            .unwrap_or_else(|e| panic!("{}: {}", path.display(), e.0));
        species.entry(file.species.clone()).or_default().push(file);
    }
    let mut paired: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for resolvers in species.values() {
        let asked = resolvers.iter().flat_map(|file| &file.variations);
        let mut aspect_sets: Vec<&[String]> = asked.map(|v| v.aspects.as_slice()).collect();
        aspect_sets.push(&[]);
        aspect_sets.sort();
        aspect_sets.dedup();
        for aspects in aspect_sets {
            if let Some(pick) = resolve(resolvers, aspects)
                && let Some(poser) = pick.poser.strip_prefix("cobblemon:")
            {
                paired
                    .entry(poser.to_owned())
                    .or_default()
                    .insert(pick.model);
            }
        }
    }

    let posers = posers();
    let (mut poses, mut conditional, mut quirks, mut named, mut transitions) = (0, 0, 0, 0, 0);
    let (mut asked, mut parts_checked, mut parts_named, mut transformed) = (0, 0, 0, 0);
    let (mut pairs, mut in_some_only) = (0, Vec::new());
    let (mut move_the_root, mut posers_moving_the_root) = (0, 0);
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unpaired = Vec::new();
    let mut root_named = Vec::new();
    let mut unread_molang: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (name, poser) in &posers {
        if poser_files.contains(*name) {
            mismatches.push(format!("{name}: Cobblemon has a poser file of this name"));
        }

        // The Bedrock animations: a pose's own, which Cobblemon looks up as it builds the model, then
        // those it only looks up when they play.
        let mut wanted: Vec<(String, String, String)> = Vec::new();
        for (key, expression) in &poser.named_animations {
            named += 1;
            ask(
                name,
                format!("named animation `{key}`"),
                expression,
                &mut wanted,
                &mut mismatches,
            );
        }
        // The parts: what each is named for, and its name.
        let mut parts: Vec<(String, &str)> = Vec::new();
        // MoLang the files carry for whoever draws to answer.
        let mut molang_sources: Vec<&str> = Vec::new();
        for pose in &poser.poses {
            poses += 1;
            molang_sources.extend(pose.conditions.molang.iter().flatten().map(String::as_str));
            for entry in &pose.animations {
                *by_kind.entry(kind(&entry.animation).unwrap()).or_default() += 1;
                if let Some(condition) = &entry.condition {
                    conditional += 1;
                    molang_sources.push(condition);
                }
                if let Animation::Bedrock {
                    group,
                    name: animation,
                } = &entry.animation
                {
                    wanted.push((
                        format!("pose `{}`", pose.name),
                        group.clone(),
                        animation.clone(),
                    ));
                }
                for bone in entry.animation.bones() {
                    parts.push((format!("pose `{}`, {:?}", pose.name, entry.animation), bone));
                }
            }
            for quirk in &pose.quirks {
                quirks += 1;
                ask(
                    name,
                    format!("pose `{}`, a quirk", pose.name),
                    quirk,
                    &mut wanted,
                    &mut mismatches,
                );
            }
            for (key, expression) in &pose.named_animations {
                named += 1;
                ask(
                    name,
                    format!("pose `{}`, named animation `{key}`", pose.name),
                    expression,
                    &mut wanted,
                    &mut mismatches,
                );
            }
            for (to, expression) in &pose.transitions {
                transitions += 1;
                if !poser.poses.iter().any(|other| other.name == *to) {
                    mismatches.push(format!(
                        "{name}, pose `{}`: a transition to `{to}`, which is no pose",
                        pose.name
                    ));
                }
                ask(
                    name,
                    format!("pose `{}`, the transition to `{to}`", pose.name),
                    expression,
                    &mut wanted,
                    &mut mismatches,
                );
            }
            for part in &pose.transformed_parts {
                transformed += 1;
                parts.push((
                    format!("pose `{}`, a transformed part", pose.name),
                    &part.part,
                ));
                if let Some(Visible::Molang(source)) = &part.visible {
                    molang_sources.push(source);
                }
            }
        }
        assert!(
            poser.transformed_parts.is_empty(),
            "{name}: the converter writes no transformed part of the poser itself"
        );
        asked += wanted.len();
        wanted.sort();
        wanted.dedup();
        // A class registers its root part under the bone's own name, so an animation that names that
        // bone moves the whole model. (A poser file's root part is `__root` to Cobblemon, and only
        // `root_part` in an animation reaches it.)
        let root_bone = poser.root_bone.as_deref().unwrap();
        let mut moving_the_root = 0;
        for (place, group, name_asked) in &wanted {
            match animation(group, name_asked) {
                Some(found) => {
                    let moves = found.bones.iter().any(|bone| bone.bone == root_bone);
                    moving_the_root += usize::from(moves);
                }
                None => {
                    let why = if groups.contains_key(group) {
                        "the group has no such animation"
                    } else {
                        "no such group"
                    };
                    mismatches.push(format!(
                        "{name}, {place}: animation.{group}.{name_asked}: {why}"
                    ));
                }
            }
        }
        move_the_root += moving_the_root;
        posers_moving_the_root += usize::from(moving_the_root > 0);
        molang_sources.sort_unstable();
        molang_sources.dedup();
        for source in molang_sources {
            if molang::Expression::parse(source).is_err() {
                unread_molang
                    .entry(source.to_owned())
                    .or_default()
                    .push(name);
            }
        }

        // The models the resolvers draw this poser with, and in each the part it draws and all under
        // it: the parts a Kotlin class finds by name (`registerChildWithAllChildren`, `getPart`).
        let drawn_with: Vec<&String> = paired.get(*name).into_iter().flatten().collect();
        if drawn_with.is_empty() {
            unpaired.push(*name);
            continue;
        }
        pairs += drawn_with.len();
        let mut names_by_model: Vec<(&str, BTreeSet<&str>)> = Vec::new();
        for model in &drawn_with {
            let Some(geometry) = models.get(*model) else {
                mismatches.push(format!("{name}: no model {model}"));
                continue;
            };
            let mut within = BTreeSet::new();
            match geometry.roots.iter().find(|part| part.name == root_bone) {
                Some(root) => part_names(root, &mut within),
                None => root_named.push(format!(
                    "{name}: its root part `{root_bone}` is no root of {model}"
                )),
            }
            // What the reader of the file draws is that same part.
            let drawn = geometry
                .drawn_root(Some(root_bone), name)
                .map(|part| &part.name);
            if !within.is_empty() && drawn.map(String::as_str) != Some(root_bone) {
                mismatches.push(format!(
                    "{name}: {model} is drawn from {drawn:?}, not from `{root_bone}`"
                ));
            }
            names_by_model.push((model, within));
        }
        parts.sort();
        parts.dedup();
        let mut seen = BTreeSet::new();
        for (place, part) in &parts {
            parts_named += 1;
            if !seen.insert(*part) {
                continue;
            }
            parts_checked += 1;
            let lacking: Vec<&str> = names_by_model
                .iter()
                .filter(|(_, within)| !within.contains(part))
                .map(|(model, _)| *model)
                .collect();
            if lacking.len() == names_by_model.len() {
                mismatches.push(format!(
                    "{name}, {place}: no model it is drawn with has a part `{part}`"
                ));
            } else if !lacking.is_empty() {
                in_some_only.push(format!("{name}, {place}: `{part}` is not in {lacking:?}"));
            }
        }
    }

    println!(
        "coded posers: {}, {poses} poses; {} of them drawn by a resolver, with {pairs} models in all",
        posers.len(),
        posers.len() - unpaired.len()
    );
    println!(
        "pose animations: {} ({conditional} with a condition); {}",
        by_kind.values().sum::<usize>(),
        KINDS
            .iter()
            .map(|kind| format!("{kind} {}", by_kind.get(kind).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "also: {quirks} quirks, {named} named animations, {transitions} transitions, {transformed} \
         transformed parts"
    );
    println!(
        "Bedrock animations asked for by name: {asked}, all looked up in {} groups; {move_the_root} of \
         them, in {posers_moving_the_root} posers, name the poser's root bone",
        groups.len()
    );
    println!(
        "parts: a pose names one {parts_named} times, which is {parts_checked} parts counted once a \
         poser, each looked up in every model the poser is drawn with; so was the root part, {pairs} times"
    );
    println!(
        "no resolver draws with these {}, whose parts were not looked up: {unpaired:?}",
        unpaired.len()
    );
    println!(
        "a root part that is no root of a model the poser is drawn with: {}",
        root_named.len()
    );
    for line in &root_named {
        println!("  {line}");
    }
    println!(
        "parts in some of a poser's models only: {}",
        in_some_only.len()
    );
    for line in &in_some_only {
        println!("  {line}");
    }
    println!(
        "MoLang in conditions and visibilities that `molang::Expression` does not read: {}",
        unread_molang.len()
    );
    for (source, posers) in &unread_molang {
        println!("  `{source}`: {posers:?}");
    }
    mismatches.extend(root_named);
    mismatches.sort();
    println!("mismatches: {}", mismatches.len());
    for line in &mismatches {
        let known = if explained(line).is_some() {
            " (explained)"
        } else {
            ""
        };
        println!("  {line}{known}");
    }
    for (start, end, why) in &EXPLAINED {
        println!("explained, `{start}...{end}`: {why}");
    }
    let unexplained: Vec<&String> = mismatches
        .iter()
        .filter(|line| explained(line).is_none())
        .collect();
    let stale: Vec<&str> = EXPLAINED
        .iter()
        .filter(|(start, end, _)| {
            !mismatches
                .iter()
                .any(|line| line.starts_with(start) && line.ends_with(end))
        })
        .map(|(start, _, _)| *start)
        .collect();
    assert!(unexplained.is_empty(), "{unexplained:#?}");
    assert!(stale.is_empty(), "explained and not found: {stale:#?}");
}
