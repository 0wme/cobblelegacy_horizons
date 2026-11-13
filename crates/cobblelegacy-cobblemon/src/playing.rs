// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a model plays beyond its pose's own animations, and how it goes from one pose to the next:
//! the part of Cobblemon's `PosableState` and `PosableModel.applyAnimations` that remembers something
//! of one entity.
//!
//! - An animation the server names (`PlayPosableAnimationPacket`: a cry, an attack, a faint) starts
//!   and runs to its end ([`Playing::play`]). One made by `q.bedrock_stateful` plays over the pose;
//!   one made by `q.bedrock_primary` takes the pose's place for as long as it lasts, the pose fading
//!   out and back in along its [`Curve`].
//! - A quirk (a blink) starts on its own now and then, while nothing primary plays.
//! - A change of pose is not a cut: the old pose's transition for the new one plays, or the two
//!   blend for a few ticks, and only then is the new pose the entity's.
//!
//! Nothing here draws or reads a file. A [`Script`] is what a poser asks for, each animation it names
//! found in its file; a [`Playing`] is one entity's state, told the time each frame
//! ([`Playing::frame`]) and read by whoever poses the model.
//!
//! What Cobblemon does and this does not: a name the server sends that is itself MoLang or a
//! `group:animation`, which `PosableModel.getAnimation` also takes; the MoLang a packet runs before
//! its animation; a quirk's `condition` that asks something of the entity (it is answered once, with
//! nothing known of it).
use crate::{
    animation::{Animation, AnimationFile},
    molang,
    poser::{self, Argument, Call, Conditions, Expression, LegacyQuirk, Poser},
    wire::PoseType,
};
use std::sync::Arc;

/// One animation of a file.
#[derive(Clone, Debug)]
pub struct Clip {
    pub file: Arc<AnimationFile>,
    /// Its place in the file's `animations`.
    pub index: usize,
}

impl PartialEq for Clip {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.file, &other.file) && self.index == other.index
    }
}

impl Clip {
    /// `animation.<group>.<name>` of the group `load` reads (`PosableModel.bedrockStateful`).
    pub fn find(
        load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>,
        group: &str,
        name: &str,
    ) -> Option<Self> {
        let file = load(group)?;
        let wanted = format!("animation.{group}.{name}");
        let index = file.animations.iter().position(|a| a.name == wanted)?;
        Some(Self { file, index })
    }

    pub fn animation(&self) -> &Animation {
        &self.file.animations[self.index]
    }

    /// What tells it from every other animation that is loaded.
    pub fn id(&self) -> (usize, usize) {
        (Arc::as_ptr(&self.file) as usize, self.index)
    }

    /// `BedrockActiveAnimation.duration`: the animation's length in seconds, -1 for one that gives
    /// none.
    pub fn duration(&self) -> f32 {
        self.animation().length.unwrap_or(-1.0)
    }
}

/// How much of a primary animation shows as it goes from its start (0) to its end (1): one of
/// `WaveFunctions`, or what a primary animation has without a `q.curve`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Without a curve: in over the first tenth, then whole to the end.
    Rise,
    /// `symmetrical`: in over the first tenth and out over the last.
    Symmetrical,
    /// `symmetrical_wide`: in over the first fifth and out over the last, eased.
    SymmetricalWide,
    /// `one`: whole all along.
    One,
}

impl Curve {
    /// `q.curve('<name>')`, None for a name Cobblemon throws at.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "symmetrical" => Self::Symmetrical,
            "symmetrical_wide" => Self::SymmetricalWide,
            "one" => Self::One,
            _ => return None,
        })
    }

    pub fn at(self, t: f32) -> f32 {
        let eased = |t: f32| 0.5 * (std::f32::consts::PI * (t - 0.5)).sin() + 0.5;
        match self {
            Self::Rise if t < 0.1 => t * 10.0,
            Self::Rise => 1.0,
            Self::Symmetrical if t < 0.1 => t * 10.0,
            Self::Symmetrical if t < 0.9 => 1.0,
            Self::Symmetrical => 1.0 - (t - 0.9) * 10.0,
            Self::SymmetricalWide if t < 0.2 => eased(t * 5.0),
            Self::SymmetricalWide if t < 0.8 => 1.0,
            Self::SymmetricalWide => eased((1.0 - t) * 5.0),
            Self::One => 1.0,
        }
    }
}

/// What makes an animation a `PrimaryAnimation`.
#[derive(Clone, Debug, PartialEq)]
pub struct Primary {
    /// The labels of the pose's animations that go on at full strength under it (`look`: the head
    /// still follows), `all` for every one of them.
    pub excluded_labels: Vec<String>,
    pub curve: Curve,
}

impl Primary {
    /// What `q.bedrock_primary` reads after the animation's group and name: a label as a string, a
    /// `q.curve(..)`, a `q.exclude_labels(..)`. None for what Cobblemon cannot make an animation of.
    fn of(arguments: &[Argument]) -> Option<Self> {
        let mut primary = Self {
            excluded_labels: Vec::new(),
            curve: Curve::Rise,
        };
        for argument in arguments {
            match argument {
                Argument::String(label) => primary.excluded_labels.push(label.clone()),
                Argument::Call(call) if call.function == "curve" => {
                    let [Argument::String(name)] = &call.arguments[..] else {
                        return None;
                    };
                    primary.curve = Curve::named(name)?;
                }
                Argument::Call(call) if call.function == "exclude_labels" => {
                    primary
                        .excluded_labels
                        .extend(call.arguments.iter().filter_map(text));
                }
                Argument::Number(_) => {}
                Argument::Call(_) | Argument::Molang(_) => return None,
            }
        }
        Some(primary)
    }

    /// `PrimaryAnimation.prevents`: whether it takes the place of a pose's animation that has
    /// `labels`.
    pub fn prevents(&self, labels: &[&str]) -> bool {
        prevents(&self.excluded_labels, labels)
    }
}

fn prevents(excluded: &[String], labels: &[&str]) -> bool {
    !excluded
        .iter()
        .any(|excluded| excluded == "all" || labels.contains(&excluded.as_str()))
}

fn text(argument: &Argument) -> Option<String> {
    match argument {
        Argument::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// The animation MoLang names for a model to play: what `q.bedrock_stateful` and `q.bedrock_primary`
/// make, before its file is looked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub group: String,
    pub name: String,
    /// `endures_primary_animations`: a primary animation that starts does not end it.
    pub endures_primary: bool,
    pub primary: Option<Primary>,
}

impl Named {
    /// None for what gives Cobblemon no active animation: `q.bedrock`, whose animation is a pose's
    /// and not one to start; any other function; arguments that are not written out.
    pub fn of(expression: &Expression) -> Option<Self> {
        let Expression::Call(Call {
            function,
            arguments,
        }) = expression
        else {
            return None;
        };
        let (Argument::String(group), Argument::String(name)) =
            (arguments.first()?, arguments.get(1)?)
        else {
            return None;
        };
        let (group, name) = (group.clone(), name.clone());
        match function.as_str() {
            "bedrock_stateful" => Some(Self {
                group,
                name,
                endures_primary: arguments
                    .iter()
                    .any(|a| matches!(a, Argument::String(s) if s == "endures_primary_animations")),
                primary: None,
            }),
            "bedrock_primary" => Some(Self {
                group,
                name,
                endures_primary: false,
                primary: Some(Primary::of(&arguments[2..])?),
            }),
            _ => None,
        }
    }

    /// One of a [`LegacyQuirk`]'s animations: MoLang as above, or `bedrock(<group>, <name>)` without
    /// quotes (`BedrockAnimationReferenceFactory.active`).
    fn legacy(source: &str) -> Option<Self> {
        if let Some(named) = Self::of(&Expression::parse(source)) {
            return Some(named);
        }
        if source.split('(').next() != Some("bedrock") {
            return None;
        }
        let inside = source.replace("bedrock(", "").replace(')', "");
        let mut parts = inside.split(',').map(str::trim);
        Some(Self {
            group: parts.next()?.to_owned(),
            name: parts.next()?.to_owned(),
            endures_primary: false,
            primary: None,
        })
    }
}

/// A quirk as MoLang writes it (`q.bedrock_quirk`, `q.bedrock_primary_quirk`), before its
/// animations are looked for.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedQuirk {
    pub group: String,
    /// One of these plays each time, picked at random.
    pub names: Vec<String>,
    /// The least and the most seconds from one occurrence to the next (8 and 30 without them).
    pub seconds: (f32, f32),
    /// It plays from once to this many times in a row.
    pub loop_times: i32,
    pub primary: Option<Primary>,
}

impl NamedQuirk {
    pub fn of(expression: &Expression) -> Option<Self> {
        let Expression::Call(Call {
            function,
            arguments,
        }) = expression
        else {
            return None;
        };
        let primary = match function.as_str() {
            "bedrock_quirk" => false,
            "bedrock_primary_quirk" => true,
            _ => return None,
        };
        let group = text(arguments.first()?)?;
        let names = match arguments.get(1) {
            Some(Argument::String(name)) => vec![name.clone()],
            Some(Argument::Call(call)) if call.function == "array" => {
                call.arguments.iter().filter_map(text).collect()
            }
            None => Vec::new(),
            Some(_) => return None,
        };
        let number = |index: usize, default: f32| match arguments.get(index) {
            None => Some(default),
            Some(Argument::Number(number)) => Some(*number as f32),
            Some(_) => None,
        };
        Some(Self {
            group,
            names,
            seconds: (number(2, 8.0)?, number(3, 30.0)?),
            loop_times: number(4, 1.0)? as i32,
            primary: if primary {
                Some(Primary::of(arguments.get(5..).unwrap_or(&[]))?)
            } else {
                None
            },
        })
    }
}

/// An animation ready to start: its [`Clip`], and what kind of animation it starts as.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub clip: Clip,
    pub endures_primary: bool,
    pub primary: Option<Primary>,
}

impl Play {
    fn find(
        load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>,
        named: &Named,
    ) -> Option<Self> {
        Some(Self {
            clip: Clip::find(load, &named.group, &named.name)?,
            endures_primary: named.endures_primary,
            primary: named.primary.clone(),
        })
    }
}

/// A quirk of a pose (`SimpleQuirk`), its animations found.
#[derive(Clone, Debug, PartialEq)]
pub struct Quirk {
    /// What it plays each time: one of these at random, or all of them together.
    pub plays: Vec<Play>,
    pub together: bool,
    pub seconds: (f32, f32),
    pub loop_times: i32,
    /// Whether it starts at all: its `condition`, answered with nothing known of the entity.
    pub runs: bool,
}

/// One pose of a [`Script`].
#[derive(Clone, Debug, PartialEq)]
pub struct PoseScript {
    pose_types: Vec<PoseType>,
    /// The pose's conditions but its MoLang, which `molang_holds` answers.
    conditions: Conditions,
    molang_holds: bool,
    /// `namedAnimations` that resolve.
    pub named: Vec<(String, Play)>,
    pub quirks: Vec<Quirk>,
    /// `transitions`: the place of another pose, and what plays on the way to it.
    pub transitions: Vec<(usize, Play)>,
    pub transform_ticks: u32,
    pub transform_to_ticks: Option<u32>,
}

/// What a poser has a model play and when, each animation found in its file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    /// In the poser's order.
    pub poses: Vec<PoseScript>,
    /// The poser's own `animations` that resolve.
    pub named: Vec<(String, Play)>,
}

/// A MoLang source's value with nothing known of the entity. 0 for one that does not parse.
fn value(source: &str) -> f64 {
    molang::Expression::parse(source)
        .map_or(0.0, |e| f64::from(e.eval(&molang::Context::default())))
}

impl Script {
    /// `load` reads a Bedrock animation group (`pikachu` for `pikachu.animation.json`). What names
    /// an animation that is not there is left out, as an expression Cobblemon fails to resolve is.
    pub fn new(poser: &Poser, load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>) -> Self {
        let mut named = |entries: &[(String, Expression)]| -> Vec<(String, Play)> {
            entries
                .iter()
                .filter_map(|(name, expression)| {
                    Some((name.clone(), Play::find(load, &Named::of(expression)?)?))
                })
                .collect()
        };
        let own = named(&poser.named_animations);
        let pose_named: Vec<_> = poser
            .poses
            .iter()
            .map(|pose| (named(&pose.named_animations), named(&pose.transitions)))
            .collect();
        let poses = poser
            .poses
            .iter()
            .zip(pose_named)
            .map(|(pose, (named, transitions))| {
                let mut quirks: Vec<Quirk> = pose
                    .quirks
                    .iter()
                    .filter_map(NamedQuirk::of)
                    .map(|quirk| Quirk {
                        plays: quirk
                            .names
                            .iter()
                            .filter_map(|name| {
                                let named = Named {
                                    group: quirk.group.clone(),
                                    name: name.clone(),
                                    endures_primary: false,
                                    primary: quirk.primary.clone(),
                                };
                                Play::find(load, &named)
                            })
                            .collect(),
                        together: false,
                        seconds: quirk.seconds,
                        loop_times: quirk.loop_times,
                        runs: true,
                    })
                    .collect();
                quirks.extend(pose.legacy_quirks.iter().map(|quirk| legacy(load, quirk)));
                let mut conditions = pose.conditions.clone();
                let molang_holds = conditions
                    .molang
                    .take()
                    .is_none_or(|sources| sources.iter().any(|source| value(source) == 1.0));
                PoseScript {
                    pose_types: pose.pose_types.clone(),
                    conditions,
                    molang_holds,
                    named,
                    quirks,
                    transitions: transitions
                        .into_iter()
                        .filter_map(|(to, play)| {
                            Some((poser.poses.iter().position(|p| p.name == to)?, play))
                        })
                        .collect(),
                    transform_ticks: pose.transform_ticks,
                    transform_to_ticks: pose.transform_to_ticks,
                }
            })
            .collect();
        Self { poses, named: own }
    }

    /// `Pose.isSuitable` and the entity's pose type being one of the pose's.
    pub fn fits(&self, pose: usize, entity: &poser::Entity) -> bool {
        self.poses.get(pose).is_some_and(|pose| {
            pose.pose_types.contains(&entity.pose)
                && pose.molang_holds
                && pose.conditions.hold(entity, &mut |_| 0.0)
        })
    }

    /// `getFirstSuitablePose`: the first pose the entity fits, the poser's first when it fits none.
    pub fn wanted(&self, entity: &poser::Entity) -> Option<usize> {
        (0..self.poses.len())
            .find(|&pose| self.fits(pose, entity))
            .or((!self.poses.is_empty()).then_some(0))
    }

    /// `PosableModel.getAnimation` for the first of `names` that gives one
    /// (`PosableState.addFirstAnimation`), the model being in the pose at `pose`: that pose's own
    /// before the poser's.
    pub fn named(&self, pose: Option<usize>, names: &[String]) -> Option<&Play> {
        fn find<'a>(named: &'a [(String, Play)], name: &str) -> Option<&'a Play> {
            // The last of a name, as a map keeps it.
            let entry = named.iter().rev().find(|(known, _)| known == name);
            entry.map(|(_, play)| play)
        }
        let pose = pose.and_then(|pose| self.poses.get(pose));
        names.iter().find_map(|name| {
            pose.and_then(|pose| find(&pose.named, name))
                .or_else(|| find(&self.named, name))
        })
    }

    /// Every animation it may start, for whoever binds them to a model's parts.
    pub fn clips(&self) -> impl Iterator<Item = &Clip> {
        let of_poses = self.poses.iter().flat_map(|pose| {
            let named = pose.named.iter().map(|(_, play)| play);
            let transitions = pose.transitions.iter().map(|(_, play)| play);
            let quirks = pose.quirks.iter().flat_map(|quirk| &quirk.plays);
            named.chain(transitions).chain(quirks)
        });
        self.named
            .iter()
            .map(|(_, play)| play)
            .chain(of_poses)
            .map(|play| &play.clip)
    }
}

fn legacy(load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>, quirk: &LegacyQuirk) -> Quirk {
    Quirk {
        plays: quirk
            .animations
            .iter()
            .filter_map(|source| Play::find(load, &Named::legacy(source)?))
            .collect(),
        together: true,
        seconds: quirk.seconds,
        loop_times: quirk.loop_times,
        runs: quirk
            .condition
            .as_deref()
            .is_none_or(|condition| value(condition) != 0.0),
    }
}

/// An animation that plays over the pose (`BedrockActiveAnimation`).
#[derive(Clone, Debug, PartialEq)]
pub struct ActivePlaying {
    pub clip: Clip,
    /// `startedSeconds`: the state's seconds when it started.
    pub started: f32,
    pub endures_primary: bool,
}

impl ActivePlaying {
    fn over(&self, now: f32) -> bool {
        self.clip.animation().is_over(now - self.started)
    }
}

/// What a primary animation runs.
#[derive(Clone, Debug, PartialEq)]
pub enum PrimaryKind {
    Clip(Clip),
    /// `PoseTransitionAnimation`: the pose at `before` fades into the one at `after`.
    Blend {
        before: usize,
        after: usize,
    },
}

/// The primary animation that plays (`PrimaryAnimation`).
#[derive(Clone, Debug, PartialEq)]
pub struct PrimaryPlaying {
    pub kind: PrimaryKind,
    pub started: f32,
    /// In seconds; -1 for an animation without a length, which ends as it starts.
    pub duration: f32,
    pub curve: Curve,
    pub excluded_labels: Vec<String>,
    /// The pose the entity is in once it ends: where a transition leads.
    pub then: Option<usize>,
}

impl PrimaryPlaying {
    /// For a [`PrimaryKind::Blend`], how much of the new pose shows at `now`, the rest being the
    /// old one's (`PoseTransitionAnimation.run`: a half sine from 0 to 1).
    pub fn blend(&self, now: f32) -> f32 {
        let ratio = ((now - self.started) / self.duration).min(1.0);
        (0.5 * (std::f32::consts::PI * (ratio - 0.5)).sin() + 0.5).clamp(0.0, 1.0)
    }

    fn prevents(&self, labels: &[&str]) -> bool {
        prevents(&self.excluded_labels, labels)
    }
}

/// What a quirk is up to for one entity (`SimpleQuirkData`).
#[derive(Clone, Debug, PartialEq)]
struct QuirkPlaying {
    animations: Vec<ActivePlaying>,
    remaining_loops: i32,
    /// The seconds of its next occurrence, -1 until one is picked.
    next: f32,
}

impl Default for QuirkPlaying {
    fn default() -> Self {
        Self {
            animations: Vec::new(),
            remaining_loops: 0,
            next: -1.0,
        }
    }
}

/// One entity's animation state (`PosableState`).
#[derive(Clone, Debug, PartialEq)]
pub struct Playing {
    /// `currentPose`, by its place in the poser's. During a transition it is still the pose that is
    /// being left.
    pub pose: Option<usize>,
    pub primary: Option<PrimaryPlaying>,
    /// `activeAnimations`.
    pub active: Vec<ActivePlaying>,
    /// One for each quirk of the current pose.
    quirks: Vec<QuirkPlaying>,
    /// `poseIntensity`: how much of the pose's animations a primary animation leaves.
    pub pose_intensity: f32,
}

impl Default for Playing {
    fn default() -> Self {
        Self {
            pose: None,
            primary: None,
            active: Vec::new(),
            quirks: Vec::new(),
            pose_intensity: 1.0,
        }
    }
}

impl Playing {
    /// A state in the pose at `pose`, with nothing played: a model no entity wears.
    pub fn posed(pose: Option<usize>) -> Self {
        Self {
            pose,
            ..Self::default()
        }
    }

    /// `PosableState.addFirstAnimation`: the first of `names` the model has an animation for starts
    /// at `now`. False when it has none.
    pub fn play(&mut self, script: &Script, names: &[String], now: f32) -> bool {
        let Some(play) = script.named(self.pose, names) else {
            return false;
        };
        self.start(play, now, None);
        true
    }

    /// What `PokemonClientDelegate` does of `DYING_EFFECTS_STARTED`: the model's `faint` starts at
    /// `now` as a primary animation, whatever it was written as, on the curve one has without one
    /// (the animation's own is not read). False for a model without a `faint`.
    pub fn faint(&mut self, script: &Script, now: f32) -> bool {
        let Some(play) = script.named(self.pose, &["faint".to_owned()]) else {
            return false;
        };
        self.add_primary(PrimaryPlaying {
            kind: PrimaryKind::Clip(play.clip.clone()),
            started: now,
            duration: play.clip.duration(),
            curve: Curve::Rise,
            excluded_labels: Vec::new(),
            then: None,
        });
        true
    }

    fn start(&mut self, play: &Play, now: f32, then: Option<usize>) {
        match &play.primary {
            Some(primary) => self.add_primary(PrimaryPlaying {
                kind: PrimaryKind::Clip(play.clip.clone()),
                started: now,
                duration: play.clip.duration(),
                curve: primary.curve,
                excluded_labels: primary.excluded_labels.clone(),
                then,
            }),
            None => self.active.push(ActivePlaying {
                clip: play.clip.clone(),
                started: now,
                endures_primary: play.endures_primary,
            }),
        }
    }

    /// `addPrimaryAnimation`: it takes the place of the one that played, ends what does not endure
    /// it, and every quirk starts over.
    fn add_primary(&mut self, primary: PrimaryPlaying) {
        self.primary = Some(primary);
        self.active.retain(|active| active.endures_primary);
        self.quirks.clear();
        self.pose_intensity = 1.0;
    }

    fn set_pose(&mut self, pose: Option<usize>) {
        if self.pose != pose {
            // A pose's quirks are its own.
            self.quirks.clear();
        }
        self.pose = pose;
        self.pose_intensity = 1.0;
    }

    /// `moveToPose`: from the pose at `from` towards the one at `to`, by the transition the first
    /// has for the second, or by a blend, or at once.
    fn move_to(&mut self, script: &Script, from: usize, to: usize, now: f32) {
        let before = &script.poses[from];
        let transition = before
            .transitions
            .iter()
            .rev()
            .find(|(target, _)| *target == to);
        match transition {
            Some((_, play)) => match &play.primary {
                Some(_) => self.start(play, now, Some(to)),
                // One that is not primary is made so, whole all along.
                None => self.add_primary(PrimaryPlaying {
                    kind: PrimaryKind::Clip(play.clip.clone()),
                    started: now,
                    duration: play.clip.duration(),
                    curve: Curve::One,
                    excluded_labels: Vec::new(),
                    then: Some(to),
                }),
            },
            None if before.transform_ticks > 0 => {
                let ticks = script.poses[to]
                    .transform_to_ticks
                    .unwrap_or(before.transform_ticks);
                self.add_primary(PrimaryPlaying {
                    kind: PrimaryKind::Blend {
                        before: from,
                        after: to,
                    },
                    started: now,
                    duration: ticks as f32 / 20.0,
                    curve: Curve::One,
                    excluded_labels: Vec::new(),
                    then: Some(to),
                });
            }
            None => self.set_pose(Some(to)),
        }
    }

    /// One frame, at `now` seconds of the state's own time (`PosableState.animationSeconds`), for
    /// an entity as `entity` has it: what `applyAnimations` and `incrementAge` change of the state.
    /// `random` gives numbers from 0 to 1, 1 excluded.
    ///
    /// The pose is kept for as long as the entity fits it (`validatePose`); when it no longer
    /// does, and no primary animation plays, the entity moves to the first pose it fits. Quirks
    /// run while nothing primary plays. A primary animation ends at its length, and the pose it led
    /// to is then the entity's; an active one is dropped once it is over.
    pub fn frame(
        &mut self,
        script: &Script,
        entity: &poser::Entity,
        now: f32,
        random: &mut dyn FnMut() -> f32,
    ) {
        match self.pose.filter(|&pose| pose < script.poses.len()) {
            Some(current) => {
                if self.primary.is_none()
                    && !script.fits(current, entity)
                    && let Some(wanted) = script.wanted(entity)
                    && wanted != current
                {
                    self.move_to(script, current, wanted, now);
                }
            }
            None => self.set_pose(script.wanted(entity)),
        }
        if self.primary.is_none()
            && let Some(pose) = self.pose.and_then(|pose| script.poses.get(pose))
        {
            self.quirks_frame(pose, now, random);
        }
        if let Some(primary) = &self.primary {
            let passed = now - primary.started;
            let over = match &primary.kind {
                PrimaryKind::Clip(clip) => clip.animation().is_over(passed),
                PrimaryKind::Blend { .. } => false,
            };
            if over || primary.started + primary.duration <= now {
                let then = primary.then;
                self.primary = None;
                self.pose_intensity = 1.0;
                if then.is_some() {
                    self.set_pose(then);
                }
            } else {
                self.pose_intensity = 1.0 - primary.curve.at(passed / primary.duration);
            }
        }
        self.active.retain(|active| !active.over(now));
    }

    /// `SimpleQuirk.apply` and `QuirkData.run` for each quirk of the pose.
    fn quirks_frame(&mut self, pose: &PoseScript, now: f32, random: &mut dyn FnMut() -> f32) {
        self.quirks
            .resize_with(pose.quirks.len(), QuirkPlaying::default);
        let mut primary = None;
        for (quirk, data) in pose.quirks.iter().zip(&mut self.quirks) {
            if data.animations.is_empty() && quirk.runs {
                let mut starts = false;
                if data.remaining_loops > 0 {
                    starts = true;
                    data.remaining_loops -= 1;
                }
                if data.remaining_loops == 0 {
                    if data.next > 0.0 {
                        if data.next <= now {
                            let most = quirk.loop_times.max(1) as f32;
                            data.remaining_loops = (random() * most) as i32;
                            starts = true;
                            data.next = -1.0;
                        }
                    } else {
                        let (least, most) = quirk.seconds;
                        data.next = now + random() * (most - least) + least;
                    }
                }
                if starts && !quirk.plays.is_empty() {
                    let one = (random() * quirk.plays.len() as f32) as usize;
                    let plays = if quirk.together {
                        &quirk.plays[..]
                    } else {
                        &quirk.plays[one.min(quirk.plays.len() - 1)..][..1]
                    };
                    for play in plays {
                        match play.primary {
                            Some(_) if primary.is_none() => primary = Some(play),
                            Some(_) => {}
                            None => data.animations.push(ActivePlaying {
                                clip: play.clip.clone(),
                                started: now,
                                endures_primary: false,
                            }),
                        }
                    }
                }
            }
            data.animations.retain(|active| !active.over(now));
            if primary.is_some() {
                break;
            }
        }
        if let Some(play) = primary {
            self.start(play, now, None);
        }
    }

    /// The animations its quirks have started and that play still.
    pub fn quirk_animations(&self) -> impl Iterator<Item = &ActivePlaying> {
        self.quirks.iter().flat_map(|quirk| &quirk.animations)
    }

    /// Every animation that plays beyond the pose's own, each with the seconds it started at: the
    /// active ones, the quirks', and the primary one when it is an animation
    /// (`PosableState.runEffects`).
    pub fn clips(&self) -> impl Iterator<Item = (&Clip, f32)> {
        let primary = self
            .primary
            .iter()
            .filter_map(|primary| match &primary.kind {
                PrimaryKind::Clip(clip) => Some((clip, primary.started)),
                PrimaryKind::Blend { .. } => None,
            });
        self.active
            .iter()
            .chain(self.quirk_animations())
            .map(|active| (&active.clip, active.started))
            .chain(primary)
    }

    /// How much of the primary animation shows.
    pub fn primary_intensity(&self) -> f32 {
        1.0 - self.pose_intensity
    }

    /// What an animation of the pose that has `labels` runs at (`getIdleIntensity`), None when it
    /// does not run at all (`shouldIdleRun`): a primary animation that does not exclude it leaves
    /// it the pose's intensity, and nothing at none.
    pub fn idle(&self, labels: &[&str]) -> Option<f32> {
        match &self.primary {
            Some(primary) if primary.prevents(labels) => {
                (self.pose_intensity > 0.0).then_some(self.pose_intensity)
            }
            _ => Some(1.0),
        }
    }

    /// Whether an animation of the pose that has `labels` starts its effects (`runEffects`): not
    /// under a primary animation that leaves it half its intensity or less.
    pub fn idle_emits(&self, labels: &[&str]) -> bool {
        match &self.primary {
            Some(primary) => !primary.prevents(labels) || self.pose_intensity > 0.5,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ANIMATIONS: &str = r#"{
        "format_version": "1.8.0",
        "animations": {
            "animation.testmon.ground_idle": {"loop": true, "animation_length": 2.0, "bones": {}},
            "animation.testmon.battle_idle": {"loop": true, "animation_length": 2.0, "bones": {}},
            "animation.testmon.sleep": {"loop": true, "animation_length": 4.0, "bones": {}},
            "animation.testmon.cry": {"animation_length": 1.0, "bones": {}},
            "animation.testmon.battle_cry": {"animation_length": 1.5, "bones": {}},
            "animation.testmon.physical": {"animation_length": 2.0, "bones": {}},
            "animation.testmon.faint": {"animation_length": 3.0, "bones": {}},
            "animation.testmon.blink": {"animation_length": 0.25, "bones": {}},
            "animation.testmon.yawn": {"animation_length": 1.0, "bones": {}},
            "animation.testmon.stretch": {"animation_length": 2.0, "bones": {}},
            "animation.testmon.battle_intro": {"animation_length": 1.0, "bones": {}},
            "animation.testmon.endless": {"bones": {}}
        }
    }"#;

    const POSER: &str = r#"{
        "animations": {
            "cry": "q.bedrock_stateful('testmon', 'cry')",
            "physical": "q.bedrock_primary('testmon', 'physical', 'look', q.curve('symmetrical_wide'))",
            "faint": "q.bedrock_primary('testmon', 'faint', q.curve('one'))",
            "missing": "q.bedrock_stateful('testmon', 'nowhere')",
            "posed": "q.bedrock('testmon', 'cry')",
            "endless": "q.bedrock_primary('testmon', 'endless')"
        },
        "poses": {
            "battle": {
                "poseTypes": ["STAND"],
                "isBattle": true,
                "animations": ["q.bedrock('testmon', 'battle_idle')"],
                "namedAnimations": {"cry": "q.bedrock_stateful('testmon', 'battle_cry')"},
                "quirks": ["q.bedrock_primary_quirk('testmon', 'stretch', 10, 10, 1, q.curve('symmetrical'))"]
            },
            "standing": {
                "poseTypes": ["STAND", "WALK"],
                "isBattle": false,
                "transformTicks": 10,
                "animations": ["q.bedrock('testmon', 'ground_idle')"],
                "quirks": [
                    "q.bedrock_quirk('testmon', 'blink', 2, 4, 3)",
                    {"name": "yawn", "animations": ["bedrock(testmon, yawn)"], "minSecondsBetweenOccurrences": 20, "maxSecondsBetweenOccurrences": 20}
                ],
                "transitions": {"battle": "q.bedrock_primary('testmon', 'battle_intro', q.curve('one'))"}
            },
            "sleeping": {
                "poseTypes": ["SLEEP"],
                "transformTicks": 0,
                "transformToTicks": 4,
                "animations": ["q.bedrock('testmon', 'sleep')"]
            },
            "resting": {
                "poseTypes": ["SLEEP", "STAND"],
                "animations": ["q.bedrock('testmon', 'sleep')"]
            }
        }
    }"#;

    fn script() -> Script {
        let file = Arc::new(AnimationFile::parse(ANIMATIONS).unwrap());
        let mut files: HashMap<String, Arc<AnimationFile>> = HashMap::new();
        files.insert("testmon".to_owned(), file);
        let poser = Poser::parse(POSER).unwrap();
        Script::new(&poser, &mut |group| files.get(group).cloned())
    }

    fn name(clip: &Clip) -> &str {
        clip.animation()
            .name
            .strip_prefix("animation.testmon.")
            .unwrap()
    }

    fn entity(pose: PoseType, battle: bool) -> poser::Entity {
        poser::Entity {
            pose,
            battle,
            ..poser::Entity::default()
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    /// A state stood in the `standing` pose.
    fn standing(script: &Script) -> Playing {
        let mut playing = Playing::default();
        playing.frame(script, &entity(PoseType::Stand, false), 0.0, &mut || 0.0);
        assert_eq!(playing.pose, Some(1));
        playing
    }

    #[test]
    fn a_curve_fades_a_primary_animation_in_and_out() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        // Without one: in over a tenth, never out.
        assert!(close(Curve::Rise.at(0.05), 0.5) && close(Curve::Rise.at(0.95), 1.0));
        assert!(close(Curve::Symmetrical.at(0.05), 0.5) && close(Curve::Symmetrical.at(0.5), 1.0));
        assert!(close(Curve::Symmetrical.at(0.95), 0.5));
        assert!(close(Curve::SymmetricalWide.at(0.0), 0.0));
        assert!(close(Curve::SymmetricalWide.at(0.1), 0.5));
        assert!(close(Curve::SymmetricalWide.at(0.5), 1.0));
        assert!(close(Curve::SymmetricalWide.at(0.9), 0.5));
        assert!(close(Curve::SymmetricalWide.at(1.0), 0.0));
        assert!(close(Curve::One.at(0.0), 1.0) && close(Curve::One.at(1.0), 1.0));
        assert_eq!(Curve::named("one"), Some(Curve::One));
        assert_eq!(Curve::named("wavy"), None);
    }

    #[test]
    fn molang_names_what_a_model_starts() {
        let of = |source: &str| Named::of(&Expression::parse(source));
        let stateful = of("q.bedrock_stateful('pikachu', 'cry')").unwrap();
        assert_eq!((&*stateful.group, &*stateful.name), ("pikachu", "cry"));
        assert!(!stateful.endures_primary && stateful.primary.is_none());
        assert!(
            of("q.bedrock_stateful('pikachu', 'cry', 'endures_primary_animations')")
                .unwrap()
                .endures_primary
        );
        let primary =
            of("q.bedrock_primary('cufant', 'physical', 'look', q.curve('symmetrical_wide'))")
                .unwrap()
                .primary
                .unwrap();
        assert_eq!(primary.excluded_labels, ["look"]);
        assert_eq!(primary.curve, Curve::SymmetricalWide);
        assert!(!primary.prevents(&["look"]) && primary.prevents(&[]));
        let bare = of("q.bedrock_primary('cufant', 'faint')")
            .unwrap()
            .primary
            .unwrap();
        assert_eq!(bare.curve, Curve::Rise);
        let all = of("q.bedrock_primary('a', 'b', q.exclude_labels('all'))")
            .unwrap()
            .primary
            .unwrap();
        assert!(!all.prevents(&[]));
        // A pose's animation is nothing to start, nor is a curve Cobblemon does not know.
        assert_eq!(of("q.bedrock('pikachu', 'cry')"), None);
        assert_eq!(of("q.bedrock_primary('a', 'b', q.curve('wavy'))"), None);
        assert_eq!(of("q.is_ridden ? q.a('b') : q.c('d')"), None);
        // The form that came before MoLang, in a quirk written as an object.
        let legacy = Named::legacy("bedrock(marill, blink)").unwrap();
        assert_eq!((&*legacy.group, &*legacy.name), ("marill", "blink"));
        assert!(Named::legacy("q.bedrock_stateful('marill', 'blink')").is_some());
        assert_eq!(Named::legacy("look"), None);
    }

    #[test]
    fn molang_names_a_quirk() {
        let of = |source: &str| NamedQuirk::of(&Expression::parse(source));
        let blink = of("q.bedrock_quirk('absol', 'blink')").unwrap();
        assert_eq!(blink.names, ["blink"]);
        assert_eq!((blink.seconds, blink.loop_times), ((8.0, 30.0), 1));
        assert!(blink.primary.is_none());
        let several = of("q.bedrock_quirk('a', q.array('one', 'two'), 2, 5, 3)").unwrap();
        assert_eq!(several.names, ["one", "two"]);
        assert_eq!((several.seconds, several.loop_times), ((2.0, 5.0), 3));
        let primary = of(
            "q.bedrock_primary_quirk('charizard', 'idle_quirk', 20, 40, 1, q.curve('symmetrical'))",
        )
        .unwrap();
        assert_eq!(primary.primary.unwrap().curve, Curve::Symmetrical);
        assert_eq!(of("q.bedrock_stateful('a', 'b')"), None);
    }

    #[test]
    fn a_script_finds_what_a_poser_names() {
        let script = script();
        let named: Vec<&str> = script.named.iter().map(|(name, _)| &**name).collect();
        // One that names no animation, or a pose's, is left out.
        assert_eq!(named, ["cry", "physical", "faint", "endless"]);
        // The pose's own comes before the poser's, and the first name that gives one wins.
        let play = |pose, wanted: &[&str]| {
            script
                .named(pose, &names(wanted))
                .map(|play| name(&play.clip))
        };
        assert_eq!(play(Some(0), &["cry"]), Some("battle_cry"));
        assert_eq!(play(Some(1), &["cry"]), Some("cry"));
        assert_eq!(play(None, &["cry"]), Some("cry"));
        assert_eq!(
            play(Some(1), &["thunderbolt", "missing", "physical", "cry"]),
            Some("physical")
        );
        assert_eq!(play(Some(1), &["thunderbolt"]), None);
        // The poses an entity fits, in the poser's order.
        assert_eq!(script.wanted(&entity(PoseType::Stand, true)), Some(0));
        assert_eq!(script.wanted(&entity(PoseType::Stand, false)), Some(1));
        assert_eq!(script.wanted(&entity(PoseType::Walk, false)), Some(1));
        assert_eq!(script.wanted(&entity(PoseType::Sleep, false)), Some(2));
        // None fits: the first.
        assert_eq!(script.wanted(&entity(PoseType::Fly, false)), Some(0));
        let standing = &script.poses[1];
        assert_eq!(standing.quirks.len(), 2);
        assert!(!standing.quirks[0].together && standing.quirks[1].together);
        assert_eq!(name(&standing.quirks[1].plays[0].clip), "yawn");
        assert_eq!(standing.transitions[0].0, 0);
        assert_eq!(script.clips().count(), 4 + 2 + 3);
    }

    #[test]
    fn a_named_animation_plays_over_the_pose_until_it_ends() {
        let script = script();
        let stand = entity(PoseType::Stand, false);
        let mut playing = standing(&script);
        assert!(!playing.play(&script, &names(&["thunderbolt"]), 1.0));
        assert!(playing.play(&script, &names(&["cry"]), 1.0));
        assert_eq!(playing.active.len(), 1);
        assert_eq!(playing.active[0].started, 1.0);
        // The pose goes on whole under it.
        assert_eq!(playing.idle(&[]), Some(1.0));
        playing.frame(&script, &stand, 1.9, &mut || 0.0);
        assert_eq!(
            playing
                .clips()
                .map(|(clip, _)| name(clip))
                .collect::<Vec<_>>(),
            ["cry"]
        );
        // Its length is one second: past it, it is gone.
        playing.frame(&script, &stand, 2.05, &mut || 0.0);
        assert!(playing.active.is_empty());
    }

    #[test]
    fn a_primary_animation_takes_the_pose_s_place() {
        let script = script();
        let stand = entity(PoseType::Stand, false);
        let mut playing = standing(&script);
        playing.play(&script, &names(&["cry"]), 1.0);
        assert!(playing.play(&script, &names(&["physical"]), 1.0));
        // What does not endure a primary animation ends with its start.
        assert!(playing.active.is_empty());
        let primary = playing.primary.as_ref().unwrap();
        assert_eq!(primary.duration, 2.0);
        // A tenth of the way in, on `symmetrical_wide`: half of it shows, and half of the pose.
        playing.frame(&script, &stand, 1.2, &mut || 0.0);
        assert!((playing.primary_intensity() - 0.5).abs() < 1e-5);
        assert!((playing.idle(&[]).unwrap() - 0.5).abs() < 1e-5);
        // The head still follows: `look` is excluded.
        assert_eq!(playing.idle(&["look"]), Some(1.0));
        assert!(playing.idle_emits(&["look"]) && !playing.idle_emits(&[]));
        // In its middle the pose's animations do not run at all.
        playing.frame(&script, &stand, 2.0, &mut || 0.0);
        assert_eq!(playing.idle(&[]), None);
        assert_eq!(playing.primary_intensity(), 1.0);
        // A cry over it stays; the pose does not change under it, even asleep.
        playing.play(&script, &names(&["cry"]), 2.0);
        playing.frame(&script, &entity(PoseType::Sleep, false), 2.5, &mut || 0.0);
        assert_eq!((playing.pose, playing.active.len()), (Some(1), 1));
        // At its length it is over, and the pose is whole again.
        playing.frame(&script, &stand, 3.0, &mut || 0.0);
        assert!(playing.primary.is_none());
        assert_eq!(playing.idle(&[]), Some(1.0));
        // One without a length ends as it starts.
        playing.play(&script, &names(&["endless"]), 4.0);
        playing.frame(&script, &stand, 4.0, &mut || 0.0);
        assert!(playing.primary.is_none());
        // A faint is primary on the plain curve, whatever curve it was written with: in over its
        // first tenth, and nothing of the pose shows through, not even a look.
        assert!(playing.faint(&script, 5.0));
        let primary = playing.primary.clone().unwrap();
        assert_eq!((primary.curve, primary.duration), (Curve::Rise, 3.0));
        playing.frame(&script, &stand, 5.15, &mut || 0.0);
        assert!((playing.primary_intensity() - 0.5).abs() < 1e-4);
        playing.frame(&script, &stand, 6.0, &mut || 0.0);
        assert_eq!((playing.idle(&[]), playing.idle(&["look"])), (None, None));
        playing.frame(&script, &stand, 8.0, &mut || 0.0);
        assert!(playing.primary.is_none());
    }

    #[test]
    fn a_change_of_pose_blends_or_plays_its_transition() {
        let script = script();
        let mut playing = standing(&script);
        // It stays in its pose for as long as it fits it.
        playing.frame(&script, &entity(PoseType::Walk, false), 1.0, &mut || 0.0);
        assert_eq!((playing.pose, playing.primary.is_some()), (Some(1), false));
        // To sleep: no transition of its own, so the two blend, for the new pose's four ticks.
        let asleep = entity(PoseType::Sleep, false);
        playing.frame(&script, &asleep, 2.0, &mut || 0.0);
        let primary = playing.primary.clone().unwrap();
        assert_eq!(
            primary.kind,
            PrimaryKind::Blend {
                before: 1,
                after: 2
            }
        );
        assert!((primary.duration - 0.2).abs() < 1e-6);
        assert_eq!(playing.pose, Some(1));
        // Neither pose's own animations run beside the blend.
        assert_eq!(playing.idle(&[]), None);
        assert!((primary.blend(2.1) - 0.5).abs() < 1e-5);
        assert!(primary.blend(2.0) < 1e-6 && primary.blend(9.0) > 1.0 - 1e-6);
        playing.frame(&script, &asleep, 2.1, &mut || 0.0);
        assert_eq!(playing.pose, Some(1));
        playing.frame(&script, &asleep, 2.2, &mut || 0.0);
        assert_eq!((playing.pose, playing.primary.is_some()), (Some(2), false));
        // Out of a pose with no ticks to blend for: at once.
        playing.frame(&script, &entity(PoseType::Stand, false), 3.0, &mut || 0.0);
        assert_eq!((playing.pose, playing.primary.is_some()), (Some(1), false));
        // Into battle: the pose's own transition plays, and the battle pose comes after it.
        let fighting = entity(PoseType::Stand, true);
        playing.frame(&script, &fighting, 4.0, &mut || 0.0);
        let primary = playing.primary.clone().unwrap();
        assert!(matches!(&primary.kind, PrimaryKind::Clip(clip) if name(clip) == "battle_intro"));
        assert_eq!((primary.then, playing.pose), (Some(0), Some(1)));
        playing.frame(&script, &fighting, 5.0, &mut || 0.0);
        assert_eq!((playing.pose, playing.primary.is_some()), (Some(0), false));
        // A pose is kept for as long as the entity fits it, though an earlier one has come to fit.
        let mut resting = Playing {
            pose: Some(3),
            ..Playing::default()
        };
        resting.frame(&script, &entity(PoseType::Sleep, false), 0.0, &mut || 0.0);
        assert_eq!(resting.pose, Some(3));
        // A state in a pose the poser lacks: the first the entity fits.
        let mut lost = Playing {
            pose: Some(9),
            ..Playing::default()
        };
        lost.frame(&script, &entity(PoseType::Sleep, false), 0.0, &mut || 0.0);
        assert_eq!(lost.pose, Some(2));
    }

    #[test]
    fn a_quirk_plays_now_and_then() {
        let script = script();
        let stand = entity(PoseType::Stand, false);
        let mut playing = Playing::default();
        // The first frame picks when each comes: 2 to 4 seconds for the blink (3 at a half), 20
        // for the yawn.
        let mut half = || 0.5;
        playing.frame(&script, &stand, 0.0, &mut half);
        assert_eq!(playing.quirk_animations().count(), 0);
        playing.frame(&script, &stand, 2.9, &mut half);
        assert_eq!(playing.quirk_animations().count(), 0);
        // Its time has come: it plays, once more after that (a half of three times, rounded down).
        playing.frame(&script, &stand, 3.0, &mut half);
        let playing_now = |playing: &Playing| {
            playing
                .quirk_animations()
                .map(|active| (name(&active.clip).to_owned(), active.started))
                .collect::<Vec<_>>()
        };
        assert_eq!(playing_now(&playing), [("blink".to_owned(), 3.0)]);
        playing.frame(&script, &stand, 3.2, &mut half);
        assert_eq!(playing_now(&playing), [("blink".to_owned(), 3.0)]);
        // Over (a quarter of a second): the second time starts in the frame after.
        playing.frame(&script, &stand, 3.3, &mut half);
        assert_eq!(playing_now(&playing), []);
        playing.frame(&script, &stand, 3.35, &mut half);
        assert_eq!(playing_now(&playing), [("blink".to_owned(), 3.35)]);
        // Then it waits for its next time, picked as the last loop started: 3.35 + 3.
        playing.frame(&script, &stand, 3.7, &mut half);
        playing.frame(&script, &stand, 6.3, &mut half);
        assert_eq!(playing_now(&playing), []);
        playing.frame(&script, &stand, 6.4, &mut half);
        assert_eq!(playing_now(&playing), [("blink".to_owned(), 6.4)]);
        // The quirk written as an object comes at its own time.
        playing.frame(&script, &stand, 20.0, &mut half);
        assert!(playing_now(&playing).contains(&("yawn".to_owned(), 20.0)));
        // A primary animation ends them all, and none starts while it plays.
        playing.play(&script, &names(&["physical"]), 20.5);
        assert_eq!(playing_now(&playing), []);
        playing.frame(&script, &stand, 21.0, &mut half);
        playing.frame(&script, &stand, 22.0, &mut half);
        assert_eq!(playing_now(&playing), []);
        // In battle, the pose's primary quirk: ten seconds after the pose is taken.
        let fighting = entity(PoseType::Stand, true);
        let mut playing = Playing::default();
        playing.frame(&script, &fighting, 1.0, &mut half);
        playing.frame(&script, &fighting, 10.9, &mut half);
        assert!(playing.primary.is_none());
        playing.frame(&script, &fighting, 11.0, &mut half);
        let primary = playing.primary.clone().unwrap();
        assert!(matches!(&primary.kind, PrimaryKind::Clip(clip) if name(clip) == "stretch"));
        assert_eq!((primary.curve, primary.then), (Curve::Symmetrical, None));
        // Over after its two seconds, and the quirk waits ten more from the frame after.
        playing.frame(&script, &fighting, 13.0, &mut half);
        assert!(playing.primary.is_none());
        playing.frame(&script, &fighting, 13.1, &mut half);
        playing.frame(&script, &fighting, 23.0, &mut half);
        assert!(playing.primary.is_none());
        playing.frame(&script, &fighting, 23.2, &mut half);
        assert!(playing.primary.is_some());
    }
}
