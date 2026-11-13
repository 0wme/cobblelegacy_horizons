// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What Cobblemon 1.8.1's own posers have their models play beyond their poses, each animation found
//! in its file and each model run through a battle. The files are not in this repository: point
//! `COBBLEMON_ASSETS` at a checkout's `common/src/main/resources/assets/cobblemon` and run with
//! `-- --ignored --nocapture`. Several folders stack (see `common::roots`).
use cobblelegacy_cobblemon::{
    animation::AnimationFile,
    playing::{Named, NamedQuirk, Playing, PrimaryKind, Script},
    poser::{Entity, Poser},
};
use std::{cell::RefCell, collections::HashMap, fs, sync::Arc};

mod common;
use common::files;

#[test]
#[ignore = "needs Cobblemon's assets in COBBLEMON_ASSETS"]
fn every_poser_s_named_animations_quirks_and_transitions_play() {
    // The animation groups by name, read when a poser first asks for one.
    let paths: HashMap<String, std::path::PathBuf> =
        files("bedrock/pokemon/animations", ".animation.json", false)
            .into_iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy();
                (name.trim_end_matches(".animation.json").to_owned(), path)
            })
            .collect();
    let read: RefCell<HashMap<String, Option<Arc<AnimationFile>>>> = RefCell::default();
    let mut load = |group: &str| -> Option<Arc<AnimationFile>> {
        let mut read = read.borrow_mut();
        let file = read.entry(group.to_owned()).or_insert_with(|| {
            let json = fs::read_to_string(paths.get(group)?).ok()?;
            AnimationFile::parse(&json).ok().map(Arc::new)
        });
        file.clone()
    };

    let (mut posers, mut unread) = (0, 0);
    // Written, said to be an active animation, and found in its file.
    let (mut named, mut named_meant, mut named_found) = (0, 0, 0);
    let (mut quirks, mut quirks_meant, mut quirks_found, mut legacy) = (0, 0, 0, 0);
    let (mut transitions, mut transitions_found) = (0, 0);
    let (mut battle_poses, mut blends, mut played, mut cries, mut quirked) = (0, 0, 0, 0, 0);
    let mut lost: Vec<String> = Vec::new();
    for path in files("bedrock/pokemon/posers", ".json", false) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(poser) = fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|json| Poser::parse(&json).map_err(|e| e.0))
        else {
            unread += 1;
            continue;
        };
        posers += 1;
        let script = Script::new(&poser, &mut load);
        assert_eq!(script.poses.len(), poser.poses.len(), "{name}");

        let written = poser
            .named_animations
            .iter()
            .chain(poser.poses.iter().flat_map(|pose| &pose.named_animations));
        for (key, expression) in written {
            named += 1;
            if Named::of(expression).is_some() {
                named_meant += 1;
            } else if lost.len() < 12 {
                lost.push(format!("{name} `{key}`: {expression:?}"));
            }
        }
        named_found += script.named.len();
        for (pose, scripted) in poser.poses.iter().zip(&script.poses) {
            named_found += scripted.named.len();
            quirks += pose.quirks.len() + pose.legacy_quirks.len();
            legacy += pose.legacy_quirks.len();
            quirks_meant += pose
                .quirks
                .iter()
                .filter(|quirk| NamedQuirk::of(quirk).is_some())
                .count();
            quirks_found += scripted
                .quirks
                .iter()
                .filter(|quirk| !quirk.plays.is_empty())
                .count();
            transitions += pose.transitions.len();
            transitions_found += scripted.transitions.len();
            battle_poses += usize::from(pose.conditions.battle == Some(true));
        }

        // The model through a minute of standing about, then a battle with a cry and an attack in
        // it: twenty frames a second, its quirks drawing their numbers from a counter.
        let mut counter = 0u32;
        let mut random = || {
            counter = counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (counter >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut playing = Playing::default();
        let stand = Entity::default();
        let fighting = Entity {
            battle: true,
            ..stand
        };
        let mut had_quirk = false;
        for frame in 0..1200 {
            playing.frame(&script, &stand, frame as f32 / 20.0, &mut random);
            had_quirk |= playing.quirk_animations().next().is_some();
        }
        quirked += usize::from(had_quirk);
        let before = playing.pose;
        let mut blended = false;
        for frame in 1200..1400 {
            let now = frame as f32 / 20.0;
            if frame == 1240 && playing.play(&script, &["cry".to_owned()], now) {
                cries += 1;
            }
            if frame == 1300 {
                let wanted = ["tackle", "physical", "special", "status"].map(str::to_owned);
                played += usize::from(playing.play(&script, &wanted, now));
            }
            playing.frame(&script, &fighting, now, &mut random);
            blended |= matches!(
                playing.primary.as_ref().map(|primary| &primary.kind),
                Some(PrimaryKind::Blend { .. })
            );
            // Whatever plays, the pose is one of the poser's.
            assert!(
                playing.pose.is_none_or(|pose| pose < script.poses.len()),
                "{name}"
            );
            assert!(playing.pose_intensity.is_finite(), "{name}");
        }
        blends += usize::from(blended);
        // Ten seconds into the battle it is in a pose that fits one, or in the one a model that
        // fits none is put in, unless something primary still plays.
        if let Some(pose) = playing.pose {
            assert!(
                script.fits(pose, &fighting)
                    || script.wanted(&fighting) == Some(pose)
                    || playing.primary.is_some(),
                "{name}: in pose {pose} in a battle, from {before:?}"
            );
        }
        // An entity that stands is in a pose once it was seen.
        assert_eq!(playing.pose.is_some(), !script.poses.is_empty(), "{name}");
    }

    println!("{posers} posers ({unread} not read)");
    println!(
        "named animations: {named} written, {named_meant} name an active animation, {named_found} found in their files"
    );
    println!(
        "quirks: {quirks} written ({legacy} as objects), {quirks_meant} of the others name animations, {quirks_found} have one to play"
    );
    println!("transitions: {transitions} written, {transitions_found} found");
    println!("{battle_poses} poses for battles; entering a battle blended {blends} models");
    println!(
        "{cries} models cried, {played} played an attack, {quirked} had a quirk within a minute"
    );
    for line in &lost {
        println!("  not an active animation: {line}");
    }
    assert!(posers > 500, "{posers} posers");
    // Nearly every animation a poser names is one to start, and is in its file.
    assert!(named_meant * 100 >= named * 97, "{named_meant} of {named}");
    assert!(
        named_found * 100 >= named_meant * 97,
        "{named_found} of {named_meant}"
    );
    assert!(
        quirks_found * 100 >= quirks * 95,
        "{quirks_found} of {quirks}"
    );
    assert!(
        transitions_found * 100 >= transitions * 80,
        "{transitions_found} of {transitions}"
    );
    // Seven posers in ten have a cry: the others are a server's own, written without one.
    assert!(cries * 100 >= posers * 60, "{cries} of {posers}");
}
