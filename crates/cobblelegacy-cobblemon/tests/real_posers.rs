// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon 1.8.1's own Pokémon posers, read whole. They are not in this repository: point
//! `COBBLEMON_ASSETS` at a checkout's `common/src/main/resources/assets/cobblemon` and run with
//! `-- --ignored --nocapture`. Several folders stack (see `common::roots`).
use cobblelegacy_cobblemon::{
    poser::{Animation, Entity, Expression, OddityKind, Poser, TransformedPart, Visible},
    wire::PoseType,
};
use std::{collections::BTreeMap, fs};

mod common;
use common::files;

/// The seven functions that give Cobblemon a pose animation.
const POSE_ANIMATIONS: [&str; 7] = [
    "bedrock",
    "look",
    "pitch_tilt",
    "biped_walk",
    "quadruped_walk",
    "bimanual_swing",
    "sine_wing_flap",
];

fn variant(animation: &Animation) -> &'static str {
    match animation {
        Animation::Bedrock { .. } => "Bedrock",
        Animation::Look(_) => "Look",
        Animation::PitchTilt(_) => "PitchTilt",
        Animation::BipedWalk(_) => "BipedWalk",
        Animation::QuadrupedWalk(_) => "QuadrupedWalk",
        Animation::BimanualSwing(_) => "BimanualSwing",
        Animation::SineWingFlap(_) => "SineWingFlap",
        Animation::Other(_) => "Other",
    }
}

/// What an expression is, for a count: the function it calls, or that it is no call.
fn function(expression: &Expression) -> String {
    match expression {
        Expression::Call(call) => format!("q.{}", call.function),
        Expression::Molang(_) => "not a call".to_owned(),
    }
}

fn count<K: Ord>(counts: &mut BTreeMap<K, usize>, key: K) {
    *counts.entry(key).or_default() += 1;
}

fn show<K: std::fmt::Display>(counts: &BTreeMap<K, usize>) -> String {
    let shown: Vec<String> = counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
    shown.join(", ")
}

fn count_part(part: &TransformedPart, counts: &mut BTreeMap<&'static str, usize>) {
    count(counts, "parts");
    let delta = &part.delta;
    for (set, what) in [
        (delta.position != [0.0; 3], "a position"),
        (delta.rotation != [0.0; 3], "a rotation"),
        (delta.scale != [1.0; 3], "a scale"),
        (part.visible == Some(Visible::Is(true)), "visible"),
        (part.visible == Some(Visible::Is(false)), "hidden"),
        (
            matches!(part.visible, Some(Visible::Molang(_))),
            "visibility in MoLang",
        ),
        (
            *delta == cobblelegacy_cobblemon::pose::Delta::NONE && part.visible.is_none(),
            "nothing",
        ),
    ] {
        if set {
            count(counts, what);
        }
    }
}

#[test]
#[ignore = "needs COBBLEMON_ASSETS, the path of Cobblemon 1.8.1's assets/cobblemon"]
fn every_pokemon_poser_of_cobblemon_reads() {
    let mut failures = Vec::new();
    let mut posers = Vec::new();
    let poser_files = files("bedrock/pokemon/posers", ".json", false);
    for path in &poser_files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match Poser::parse(&fs::read_to_string(path).unwrap()) {
            Ok(poser) => posers.push((name, poser)),
            Err(e) => failures.push(format!("{}: {}", path.display(), e.0)),
        }
    }

    let mut poses = 0;
    let mut animations = BTreeMap::new();
    let mut others = BTreeMap::new();
    let mut with_condition = 0;
    let mut bare_looks = 0;
    let mut conditions = BTreeMap::new();
    let mut expressions: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    let mut pose_parts = BTreeMap::new();
    let mut poser_parts = BTreeMap::new();
    let mut oddities = BTreeMap::new();
    let mut with_root_bone = 0;
    for (file, poser) in &posers {
        with_root_bone += usize::from(poser.root_bone.is_some());
        for (_, expression) in &poser.named_animations {
            count(
                expressions
                    .entry("the posers' named animations")
                    .or_default(),
                function(expression),
            );
        }
        for part in &poser.transformed_parts {
            count_part(part, &mut poser_parts);
        }
        for pose in &poser.poses {
            poses += 1;
            for entry in &pose.animations {
                count(&mut animations, variant(&entry.animation));
                with_condition += usize::from(entry.condition.is_some());
                match &entry.animation {
                    Animation::Look(look) if look.or_bone.is_some() => bare_looks += 1,
                    Animation::Other(expression) => {
                        let mut what = function(expression);
                        if let Expression::Call(call) = expression
                            && POSE_ANIMATIONS.contains(&call.function.as_str())
                        {
                            what.push_str(" (arguments not written out)");
                        }
                        count(&mut others, what);
                    }
                    _ => {}
                }
            }
            for (kind, list) in [
                ("quirks", pose.quirks.iter().collect::<Vec<_>>()),
                (
                    "the poses' named animations",
                    pose.named_animations.iter().map(|(_, e)| e).collect(),
                ),
                (
                    "transitions",
                    pose.transitions.iter().map(|(_, e)| e).collect(),
                ),
            ] {
                for expression in list {
                    count(expressions.entry(kind).or_default(), function(expression));
                }
            }
            let asked = &pose.conditions;
            for (given, key) in [
                (asked.battle.is_some(), "isBattle"),
                (asked.touching_water.is_some(), "isTouchingWater"),
                (asked.under_water.is_some(), "isUnderWater"),
                (asked.in_water_or_rain.is_some(), "isInWaterOrRain"),
                (asked.dusk.is_some(), "isDusk"),
                (asked.ride_styles.is_some(), "isRideStyle"),
                (asked.molang.is_some(), "condition"),
                (asked.wild.is_some(), "isWild (not asked)"),
                (pose.transform_to_ticks.is_some(), "transformToTicks"),
                (!pose.quirks.is_empty(), "quirks"),
                (!pose.named_animations.is_empty(), "namedAnimations"),
                (!pose.transitions.is_empty(), "transitions"),
                (!pose.transformed_parts.is_empty(), "transformedParts"),
                (pose.animations.is_empty(), "no animation"),
                (pose.pose_types.is_empty(), "no pose type"),
            ] {
                if given {
                    count(&mut conditions, key);
                }
            }
            for part in &pose.transformed_parts {
                count_part(part, &mut pose_parts);
            }
        }
        for oddity in &poser.oddities {
            let kind = match &oddity.kind {
                OddityKind::Unread(_) => "text after the expression, not read",
                OddityKind::UnknownName(_) => "a name no struct has, read as 0",
                OddityKind::NoAnimation => "an entry without an animation, dropped",
                OddityKind::LegacyQuirk => "a quirk in the older object form, not played",
            };
            count(&mut oddities, kind);
            println!(
                "  odd in {file}, pose {:?}: {:?} in `{}`",
                oddity.pose.as_deref().unwrap_or("(none)"),
                oddity.kind,
                oddity.source
            );
        }
    }
    println!(
        "posers: {} files, {} read, {with_root_bone} with a rootBone, {poses} poses",
        poser_files.len(),
        posers.len()
    );
    println!(
        "animations: {} entries, {with_condition} with a condition; {}",
        animations.values().sum::<usize>(),
        show(&animations)
    );
    println!("  of the looks, {bare_looks} are the bare word `look`");
    println!("  the others: {}", show(&others));
    for (kind, counts) in &expressions {
        println!(
            "{kind}: {}; {}",
            counts.values().sum::<usize>(),
            show(counts)
        );
    }
    println!(
        "malformed entries: {}; {}",
        oddities.values().sum::<usize>(),
        show(&oddities)
    );
    println!("poses with: {}", show(&conditions));
    println!(
        "transformed parts of poses, by what they set: {}",
        show(&pose_parts)
    );
    println!(
        "transformed parts of posers, by what they set: {}",
        show(&poser_parts)
    );

    // A wild Pokémon in each of the poses one is seen in, out of battle, and out of water unless it
    // swims. Nothing here runs MoLang, so the conditions are answered all one way and then all the other.
    for (answer, said) in [(0.0, "no"), (1.0, "yes")] {
        println!("picks, every MoLang condition answered {said}:");
        for pose_type in [
            PoseType::Stand,
            PoseType::Walk,
            PoseType::Sleep,
            PoseType::Fly,
            PoseType::Hover,
            PoseType::Swim,
            PoseType::Float,
        ] {
            let in_water = matches!(pose_type, PoseType::Swim | PoseType::Float);
            let entity = Entity {
                pose: pose_type,
                touching_water: in_water,
                under_water: in_water,
                in_water_or_rain: in_water,
                ..Entity::default()
            };
            let (mut fitting, mut fallbacks, mut of_that_type) = (0, 0, 0);
            for (file, poser) in &posers {
                let Some(pose) = poser.pose_for(&entity, &mut |_| answer) else {
                    failures.push(format!("{file}: no pose at all"));
                    continue;
                };
                if pose.fits(&entity, &mut |_| answer) {
                    fitting += 1;
                } else {
                    // The fallback is the file's first pose.
                    assert_eq!(pose.name, poser.poses[0].name, "{file}");
                    fallbacks += 1;
                    let has_type = |pose: &cobblelegacy_cobblemon::poser::Pose| {
                        pose.pose_types.contains(&pose_type)
                    };
                    of_that_type += usize::from(poser.poses.iter().any(has_type));
                }
            }
            println!(
                "  {pose_type:?}: {fitting} posers have a pose that fits, {fallbacks} fall back to \
                 their first pose ({of_that_type} of them have a pose of that type, whose conditions \
                 fail)"
            );
        }
    }

    assert!(!posers.is_empty(), "no poser under {:?}", common::roots());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
