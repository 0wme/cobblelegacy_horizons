// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon 1.8.1's own particle effects, read and played whole. They are not in this repository:
//! point `COBBLEMON_ASSETS` at a checkout's `common/src/main/resources/assets/cobblemon` and run with
//! `-- --ignored --nocapture`. Several folders stack (see `common::roots`).
use cobblelegacy_cobblemon::{
    snowstorm::{Effect, Facing, Material},
    storm::{Body, Frame, Quad, Start, Storms, Tag},
};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::Arc,
};

mod common;
use common::{files, roots};

/// Every effect of the roots, by its identifier: a file of a root replaces the one a root before it
/// has at the same path, and an identifier is the last file's that claims it.
fn effects() -> (HashMap<String, Arc<Effect>>, usize) {
    let paths = files("bedrock/particles", ".particle.json", true);
    let mut effects = HashMap::new();
    for path in &paths {
        let json = fs::read_to_string(path).unwrap();
        let effect = Effect::parse(&json).unwrap_or_else(|e| panic!("{}: {}", path.display(), e.0));
        effects.insert(effect.id.clone(), Arc::new(effect));
    }
    (effects, paths.len())
}

/// The file of an effect's texture, in the root that wins among those that have it.
fn texture(effect: &Effect) -> Option<std::path::PathBuf> {
    let (namespace, path) = effect.texture_path();
    roots()
        .iter()
        .rev()
        // The roots are `assets/cobblemon` folders: another namespace is beside them.
        .map(|root| root.parent().unwrap().join(namespace).join(&path))
        .find(|file| file.is_file())
}

/// Plays `storms` for `ticks`, drawing twice a tick: what it drew, and the most particles at once.
fn play(
    storms: &mut Storms,
    find: &dyn Fn(&str) -> Option<Arc<Effect>>,
    ticks: usize,
) -> (Vec<(String, Quad)>, usize) {
    let (mut drawn, mut most) = (Vec::new(), 0);
    for _ in 0..ticks {
        storms.tick(find);
        for partial in [0.0, 0.5] {
            let mut now = 0;
            storms.quads(partial, &mut |effect, quad| {
                now += 1;
                drawn.push((effect.id.clone(), *quad));
            });
            most = most.max(now);
        }
    }
    (drawn, most)
}

#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn every_effect_of_cobblemon_is_read() {
    let (effects, files) = effects();
    assert!(files > 1000, "{files} files");
    let mut unhandled: BTreeMap<String, usize> = BTreeMap::new();
    let (mut clean, mut missing_textures, mut missing_children) = (0, Vec::new(), Vec::new());
    let (mut facings, mut materials) = (BTreeMap::new(), BTreeMap::new());
    for effect in effects.values() {
        clean += usize::from(effect.unhandled.is_empty());
        for note in &effect.unhandled {
            // One line a kind: the names are in backticks.
            let kind = if note.contains("is taken as 0") {
                "an expression this MoLang does not take".to_owned()
            } else {
                note.clone()
            };
            *unhandled.entry(kind).or_default() += 1;
        }
        if texture(effect).is_none() {
            missing_textures.push((effect.id.clone(), effect.texture.clone()));
        }
        for child in effect.children() {
            if !effects.contains_key(child) {
                missing_children.push((effect.id.clone(), child.to_owned()));
            }
        }
        *facings
            .entry(format!("{:?}", effect.particle.facing))
            .or_insert(0) += 1;
        *materials
            .entry(format!("{:?}", effect.material))
            .or_insert(0) += 1;
    }
    println!(
        "{files} files, {} effects, {clean} with nothing left out",
        effects.len()
    );
    println!("facings: {facings:?}");
    println!("materials: {materials:?}");
    let mut kinds: Vec<_> = unhandled.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (kind, count) in &kinds {
        println!("{count:5}  {kind}");
    }
    missing_textures.sort();
    println!("{} effects without their texture:", missing_textures.len());
    for (id, texture) in &missing_textures {
        println!("       {id}: {texture}");
    }
    println!(
        "{} events start an effect that is not there",
        missing_children.len()
    );
    for (id, child) in &missing_children {
        println!("       {id}: {child}");
    }
    // Nearly all of them have their texture; the few that do not have none in Cobblemon either.
    assert!(missing_textures.len() * 50 < effects.len());
    assert_eq!(
        (materials.get("Add"), effects.values().next().is_some()),
        (
            Some(
                &effects
                    .values()
                    .filter(|e| e.material == Material::Add)
                    .count()
            ),
            true
        )
    );
    assert!(
        effects
            .values()
            .any(|e| e.particle.facing == Facing::LookAtDirection)
    );
}

#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn every_ball_has_its_four_effects_and_they_play() {
    let (effects, _) = effects();
    let find = |id: &str| effects.get(id).cloned();
    let balls: Vec<String> = files("bedrock/poke_balls/variations", "_base.json", false)
        .iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.trim_start_matches("0_")
                .trim_end_matches("_base.json")
                .to_owned()
        })
        .collect();
    assert!(balls.len() >= 48, "{balls:?}");
    let (mut with, mut without, mut absent) = (0, Vec::new(), Vec::new());
    let mut beyond = std::collections::BTreeSet::new();
    for ball in &balls {
        // `PokemonClientDelegate`: the Ball's name without its underscores.
        let kind = ball.replace('_', "");
        for mode in ["casual", "battle"] {
            let ids = [
                format!("cobblemon:{kind}/{mode}/sendflash"),
                format!("cobblemon:{kind}/{mode}/ballsparks"),
                format!("cobblemon:{kind}/{mode}/ballsendsparkle"),
                format!("cobblemon:{kind}/ballsparkle"),
            ];
            if !effects.contains_key(&ids[0]) {
                without.push(format!("{ball} ({mode})"));
                continue;
            }
            with += 1;
            let at = [100.5, 64.0, -20.5];
            let mut storms = Storms::default();
            let mut all = Vec::new();
            // The flash, the sparks a tick later, the sparkle eight ticks after them.
            for (id, wait) in ids.iter().zip([0, 1, 0, 8]) {
                // Cobblemon plays what there is: three of the old Balls name their battle sparks
                // otherwise, and have none.
                let Some(effect) = find(id) else {
                    absent.push(id.clone());
                    continue;
                };
                assert!(effect.unhandled.is_empty(), "{id}: {:?}", effect.unhandled);
                assert!(texture(&effect).is_some(), "{id}: no {}", effect.texture);
                assert_eq!(effect.particle.facing, Facing::RotateXyz, "{id}");
                if wait > 0 {
                    all.extend(play(&mut storms, &find, wait).0);
                }
                storms.start(effect, Start::at(at), &find);
            }
            let (drawn, _) = play(&mut storms, &find, 60);
            all.extend(drawn);
            assert!(
                storms.is_empty(),
                "{kind}/{mode} still plays after 3 seconds"
            );
            for id in ids.iter().filter(|id| !absent.contains(id)) {
                let of: Vec<&Quad> = all
                    .iter()
                    .filter(|(drawn, _)| drawn == id)
                    .map(|(_, quad)| quad)
                    .collect();
                assert!(!of.is_empty(), "{id} drew nothing");
                for quad in of {
                    // A size may end a hair below nothing, where its curve ends.
                    assert!(
                        quad.center.iter().all(|n| n.is_finite())
                            && quad
                                .half
                                .iter()
                                .all(|n| n.is_finite() && *n > -0.01 && *n < 4.0),
                        "{id}: {quad:?}"
                    );
                    // A frame of the sheet; a few effects count one frame more than their sheet has
                    // and end on what is beside it in the atlas.
                    assert!(
                        quad.uv[0] < quad.uv[2] && quad.uv[1] < quad.uv[3],
                        "{id}: {quad:?}"
                    );
                    if !quad.uv.iter().all(|n| (-1.0e-6..=1.000_001).contains(n)) {
                        beyond.insert(id.clone());
                    }
                    // Within a few blocks of where the Ball opened.
                    let away: f64 = (0..3)
                        .map(|axis| (quad.center[axis] - at[axis]).powi(2))
                        .sum();
                    assert!(away.sqrt() < 12.0, "{id}: {quad:?}");
                }
            }
        }
    }
    println!("{with} Ball and mode pairs play; none for: {without:?}");
    println!("effects that are not there: {absent:?}");
    println!(
        "{} effects show a frame beyond their sheet: {beyond:?}",
        beyond.len()
    );
    assert!(beyond.len() * 10 < with * 4, "{beyond:?}");
    assert!(with >= 96);
    assert!(absent.len() <= 3, "{absent:?}");
}

#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn the_shiny_ring_plays_on_a_pokemon() {
    let (effects, _) = effects();
    let find = |id: &str| effects.get(id).cloned();
    let ring = find("cobblemon:shiny_ring").expect("the shiny ring");
    let feet = [8.5, 70.0, 3.5];
    let body = Body {
        width: 0.4,
        height: 0.5,
        scale: 1.0,
        center: [feet[0], feet[1] + 0.25, feet[2]],
    };
    let middle = Frame::at([feet[0], feet[1] + 0.25, feet[2]]);
    let mut storms = Storms::default();
    storms.start(
        ring,
        Start {
            emitter: Frame::turned(middle.origin, 180.0, 1.0),
            attached: Some(middle),
            tag: Some(Tag {
                entity: 12,
                locator: "middle".into(),
                bound: true,
            }),
            body: Some(body),
            target: None,
            source: 0,
        },
        &find,
    );
    // The ring's eight emitters and the sparkles its creation events start, and its chime.
    let started: Vec<String> = storms.iter().map(|s| s.effect.id.clone()).collect();
    println!("started: {started:?}");
    assert!(started.len() >= 9, "{started:?}");
    assert!(
        (2..=8).all(|n| started.contains(&format!("cobblemon:shiny_ring{n}"))),
        "{started:?}"
    );
    assert_eq!(storms.sounds.len(), 1);
    assert_eq!(storms.sounds[0].id, "cobblemon:particle.shiny_chime");
    let (drawn, most) = play(&mut storms, &find, 80);
    let mut by_effect: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, quad) in &drawn {
        *by_effect.entry(id).or_default() += 1;
        assert!(quad.center.iter().all(|n| n.is_finite()), "{id}: {quad:?}");
        // Around the Pokémon, not far from it.
        let away: f64 = (0..3)
            .map(|axis| (quad.center[axis] - middle.origin[axis]).powi(2))
            .sum();
        assert!(away.sqrt() < 6.0, "{id}: {quad:?}");
    }
    println!("drawn: {by_effect:?}, at most {most} at once");
    assert!(by_effect.contains_key("cobblemon:shiny_ring"));
    assert!(by_effect.len() >= 9, "{by_effect:?}");
    assert!(storms.is_empty(), "the ring still plays after 4 seconds");
}

/// The effects the animations of a pose start (`particle_effects`): each on a locator of a Pokémon
/// that stands still, as `BedrockParticleKeyframe.run` starts them. They go on for as long as the
/// pose lasts, and what they draw can be seen: it has a size, is not clear, and stays about the
/// Pokémon.
#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn the_effects_of_a_pose_s_animations_play_on_a_pokemon() {
    let (effects, _) = effects();
    let find = |id: &str| effects.get(id).cloned();
    let feet = [8.5, 70.0, 3.5];
    for id in [
        "cobblemon:ghostly_smoke",
        "cobblemon:shiny_ghostly_smoke",
        "cobblemon:shiny_ghostly_smoke_center",
        "cobblemon:slugma_lava_bubble",
        "cobblemon:shiny_slugma_lava_bubble",
        "cobblemon:magcargo_lava_bubble",
        "cobblemon:hooh_tailtrail",
    ] {
        let effect = find(id).unwrap_or_else(|| panic!("no effect {id}"));
        let body = Body {
            width: 0.8,
            height: 0.8,
            scale: 1.0,
            center: [feet[0], feet[1] + 0.4, feet[2]],
        };
        // A locator a little above the feet, the Pokémon turned a quarter and of its natural size.
        let locator = Frame::turned([feet[0], feet[1] + 0.3, feet[2]], 90.0, 1.0);
        let mut storms = Storms::default();
        storms.start(
            effect.clone(),
            Start {
                emitter: Frame {
                    scale: if effect.emitter.entity_scaling {
                        locator.scale
                    } else {
                        1.0
                    },
                    ..locator
                },
                attached: Some(locator),
                tag: Some(Tag {
                    entity: 12,
                    locator: "gas".into(),
                    bound: true,
                }),
                body: Some(body),
                target: None,
                source: 1,
            },
            &find,
        );
        let (drawn, most) = play(&mut storms, &find, 120);
        let seen = drawn
            .iter()
            .filter(|(_, quad)| quad.half[0] > 0.0 && quad.half[1] > 0.0 && quad.colour[3] > 0.0)
            .count();
        let widest = drawn.iter().map(|(_, q)| q.half[0]).fold(0.0, f32::max);
        let farthest = drawn
            .iter()
            .map(|(_, quad)| {
                (0..3)
                    .map(|axis| (quad.center[axis] - locator.origin[axis]).powi(2))
                    .sum::<f64>()
                    .sqrt()
            })
            .fold(0.0, f64::max);
        println!(
            "{id}: {} quads drawn in six seconds, {seen} of them seen, at most {most} at once, the \
             widest {widest:.3} across its half, the farthest {farthest:.2} away; lacking: {:?}",
            drawn.len(),
            effect.unhandled
        );
        assert!(seen > 0, "{id} draws nothing that is seen");
        assert!(farthest < 8.0, "{id} goes {farthest} away");
        // It goes on: its emitter is found again by its number, and is not gone.
        assert!(
            storms.iter().any(|s| s.source == 1 && !s.despawned()),
            "{id} ended by itself"
        );
    }
}

#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn every_effect_plays_without_a_number_that_is_none() {
    let (effects, _) = effects();
    let find = |id: &str| effects.get(id).cloned();
    let mut ids: Vec<&String> = effects.keys().collect();
    ids.sort();
    let (mut drew, mut silent, mut busiest) = (0, Vec::new(), (0, String::new()));
    for id in ids {
        let at = [0.5, 80.0, 0.5];
        let body = Body {
            width: 0.8,
            height: 1.2,
            scale: 1.0,
            center: [at[0], at[1] + 0.6, at[2]],
        };
        let mut storms = Storms::default();
        storms.start(
            find(id).unwrap(),
            Start {
                emitter: Frame::at(at),
                attached: None,
                tag: None,
                body: Some(body),
                target: Some([at[0], at[1], at[2] + 5.0]),
                source: 0,
            },
            &find,
        );
        // Two seconds: what loops for ever is cut there.
        let (drawn, most) = play(&mut storms, &find, 40);
        if most > busiest.0 {
            busiest = (most, id.clone());
        }
        if drawn.is_empty() {
            silent.push(id.clone());
        } else {
            drew += 1;
        }
        for (of, quad) in &drawn {
            assert!(
                quad.center.iter().all(|n| n.is_finite()),
                "{id} ({of}): {quad:?}"
            );
        }
    }
    println!(
        "{drew} effects drew particles, {} drew none in two seconds; the busiest, {}, had {} at once",
        silent.len(),
        busiest.1,
        busiest.0
    );
    println!("none from: {silent:?}");
    assert!(drew * 10 > effects.len() * 9);
}
