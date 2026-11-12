// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon 1.8.1's own Pokémon animations, read and sampled whole. They are not in this repository:
//! point `COBBLEMON_ASSETS` at a checkout's `common/src/main/resources/assets/cobblemon` and run with
//! `-- --ignored --nocapture`. Several folders stack (see `common::roots`).
use cobblelegacy_cobblemon::{
    animation::{Animation, AnimationFile, Census},
    molang::Context,
};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

mod common;
use common::files;

/// How many numbers `json` writes with an exponent, outside its strings: the one spelling this crate
/// reads differently from Cobblemon (see `animation`'s description).
fn numbers_with_an_exponent(json: &str) -> usize {
    let bytes = json.as_bytes();
    let (mut in_string, mut found, mut index) = (false, 0, 0);
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if in_string => index += 1,
            b'"' => in_string = !in_string,
            b'e' | b'E' if !in_string && index > 0 && bytes[index - 1].is_ascii_digit() => {
                found += 1;
            }
            _ => {}
        }
        index += 1;
    }
    found
}

/// The entries of `counts` whose name starts with `prefix`, the most frequent first, as one line.
fn listed(counts: &BTreeMap<String, usize>, prefix: &str) -> String {
    let mut entries: Vec<_> = counts
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .collect();
    entries.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let entries: Vec<_> = entries
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    format!("{} ({})", entries.len(), entries.join(", "))
}

#[derive(Clone, Copy, Default)]
struct Sampled {
    samples: u64,
    /// Bones an animation moved.
    deltas: u64,
    /// Samples past a length that ends the animation, which move nothing.
    over: u64,
}

/// Samples `animation` at `seconds`, as an animation of a pose: the state's seconds are the animation's.
/// A number that is not finite is a failure.
fn sample(
    path: &Path,
    animation: &Animation,
    seconds: f32,
    sampled: &mut Sampled,
    failures: &mut Vec<String>,
) {
    let context = Context {
        anim_time: seconds,
        age_in_ticks: seconds * 20.0,
        limb_swing: 0.6,
        limb_swing_amount: 0.25,
    };
    sampled.samples += 1;
    sampled.over += u64::from(animation.is_over(seconds));
    let mut called = vec![false; animation.bones.len()];
    animation.sample(seconds, &context, &mut |bone, delta| {
        sampled.deltas += 1;
        assert!(
            !std::mem::replace(&mut called[bone], true),
            "{}: bone {bone} twice",
            animation.name
        );
        let mut numbers = delta
            .position
            .iter()
            .chain(&delta.rotation)
            .chain(&delta.scale);
        if !numbers.all(|number| number.is_finite()) && failures.len() < 50 {
            failures.push(format!(
                "{}: {} at {seconds}, bone {}: {delta:?}",
                path.display(),
                animation.name,
                animation.bones[bone].bone
            ));
        }
    });
}

#[test]
#[ignore = "needs COBBLEMON_ASSETS, the path of Cobblemon 1.8.1's assets/cobblemon"]
fn every_pokemon_animation_of_cobblemon_reads_and_samples() {
    let mut failures = Vec::new();

    let paths = files("bedrock/pokemon/animations", ".animation.json", false);
    let mut census = Census::default();
    let mut read = Vec::new();
    let (mut parsing, mut bytes, mut exponents) = (Duration::ZERO, 0, 0);
    for path in &paths {
        let json = fs::read_to_string(path).unwrap();
        bytes += json.len();
        exponents += numbers_with_an_exponent(&json);
        let started = Instant::now();
        let file = AnimationFile::parse_counting(&json, &mut census);
        parsing += started.elapsed();
        match file {
            Ok(file) => read.push((path, file)),
            Err(e) => failures.push(format!("{}: {}", path.display(), e.0)),
        }
    }

    let animations = || read.iter().flat_map(|(_, file)| &file.animations);
    let (mut bones, mut wrapping, mut ending, mut endless, mut loop_without_length) =
        (0, 0, 0, 0, 0);
    for animation in animations() {
        bones += animation.bones.len();
        let positive = animation.length.is_some_and(|length| length > 0.0);
        match (animation.wraps(), positive) {
            (true, _) => wrapping += 1,
            (false, true) => ending += 1,
            (false, false) => endless += 1,
        }
        loop_without_length += usize::from(animation.looping && !positive);
    }
    println!(
        "files: {} ({bytes} bytes), animations: {}, bones named: {bones}",
        paths.len(),
        animations().count()
    );
    println!(
        "time: {wrapping} animations wrap at their length, {ending} end at it, {endless} have none \
         and go on ({loop_without_length} of those say `loop`)"
    );
    println!(
        "channels: {} of numbers, {} with MoLang, {} keyframed ({} keyframes)",
        census.number_channels, census.molang_channels, census.keyframed_channels, census.keyframes
    );
    println!(
        "components: {} numbers, {} MoLang expressions, of which {} are constant once compiled; \
         {exponents} numbers written with an exponent",
        census.numbers, census.expressions, census.constant_expressions
    );
    println!("queries: {}", listed(&census.names, "query."));
    println!("math: {}", listed(&census.names, "math."));
    println!("variables: {}", listed(&census.names, "variable."));
    let others: BTreeMap<String, usize> = census
        .names
        .iter()
        .filter(|(name, _)| {
            !["query.", "math.", "variable."]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .map(|(name, count)| (name.clone(), *count))
        .collect();
    println!("other names: {}", listed(&others, ""));
    println!(
        "skipped keys of an animation: {}",
        listed(&census.ignored_animation_keys, "")
    );
    println!(
        "skipped keys of a bone: {}",
        listed(&census.ignored_bone_keys, "")
    );

    let mut sources: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for unparsed in &census.unparsed {
        sources
            .entry(&unparsed.source)
            .or_insert((0, &unparsed.error))
            .0 += 1;
    }
    let mut sources: Vec<_> = sources.into_iter().collect();
    sources.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    println!(
        "components that did not compile: {} ({} distinct)",
        census.unparsed.len(),
        sources.len()
    );
    for (source, (count, error)) in sources.iter().take(10) {
        println!("  {count} x {source:?}: {error}");
    }

    // Every animation at a few moments: one ordinary moment first, timed; then its start, another
    // ordinary one, and around its end.
    let mut sampled = Sampled::default();
    let started = Instant::now();
    for (path, file) in &read {
        for animation in &file.animations {
            sample(path, animation, 0.37, &mut sampled, &mut failures);
        }
    }
    let sampling = started.elapsed();
    let once = sampled;
    for (path, file) in &read {
        for animation in &file.animations {
            for seconds in [0.0, 1.0] {
                sample(path, animation, seconds, &mut sampled, &mut failures);
            }
            if let Some(length) = animation.length {
                for seconds in [length - 0.001, length, length + 0.001] {
                    sample(path, animation, seconds, &mut sampled, &mut failures);
                }
            }
        }
    }
    let Sampled {
        samples,
        deltas,
        over,
    } = sampled;
    println!(
        "sampled {samples} times, {deltas} bones moved, {over} times past a length that ends the \
         animation"
    );
    println!(
        "took: {parsing:?} to read every file, {sampling:?} to sample every animation once ({} \
         animations, {} bones moved)",
        once.samples, once.deltas
    );

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(exponents, 0);
}
