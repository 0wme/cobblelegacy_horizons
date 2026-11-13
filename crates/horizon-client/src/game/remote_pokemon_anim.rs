//! A Pokémon's animation (CobbleLegacy's fork): its poser's pose for what it is doing, run each draw,
//! and what it plays over that pose.
//!
//! Cobblemon's `PosableModel` picks one pose of the species' poser file from the entity's `PoseType`
//! and whether it is in a battle, applies the pose's `transformedParts`, then runs the pose's
//! animations on the model at rest: Bedrock animations (`q.bedrock('pikachu', 'ground_idle')`,
//! sampled at the entity's age) and a few written in code (`q.look`, `q.quadruped_walk`...).
//! `cobblelegacy_cobblemon` reads the files and does the math; this binds them to the parts the mob
//! renderer poses.
//!
//! A Pokémon in the level has a state of its own besides (`cobblelegacy_cobblemon::playing`, which is
//! Cobblemon's `PosableState`): the animation the server named for it (a cry, an attack, a faint:
//! `PlayPosableAnimationPacket`), its quirks (a blink now and then), and the pose it is leaving while
//! it blends into the next. The level moves that state on once a frame ([`Animator::frame`]); a draw
//! only reads it, so a model drawn twice in a frame is the same both times. A model in a screen has
//! no such state: it is in its pose, whole.
//!
//! The same object says what the model is drawn with as time goes: the frame of a texture that has
//! several, and the layers Cobblemon draws over the model (`PosableModel.render`: the model once more
//! for each `ModelLayer` of its look, under that layer's texture). And it says which animations start
//! particle effects as they go (Gastly's gas), for `remote_pokemon_particles.rs` to play them.
//!
//! Not here yet: the MoLang a server sends to run before an animation, a name it sends that is
//! itself MoLang, conditions on the world around the Pokémon (dusk, rain), ridden poses; a layer's
//! `tint` and `scrolling`, and frames blended into one another (`interpolation`), which no Pokémon
//! file of Cobblemon 1.8.1 has.
use super::super::super::Game;
use cobblelegacy_cobblemon::{
    animation::{Animation, AnimationFile},
    molang::{Context, Expression},
    playing::{Clip, Playing, PrimaryKind, Script},
    pose::Delta,
    poser::{self, Poser, Tilt},
    resolver::Texture,
    sound::PlayAnimation,
    wire::PoseType,
};
use horizon_client::mobs::{
    Mob, MobType, Species,
    anim::{Poses, State, part_key},
    own::{self, OwnAnimation},
    render::{self, Layer},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

/// What `Mob::extra.variant` holds for a Pokémon: its pose (the one it appeared in, then the one the
/// server last said), and above it whether the server says it moves (`PokemonEntity.MOVING`).
pub(super) const MOVING: u8 = 0x80;
/// Set once the server said the pose after the Pokémon appeared (`PokemonEntity.POSE_TYPE`).
const SAID: u8 = 0x40;
/// Set while the server says the Pokémon is in a battle (`PokemonEntity.BATTLE_ID`).
const BATTLE: u8 = 0x20;

/// `variant` once the server says the Pokémon is in `pose`: asleep, taking off, in the water.
pub(super) fn with_pose(variant: u8, pose: PoseType) -> u8 {
    (variant & (MOVING | BATTLE)) | SAID | pose as u8
}

/// `variant` once the server says whether the Pokémon is in a battle.
pub(super) fn with_battle(variant: u8, battle: bool) -> u8 {
    (variant & !BATTLE) | if battle { BATTLE } else { 0 }
}

/// The pose a Pokémon is in: the one the server said (`PokemonServerDelegate.updatePoseType`, each tick).
///
/// Until it says one, from the pose the Pokémon appeared in and whether it moves, which is that
/// function's answer within one family (ground, air, water). The pose travels in a metadata type of
/// Cobblemon's, which reaches the window apart from the data vanilla reads (`remote_pokemon_data`): were
/// that part not to come, this still tells a walk from a stand.
pub(super) fn pose_of(variant: u8) -> PoseType {
    use PoseType::*;
    let appeared = PoseType::ALL
        .get(usize::from(variant & !(MOVING | SAID | BATTLE)))
        .copied()
        .unwrap_or(Stand);
    if variant & SAID != 0 {
        return appeared;
    }
    let moving = variant & MOVING != 0;
    match appeared {
        Stand | Walk => {
            if moving {
                Walk
            } else {
                Stand
            }
        }
        Hover | Fly => {
            if moving {
                Fly
            } else {
                Hover
            }
        }
        Float | Swim => {
            if moving {
                Swim
            } else {
                Float
            }
        }
        // Woken up and walking off.
        Sleep if moving => Walk,
        other => other,
    }
}

/// What a pose's conditions read of a Pokémon whose `Mob::extra.variant` is `variant`. It is in the
/// water when the server has it float or swim; the hour and the weather are not known here.
fn described(variant: u8) -> poser::Entity {
    let pose = pose_of(variant);
    let water = matches!(pose, PoseType::Swim | PoseType::Float);
    poser::Entity {
        pose,
        battle: variant & BATTLE != 0,
        touching_water: water,
        under_water: water,
        in_water_or_rain: water,
        dusk: false,
        ride_style: None,
    }
}

/// Whether what the Pokémon's animations do is said in the log (`HORIZON_COBBLEMON_TRACE`): what a
/// test reads, an animation being nothing `horizon-ctl` can ask for.
pub(super) fn trace() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some())
}

/// One animation of a pose, bound to the model's parts.
enum Step {
    /// A Bedrock animation: `file.animations[index]`, and for each of its bones the key of the part it
    /// poses.
    Bedrock {
        file: Arc<AnimationFile>,
        index: usize,
        parts: Vec<u64>,
    },
    /// One written in code: it names the parts it poses as it runs.
    Coded(poser::Animation),
}

impl Step {
    /// `PoseAnimation.labels`, by which a primary animation lets some of a pose's animations go on:
    /// only a look has one.
    fn labels(&self) -> &'static [&'static str] {
        match self {
            Self::Coded(poser::Animation::Look(_)) => &["look"],
            _ => &[],
        }
    }
}

/// A part held differently (`ModelPartTransformation`): the part, what is added to its pose, and
/// whether it is hidden.
type Held = (u64, Delta, bool);

/// A pose of the poser, ready to run.
struct Program {
    /// The pose's `transformedParts`.
    parts: Vec<Held>,
    /// The animations in the pose's order, each with the condition it runs under.
    steps: Vec<(Option<Expression>, Step)>,
}

pub(super) struct Animator {
    /// The poser's own `transformedParts`, twice: `PosableModel.applyAnimations` runs them once
    /// itself and once more through `setDefault`, and their deltas add up.
    parts: Vec<Held>,
    /// A program per pose of the poser, in the file's order.
    programs: Vec<Program>,
    /// The poses' names, for what is said of them.
    names: Vec<String>,
    /// The poser's `properties`: how the model holds an item at a locator.
    properties: Vec<(String, String)>,
    /// What the model plays beyond its poses' animations, and which pose an entity is in.
    script: Script,
    /// For each animation the script may start (`Clip::id`), the parts its bones pose.
    bound: HashMap<(usize, usize), Vec<u64>>,
    /// The program of each `PoseType` (by ordinal), for a Pokémon out of battle.
    by_pose: [Option<usize>; 14],
    /// Whether any of its animations starts a particle effect.
    emits: bool,
    /// The model's own texture when it has several frames, by their sprites in the atlas.
    skin: Option<Texture>,
    /// What is drawn over the model, in the look's order.
    overlays: Vec<Overlay>,
    /// What each Pokémon of the level that wears this model plays, by entity id.
    entities: Mutex<HashMap<u32, Entity>>,
}

/// `PokemonClientDelegate`: three seconds after it starts to faint, a Pokémon is drawn no more
/// (`activeSendoutScale` is 0 from then on).
const FAINTED_SECONDS: f32 = 3.0;

/// One Pokémon's animation state.
#[derive(Default)]
struct Entity {
    playing: Playing,
    /// The state's seconds when it started to faint.
    fainted: Option<f32>,
    /// What the pose's animations keep from frame to frame (a tilt that eases), as the frame that
    /// is drawn found it, and as it left it.
    tilt_before: Tilt,
    tilt: Tilt,
    /// What its quirks draw their times from.
    random: u32,
}

impl Entity {
    /// A number from 0 to 1, 1 excluded (xorshift).
    fn random(state: &mut u32) -> f32 {
        if *state == 0 {
            *state = 0x9E37_79B9;
        }
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        (*state >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// One layer of a look (`resolver::Layer`), ready to draw.
pub(super) struct Overlay {
    /// The layer's texture, by its frames' sprites in the atlas.
    pub(super) texture: Texture,
    /// Drawn at full brightness and without shading, whatever the light on the Pokémon.
    pub(super) emissive: bool,
    /// Its texels blend with what is behind them; else each is there whole or not at all.
    pub(super) translucent: bool,
    /// `translucent_cull`: a translucent layer that shows the model's own faces only, as every other
    /// layer does, not the back of each as well.
    pub(super) cull: bool,
}

/// `PosableState.animationSeconds` for an age in ticks, which is what a texture's frames go by
/// (`AnimatedModelTextureSupplier`).
fn seconds(age: f32) -> f32 {
    age / 20.0
}

/// What a pose's conditions and its animations' MoLang ask of the entity, as it is at `state`.
pub(super) fn context(state: &State) -> Context {
    Context {
        anim_time: seconds(state.age),
        age_in_ticks: state.age,
        limb_swing: state.pos,
        limb_swing_amount: state.speed,
    }
}

/// One of a pose's Bedrock animations that starts particle effects as it goes.
pub(super) struct Emitting<'a> {
    /// What tells the animation from the model's others. Cobblemon knows a keyframe by which object
    /// it is, and two poses that name one animation share its keyframes.
    pub(super) id: (usize, usize),
    pub(super) animation: &'a Animation,
    condition: Option<&'a Expression>,
}

impl Emitting<'_> {
    /// Whether the pose runs the animation for an entity as `context` has it.
    pub(super) fn runs(&self, context: &Context) -> bool {
        self.condition.is_none_or(|c| c.eval(context) != 0.0)
    }
}

/// A MoLang source's value with nothing known of the entity: what a part's visibility is asked
/// once, as the model is built. 0 for one that does not parse.
fn value(source: &str) -> f64 {
    Expression::parse(source).map_or(0.0, |e| f64::from(e.eval(&Context::default())))
}

/// One pass over a model's animations: what they read of the mob, and what those written in code
/// keep as they go.
struct Pass<'a> {
    state: &'a State,
    context: Context,
    /// `Entity.getDeltaMovement`, blocks a tick.
    velocity: [f64; 3],
    tilt: Tilt,
    /// Whether the animations read from files run. Not when only `tilt` is wanted.
    bones: bool,
}

/// What an animation does to a part: its key, the change, whether it is hidden.
type Out<'a> = dyn FnMut(u64, Delta, bool) + 'a;

impl Animator {
    /// `load` reads a Bedrock animation group (`pikachu` for `pikachu.animation.json`).
    pub(super) fn new(
        poser: &Poser,
        load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>,
    ) -> Self {
        let held = |t: &poser::TransformedPart| -> Held {
            let hidden = t.visible.as_ref().is_some_and(|v| !v.get(&mut value));
            (part_key(&t.part), t.delta, hidden)
        };
        let programs: Vec<Program> = poser
            .poses
            .iter()
            .map(|pose| Program {
                parts: pose.transformed_parts.iter().map(held).collect(),
                steps: pose
                    .animations
                    .iter()
                    .filter_map(|entry| {
                        let condition = match &entry.condition {
                            // A condition that does not parse never holds.
                            Some(source) => Some(Expression::parse(source).ok()?),
                            None => None,
                        };
                        let step = match &entry.animation {
                            poser::Animation::Bedrock { group, name } => {
                                let file = load(group)?;
                                let wanted = format!("animation.{group}.{name}");
                                let index =
                                    file.animations.iter().position(|a| a.name == wanted)?;
                                let parts = file.animations[index]
                                    .bones
                                    .iter()
                                    .map(|b| part_key(&b.bone))
                                    .collect();
                                Step::Bedrock { file, index, parts }
                            }
                            // Kept as parsed and not run: nothing here knows what it does.
                            poser::Animation::Other(_) => return None,
                            coded => Step::Coded(coded.clone()),
                        };
                        Some((condition, step))
                    })
                    .collect(),
            })
            .collect();
        let script = Script::new(poser, load);
        let bound: HashMap<(usize, usize), Vec<u64>> = script
            .clips()
            .map(|clip| {
                let bones = &clip.animation().bones;
                (clip.id(), bones.iter().map(|b| part_key(&b.bone)).collect())
            })
            .collect();
        let mut by_pose = [None; 14];
        for (ordinal, &pose) in PoseType::ALL.iter().enumerate() {
            by_pose[ordinal] = script.wanted(&described(with_pose(0, pose)));
        }
        let mut animator = Self {
            parts: poser
                .transformed_parts
                .iter()
                .chain(&poser.transformed_parts)
                .map(held)
                .collect(),
            programs,
            names: poser.poses.iter().map(|pose| pose.name.clone()).collect(),
            properties: poser.properties.clone(),
            script,
            bound,
            by_pose,
            emits: false,
            skin: None,
            overlays: Vec::new(),
            entities: Mutex::default(),
        };
        animator.emits = (0..animator.programs.len())
            .any(|program| animator.emitting(program).next().is_some())
            || animator
                .script
                .clips()
                .any(|clip| !clip.animation().particles.is_empty());
        animator
    }

    /// A model no poser poses: it stays at rest.
    pub(super) fn still() -> Self {
        Self {
            parts: Vec::new(),
            programs: Vec::new(),
            names: Vec::new(),
            properties: Vec::new(),
            script: Script::default(),
            bound: HashMap::new(),
            by_pose: [None; 14],
            emits: false,
            skin: None,
            overlays: Vec::new(),
            entities: Mutex::default(),
        }
    }

    /// The same, drawn with `skin` when the model's own texture has several frames, and with
    /// `overlays` over it.
    pub(super) fn dressed(mut self, skin: Option<Texture>, overlays: Vec<Overlay>) -> Self {
        (self.skin, self.overlays) = (skin, overlays);
        self
    }

    /// `PosableModel.getLocatorDisplayContext`: the name of the way an item shown at `locator` is
    /// held, when the poser says (`Poser::display_context`).
    pub(super) fn display_context(&self, locator: &str) -> Option<&str> {
        let wanted = format!("{locator}_display_context");
        let found = self.properties.iter().rev().find(|(key, _)| *key == wanted);
        found.map(|(_, context)| context.as_str())
    }

    /// Whether one of its layers is seen from both sides: it is drawn on `own::TRANSLUCENT_LAYER`,
    /// which the type then needs a model for.
    pub(super) fn two_sided(&self) -> bool {
        self.overlays
            .iter()
            .any(|overlay| !overlay.emissive && overlay.translucent && !overlay.cull)
    }

    /// The place in the poser's poses of the pose of each `PoseType`, by ordinal, for a Pokémon out
    /// of battle: what else is told by pose (a cry's sound, `remote_pokemon_sounds.rs`) is told by
    /// the pose the model is drawn in.
    pub(super) fn by_pose(&self) -> [Option<usize>; 14] {
        self.by_pose
    }

    /// The place in the poser's poses of the pose a Pokémon whose `Mob::extra.variant` is `variant`
    /// takes when it is in none.
    pub(super) fn program(&self, variant: u8) -> Option<usize> {
        self.script.wanted(&described(variant))
    }

    /// The animations of the pose at `program` that start particle effects, in the pose's order.
    pub(super) fn emitting(&self, program: usize) -> impl Iterator<Item = Emitting<'_>> {
        let steps = self.programs.get(program).map(|p| &p.steps[..]);
        steps
            .into_iter()
            .flatten()
            .filter_map(|(condition, step)| match step {
                Step::Bedrock { file, index, .. } => Some(Emitting {
                    id: (Arc::as_ptr(file) as usize, *index),
                    animation: &file.animations[*index],
                    condition: condition.as_ref(),
                }),
                Step::Coded(_) => None,
            })
            .filter(|emitting| !emitting.animation.particles.is_empty())
    }

    /// Whether an animation of any pose, or one it may start, starts a particle effect.
    pub(super) fn emits(&self) -> bool {
        self.emits
    }

    fn entities(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Entity>> {
        self.entities.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Once a frame, before it is drawn: what `mob`, a Pokémon of the level, plays moves on to
    /// `state` (`PosableModel.applyAnimations` and `PosableState.incrementAge`, for what they change
    /// of the state).
    pub(super) fn frame(&self, mob: &Mob, state: &State) {
        let mut entities = self.entities();
        let entity = entities.entry(mob.id).or_default();
        let Entity {
            playing, random, ..
        } = &mut *entity;
        let before = trace().then(|| {
            let quirks = playing.quirk_animations().count();
            (playing.pose, playing.primary.is_some(), quirks)
        });
        playing.frame(
            &self.script,
            &described(mob.extra.variant),
            seconds(state.age),
            &mut || Entity::random(random),
        );
        if let Some(before) = before {
            self.say(mob, playing, before);
        }
        // What the animations written in code keep, as this frame's draws find it and as they
        // leave it: worked out here once, so that a second draw of the frame changes nothing.
        entity.tilt_before = entity.tilt;
        let mut pass = Pass::new(mob, state, entity.tilt, false);
        self.animate(&entity.playing, &mut pass, &mut |_, _, _| {});
        entity.tilt = pass.tilt;
    }

    /// What a frame changed of what `mob` plays, for the log: `before` is the pose it was in,
    /// whether a primary animation played, and how many of its quirks' animations did.
    fn say(&self, mob: &Mob, playing: &Playing, before: (Option<usize>, bool, usize)) {
        let id = mob.id;
        let pose = |pose: Option<usize>| {
            let name = pose.and_then(|pose| self.names.get(pose));
            name.map_or("none", String::as_str)
        };
        let (was_in, had_primary, quirks) = before;
        if playing.pose != was_in {
            log::info!(
                "Horizon: Pokémon {id} is drawn in the pose {} ({})",
                pose(playing.pose),
                mob.kind.id
            );
        }
        if let Some(primary) = playing.primary.as_ref().filter(|_| !had_primary) {
            match (&primary.kind, primary.then) {
                (PrimaryKind::Blend { before, after }, _) => log::info!(
                    "Horizon: Pokémon {id} blends from the pose {} to the pose {}",
                    pose(Some(*before)),
                    pose(Some(*after))
                ),
                (PrimaryKind::Clip(clip), Some(then)) => log::info!(
                    "Horizon: Pokémon {id} plays the transition {} to the pose {}",
                    clip.animation().name,
                    pose(Some(then))
                ),
                (PrimaryKind::Clip(clip), None) => log::info!(
                    "Horizon: Pokémon {id} starts the quirk {}",
                    clip.animation().name
                ),
            }
        }
        if playing.quirk_animations().count() > quirks
            && let Some(last) = playing
                .quirk_animations()
                .max_by(|a, b| a.started.total_cmp(&b.started))
        {
            log::info!(
                "Horizon: Pokémon {id} starts the quirk {}",
                last.clip.animation().name
            );
        }
    }

    /// `PosableState.addFirstAnimation`: entity `id`, `age` ticks old, starts the first of `names`
    /// its model has an animation for. The animation's name, None when it has none.
    pub(super) fn play(&self, id: u32, names: &[String], age: f32) -> Option<String> {
        let mut entities = self.entities();
        let playing = &mut entities.entry(id).or_default().playing;
        let name = self
            .script
            .named(playing.pose, names)
            .map(|play| play.clip.animation().name.clone())?;
        playing.play(&self.script, names, seconds(age));
        Some(name)
    }

    /// `PokemonClientDelegate` on `DYING_EFFECTS_STARTED`: entity `id`, `age` ticks old, faints
    /// (`fainting`), or is back from it. The animation it starts, None for a model without a faint:
    /// that one stands until it is drawn no more.
    pub(super) fn faint(&self, id: u32, fainting: bool, age: f32) -> Option<String> {
        let mut entities = self.entities();
        let entity = entities.entry(id).or_default();
        if !fainting {
            entity.fainted = None;
            return None;
        }
        let now = seconds(age);
        entity.fainted = Some(now);
        let playing = &mut entity.playing;
        let name = self
            .script
            .named(playing.pose, &["faint".to_owned()])
            .map(|play| play.clip.animation().name.clone())?;
        playing.faint(&self.script, now);
        Some(name)
    }

    /// Entity `id` is gone, or wears another model.
    pub(super) fn forget(&self, id: u32) {
        self.entities().remove(&id);
    }

    /// `with` what entity `id` plays, None for one the level has not moved on yet.
    pub(super) fn playing<R>(&self, id: u32, with: impl FnOnce(Option<&Playing>) -> R) -> R {
        with(self.entities().get(&id).map(|entity| &entity.playing))
    }

    /// The place in the poser's poses of the pose entity `id` is in.
    pub(super) fn current(&self, id: u32) -> Option<usize> {
        self.playing(id, |playing| playing.and_then(|playing| playing.pose))
    }
}

impl OwnAnimation for Animator {
    fn texture(&self, age: f32) -> Option<&str> {
        self.skin.as_ref()?.at(seconds(age))
    }

    fn layers(&self, mob: &Mob, age: f32, out: &mut Vec<Layer>) {
        // `PosableModel.render` gives a layer the model's own colour: the red of a Pokémon called back.
        self.overlays_at(age, render::body_color(mob, 0.0), out);
    }

    fn setup(&self, mob: &Mob, state: &State, poses: &mut Poses) {
        let entities = self.entities();
        let entity = entities.get(&mob.id);
        // Fainted for good: nothing of it is drawn, as a model scaled to nothing.
        if entity
            .and_then(|entity| entity.fainted)
            .is_some_and(|since| seconds(state.age) - since >= FAINTED_SECONDS)
        {
            poses.hide_key(part_key(super::STAND));
            return;
        }
        let whole;
        let (playing, tilt) = match entity {
            Some(entity) => (&entity.playing, entity.tilt_before),
            // A model in a screen, or a Pokémon the level has not moved on yet: in its pose.
            None => {
                whole = Playing::posed(self.program(mob.extra.variant));
                (&whole, Tilt::default())
            }
        };
        let mut pass = Pass::new(mob, state, tilt, true);
        self.animate(playing, &mut pass, &mut |part, delta, hidden| {
            if let Some(pose) = poses.by_key(part) {
                delta.apply(pose);
            }
            if hidden {
                poses.hide_key(part);
            }
        });
    }
}

impl<'a> Pass<'a> {
    fn new(mob: &Mob, state: &'a State, mut tilt: Tilt, bones: bool) -> Self {
        tilt.begin_frame();
        Self {
            state,
            context: context(state),
            velocity: [mob.velocity.x, mob.velocity.y, mob.velocity.z],
            tilt,
            bones,
        }
    }
}

impl Animator {
    /// The layers drawn over the model `age` ticks into its life, in `color`.
    fn overlays_at(&self, age: f32, color: u32, out: &mut Vec<Layer>) {
        for overlay in &self.overlays {
            // Past the last frame of a texture that does not start over, the last one stays; a layer
            // whose texture has no frame for this moment is not drawn, as in Cobblemon.
            let Some(sprite) = overlay.texture.at(seconds(age)) else {
                continue;
            };
            // `PosableModel.getLayer`: an emissive layer is drawn by the emissive render type whether
            // it is translucent or not; of the others, only a translucent one blends, and it alone is
            // seen from both sides unless it says otherwise.
            let model = match (overlay.emissive, overlay.translucent, overlay.cull) {
                (false, true, false) => own::TRANSLUCENT_LAYER,
                (false, true, true) => own::TRANSLUCENT_CULL_LAYER,
                _ => own::LAYER,
            };
            out.push(Layer {
                model: model.into(),
                texture: sprite.to_owned().into(),
                color,
                full_bright: overlay.emissive,
                scroll: 0.0,
            });
        }
    }

    /// One of a pose's animations at `intensity`, `seconds` into its time.
    fn step(&self, step: &Step, intensity: f32, seconds: f32, pass: &mut Pass, out: &mut Out) {
        match step {
            Step::Bedrock { file, index, parts } => {
                if pass.bones {
                    file.animations[*index].sample(seconds, &pass.context, &mut |bone, delta| {
                        out(parts[bone], delta.at(intensity), false);
                    });
                }
            }
            Step::Coded(animation) => {
                let state = pass.state;
                let motion = poser::Motion {
                    limb_swing: state.pos,
                    limb_swing_amount: state.speed,
                    age_in_ticks: state.age,
                    // `PosableState.animationSeconds`: the entity's age, with the partial tick.
                    animation_seconds: self::seconds(state.age),
                    head_yaw: state.y_rot,
                    head_pitch: state.x_rot,
                    velocity: pass.velocity,
                    ridden: false,
                    intensity,
                };
                animation.apply(&motion, &mut pass.tilt, &mut |part, delta| {
                    out(part_key(part), delta, false);
                });
            }
        }
    }

    /// An animation that was started, `passed` seconds into it.
    fn clip(&self, clip: &Clip, passed: f32, intensity: f32, pass: &Pass, out: &mut Out) {
        if !pass.bones {
            return;
        }
        let Some(parts) = self.bound.get(&clip.id()) else {
            return;
        };
        clip.animation()
            .sample(passed, &pass.context, &mut |bone, delta| {
                out(parts[bone], delta.at(intensity), false);
            });
    }

    /// The pose at `program`, at `intensity`: its parts held as it holds them, then each of its
    /// animations (`PoseTransitionAnimation`, for each of the two poses it blends).
    fn whole(&self, program: usize, intensity: f32, pass: &mut Pass, out: &mut Out) {
        let Some(program) = self.programs.get(program) else {
            return;
        };
        for &(part, delta, hidden) in &program.parts {
            out(part, delta.at(intensity), hidden);
        }
        let seconds = seconds(pass.state.age);
        for (condition, step) in &program.steps {
            if condition
                .as_ref()
                .is_some_and(|c| c.eval(&pass.context) == 0.0)
            {
                continue;
            }
            self.step(step, intensity, seconds, pass, out);
        }
    }

    /// The model's parts as `playing` has them at the pass's moment, in the order
    /// `PosableModel.applyAnimations` goes: the pose's held parts, its quirks, the primary animation,
    /// what else was started, then the pose's own animations, each at what the primary one leaves it.
    fn animate(&self, playing: &Playing, pass: &mut Pass, out: &mut Out) {
        let now = seconds(pass.state.age);
        for &(part, delta, hidden) in &self.parts {
            out(part, delta, hidden);
        }
        let program = playing.pose.and_then(|pose| self.programs.get(pose));
        let blend = playing
            .primary
            .as_ref()
            .and_then(|primary| match primary.kind {
                PrimaryKind::Blend { before, after } => Some((before, after, primary.blend(now))),
                PrimaryKind::Clip(_) => None,
            });
        if let Some((before, after, new)) = blend {
            // Neither pose is the model's own for now: the old one fades as the new one comes.
            self.whole(before, 1.0 - new, pass, out);
            self.whole(after, new, pass, out);
        } else {
            for &(part, delta, hidden) in program.map_or(&[][..], |p| &p.parts[..]) {
                out(part, delta, hidden);
            }
            match &playing.primary {
                Some(primary) => {
                    if let PrimaryKind::Clip(clip) = &primary.kind {
                        let intensity = playing.primary_intensity();
                        self.clip(clip, now - primary.started, intensity, pass, out);
                    }
                }
                None => {
                    for quirk in playing.quirk_animations() {
                        self.clip(&quirk.clip, now - quirk.started, 1.0, pass, out);
                    }
                }
            }
        }
        for active in &playing.active {
            self.clip(&active.clip, now - active.started, 1.0, pass, out);
        }
        let Some(program) = program else {
            return;
        };
        for (condition, step) in &program.steps {
            let Some(intensity) = playing.idle(step.labels()) else {
                continue;
            };
            if condition
                .as_ref()
                .is_some_and(|c| c.eval(&pass.context) == 0.0)
            {
                continue;
            }
            self.step(step, intensity, now, pass, out);
        }
    }
}

/// Which animator poses each Pokémon of the level: the one of the look its type was made with.
#[derive(Default)]
pub(super) struct Players {
    by_entity: HashMap<i32, Player>,
}

struct Player {
    /// The type the Pokémon had when its animator was looked for: its look is found again when
    /// that changes.
    kind: Arc<MobType>,
    animator: Option<Arc<Animator>>,
}

impl Players {
    /// What poses entity `id`, once the level has seen it with the body it has.
    pub(super) fn animator(&self, id: i32) -> Option<&Arc<Animator>> {
        self.by_entity.get(&id)?.animator.as_ref()
    }

    /// Entity `id` is gone: what it played is forgotten.
    pub(super) fn forget(&mut self, id: i32) {
        if let Some(Player {
            animator: Some(animator),
            ..
        }) = self.by_entity.remove(&id)
        {
            animator.forget(id as u32);
        }
    }
}

impl super::Pokemon {
    /// What poses `mob`, a Pokémon of the level: its look's animator, found by the type it wears.
    /// None while its look's files are being read, and for a body that stands in.
    fn animator_of(&mut self, mob: &Mob) -> Option<Arc<Animator>> {
        let id = mob.id as i32;
        if let Some(player) = self.players.by_entity.get(&id)
            && Arc::ptr_eq(&player.kind, &mob.kind)
        {
            return player.animator.clone();
        }
        let animator = self
            .looks
            .values()
            .flatten()
            .find(|look| Arc::ptr_eq(&look.kind, &mob.kind))
            .and_then(|look| look.animator.clone());
        let player = Player {
            kind: mob.kind.clone(),
            animator: animator.clone(),
        };
        // What it played under another body was that model's.
        if let Some(Player {
            animator: Some(old),
            ..
        }) = self.players.by_entity.insert(id, player)
        {
            old.forget(mob.id);
        }
        animator
    }
}

impl Game<'_> {
    /// Once a frame, before it is drawn: what each Pokémon of the level plays moves on.
    pub(super) fn pokemon_animation_frame(&mut self) {
        let partial = self.clock.fraction() as f32;
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        for mob in self.mobs.iter().filter(|m| m.species() == Species::Pokemon) {
            if let Some(animator) = r.pokemon.animator_of(mob) {
                animator.frame(mob, &horizon_client::mobs::anim::state(mob, partial));
            }
        }
    }

    /// Pokémon `id` faints, or is back from it (`PokemonEntity.DYING_EFFECTS_STARTED`): it plays its
    /// faint, and is drawn no more three seconds on.
    pub(super) fn pokemon_faint(&mut self, id: i32, fainting: bool) {
        let partial = self.clock.fraction() as f32;
        let Some(mob) = self.mob_index.get(&self.mobs, id as u32) else {
            return;
        };
        if mob.species() != Species::Pokemon {
            return;
        }
        let Some(animator) = self
            .remote
            .as_mut()
            .and_then(|r| r.pokemon.animator_of(mob))
        else {
            return;
        };
        let age = horizon_client::mobs::anim::state(mob, partial).age;
        let started = animator.faint(mob.id, fainting, age);
        if trace() && fainting {
            log::info!(
                "Horizon: Pokémon {id} faints and starts {}",
                started.as_deref().unwrap_or("nothing")
            );
        }
    }

    /// `PlayPosableAnimationHandler`, for what of it is seen: the entity starts the first of the
    /// packet's animations its model has (a cry, an attack, a faint). What is heard of it is
    /// `pokemon_animation_sounds`'s.
    pub(super) fn pokemon_animation_play(&mut self, data: &[u8]) {
        // What does not decode is said by the sounds' reader.
        let Ok(packet) = PlayAnimation::read(data) else {
            return;
        };
        let partial = self.clock.fraction() as f32;
        let Some(mob) = self.mob_index.get(&self.mobs, packet.entity as u32) else {
            return;
        };
        let Some(animator) = self
            .remote
            .as_mut()
            .and_then(|r| r.pokemon.animator_of(mob))
        else {
            return;
        };
        let age = horizon_client::mobs::anim::state(mob, partial).age;
        let started = animator.play(mob.id, &packet.animations, age);
        if trace() {
            log::info!(
                "Horizon: Pokémon {} is told to play {:?} and starts {}",
                packet.entity,
                packet.animations,
                started.as_deref().unwrap_or("nothing")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PoseType::*;
    use cobblelegacy_cobblemon::playing::Playing;

    #[test]
    fn the_pose_is_the_server_s_once_it_said_one() {
        // As it appeared, a Pokémon is told walking from standing by `MOVING` alone.
        assert_eq!(pose_of(Stand as u8), Stand);
        assert_eq!(pose_of(Stand as u8 | MOVING), Walk);
        assert_eq!(pose_of(Hover as u8 | MOVING), Fly);
        assert_eq!(pose_of(Swim as u8), Float);
        assert_eq!(pose_of(Sleep as u8), Sleep);
        // The server's word stands whatever `MOVING` says, and keeps that bit as it was.
        for said in [Sleep, Fly, Hover, Swim, Stand, Walk, Glide] {
            for variant in [Stand as u8, Stand as u8 | MOVING, Fly as u8 | MOVING] {
                let now = with_pose(variant, said);
                assert_eq!(pose_of(now), said);
                assert_eq!(now & MOVING, variant & MOVING);
                // `MOVING` changing afterwards (`remote_mob_data`) does not undo it.
                assert_eq!(pose_of(now ^ MOVING), said);
            }
        }
        // A second word replaces the first.
        assert_eq!(
            pose_of(with_pose(with_pose(Stand as u8, Sleep), Walk)),
            Walk
        );
        // A battle is told apart from the pose, and neither word undoes the other.
        let fighting = with_battle(with_pose(Stand as u8 | MOVING, Walk), true);
        assert_eq!(pose_of(fighting), Walk);
        assert!(described(fighting).battle && !described(Stand as u8).battle);
        assert!(described(with_pose(fighting, Sleep)).battle);
        assert_eq!(fighting & MOVING, MOVING);
        assert!(!described(with_battle(fighting, false)).battle);
        assert_eq!(pose_of(with_battle(fighting, false)), Walk);
        // In the water when it floats or swims.
        assert!(described(with_pose(0, Swim)).touching_water);
        assert!(!described(with_pose(0, Walk)).touching_water);
    }

    /// A model of a body and a head. It idles a pixel up, looks about, and in battle idles two
    /// pixels up; asleep it lies three pixels down, its head hidden. A cry nods the head, an attack
    /// lifts the body eight pixels and lets the head look on, a blink shrinks the head.
    fn testmon() -> Animator {
        let poser = Poser::parse(
            r#"{
                "animations": {
                    "cry": "q.bedrock_stateful('testmon', 'cry')",
                    "physical": "q.bedrock_primary('testmon', 'physical', 'look', q.curve('one'))"
                },
                "poses": {
                    "battle": {
                        "poseTypes": ["STAND"],
                        "isBattle": true,
                        "animations": ["q.bedrock('testmon', 'battle_idle')"]
                    },
                    "standing": {
                        "poseTypes": ["STAND", "WALK"],
                        "isBattle": false,
                        "animations": ["q.bedrock('testmon', 'idle')", "q.look('head')"],
                        "quirks": ["q.bedrock_quirk('testmon', 'blink', 1, 1, 1)"]
                    },
                    "sleeping": {
                        "poseTypes": ["SLEEP"],
                        "transformedParts": [{"part": "head", "position": [0, 2, 0], "isVisible": false}],
                        "animations": ["q.bedrock('testmon', 'sleep')"]
                    }
                }
            }"#,
        )
        .unwrap();
        let moved = |name: &str, more: &str, y: f32| {
            format!(
                r#""animation.testmon.{name}": {{{more} "bones": {{"body": {{"position": [0, {y}, 0]}}}}}}"#
            )
        };
        let file = Arc::new(
            AnimationFile::parse(&format!(
                r#"{{"animations": {{
                    {}, {}, {}, {},
                    "animation.testmon.cry": {{"animation_length": 1.0,
                        "bones": {{"head": {{"rotation": [10, 0, 0]}}}}}},
                    "animation.testmon.blink": {{"animation_length": 0.5,
                        "bones": {{"head": {{"scale": [0.5, 0.5, 0.5]}}}}}}
                }}}}"#,
                moved("idle", r#""loop": true,"#, 1.0),
                moved("battle_idle", r#""loop": true,"#, 2.0),
                moved("sleep", r#""loop": true,"#, -3.0),
                moved("physical", r#""animation_length": 2.0,"#, 8.0),
            ))
            .unwrap(),
        );
        let mut load = |group: &str| (group == "testmon").then(|| file.clone());
        Animator::new(&poser, &mut load)
    }

    fn at(age: f32) -> State {
        State {
            pos: 0.0,
            speed: 0.0,
            age,
            y_rot: 20.0,
            x_rot: 0.0,
            swing: 0.0,
            baby: false,
            partial: 0.0,
        }
    }

    /// What `playing` makes of the model `age` ticks in: the body's height (pixels up), the head's
    /// pitch and yaw (degrees), its scale, and whether it is hidden.
    fn drawn(animator: &Animator, playing: &Playing, age: f32) -> (f32, [f32; 2], f32, bool) {
        let state = at(age);
        let mut pass = Pass {
            state: &state,
            context: context(&state),
            velocity: [0.0; 3],
            tilt: Tilt::default(),
            bones: true,
        };
        let (body, head) = (part_key("body"), part_key("head"));
        let (mut height, mut turn, mut scale, mut hid) = (0.0, [0.0; 2], 1.0, false);
        animator.animate(playing, &mut pass, &mut |part, delta, hidden| {
            if part == body {
                height -= delta.position[1];
            } else if part == head {
                turn[0] += delta.rotation[0] * 57.2958;
                turn[1] += delta.rotation[1] * 57.2958;
                scale *= delta.scale[1];
                hid |= hidden;
            }
        });
        let round = |n: f32| (n * 1000.0).round() / 1000.0;
        (round(height), turn.map(round), round(scale), hid)
    }

    #[test]
    fn what_is_played_moves_the_model_over_its_pose() {
        let animator = testmon();
        assert_eq!(animator.names, ["battle", "standing", "sleeping"]);
        let standing = described(Stand as u8);
        // The random numbers its quirks ask for: always a half.
        let mut half = || 0.5;
        let mut playing = Playing::default();
        playing.frame(&animator.script, &standing, 0.0, &mut half);
        assert_eq!(playing.pose, Some(1));
        // In its pose: the idle's pixel, and the head turned the way it looks.
        assert_eq!(
            drawn(&animator, &playing, 0.0),
            (1.0, [0.0, 20.0], 1.0, false)
        );
        // A model in a screen is in its pose the same, with nothing played.
        let whole = Playing::posed(animator.program(Stand as u8));
        assert_eq!(
            drawn(&animator, &whole, 0.0),
            (1.0, [0.0, 20.0], 1.0, false)
        );

        // Its blink comes a second in, for half a second.
        playing.frame(&animator.script, &standing, 1.0, &mut half);
        assert_eq!(
            drawn(&animator, &playing, 24.0),
            (1.0, [0.0, 20.0], 0.5, false)
        );
        playing.frame(&animator.script, &standing, 1.6, &mut half);
        assert_eq!(drawn(&animator, &playing, 32.0).2, 1.0);

        // A cry nods the head over the pose, from its start.
        assert!(playing.play(&animator.script, &["cry".to_owned()], 2.0));
        assert_eq!(
            drawn(&animator, &playing, 50.0),
            (1.0, [10.0, 20.0], 1.0, false)
        );
        // An attack takes the pose's place: its own eight pixels and no idle; the head looks on.
        // The cry ends with its start.
        assert!(playing.play(&animator.script, &["physical".to_owned()], 2.5));
        playing.frame(&animator.script, &standing, 3.0, &mut half);
        assert_eq!(
            drawn(&animator, &playing, 60.0),
            (8.0, [0.0, 20.0], 1.0, false)
        );
        // Over after its two seconds.
        playing.frame(&animator.script, &standing, 4.5, &mut half);
        assert_eq!(drawn(&animator, &playing, 90.0).0, 1.0);

        // Into battle the two poses blend for ten ticks (no transition of the pose's own): half
        // way, half of each idle, and no look, which is the standing pose's alone.
        let fighting = described(with_battle(Stand as u8, true));
        playing.frame(&animator.script, &fighting, 5.0, &mut half);
        assert_eq!(
            drawn(&animator, &playing, 100.0),
            (1.0, [0.0, 20.0], 1.0, false)
        );
        assert_eq!(
            drawn(&animator, &playing, 105.0),
            (1.5, [0.0, 10.0], 1.0, false)
        );
        playing.frame(&animator.script, &fighting, 5.5, &mut half);
        assert_eq!(playing.pose, Some(0));
        assert_eq!(
            drawn(&animator, &playing, 110.0),
            (2.0, [0.0, 0.0], 1.0, false)
        );

        // Asleep: the pose's held parts come with it.
        let mut asleep = Playing::default();
        let sleeping = described(with_pose(0, Sleep));
        asleep.frame(&animator.script, &sleeping, 0.0, &mut half);
        assert_eq!(
            drawn(&animator, &asleep, 0.0),
            (-3.0, [0.0, 0.0], 1.0, true)
        );
    }

    /// Ponyta's mane, four frames at ten a second, and a layer of each other kind.
    fn dressed() -> Animator {
        let flame = Texture::Animated {
            frames: (1..=4).map(|i| format!("ponyta|{i}")).collect(),
            fps: 10.0,
            looping: true,
            interpolation: false,
        };
        // `PoseType::None` is in scope here.
        Animator::still().dressed(
            Option::None,
            vec![
                Overlay {
                    texture: flame,
                    emissive: true,
                    translucent: false,
                    cull: false,
                },
                Overlay {
                    texture: Texture::Static("ponyta|5".into()),
                    emissive: false,
                    translucent: true,
                    cull: false,
                },
                Overlay {
                    texture: Texture::Static("ponyta|6".into()),
                    emissive: false,
                    translucent: false,
                    cull: false,
                },
                Overlay {
                    texture: Texture::Static("ponyta|7".into()),
                    emissive: true,
                    translucent: true,
                    cull: true,
                },
                Overlay {
                    texture: Texture::Static("ponyta|8".into()),
                    emissive: false,
                    translucent: true,
                    cull: true,
                },
            ],
        )
    }

    #[test]
    fn a_layer_is_drawn_with_the_frame_of_the_moment() {
        let animator = dressed();
        let at = |age: f32| {
            let mut layers = Vec::new();
            animator.overlays_at(age, 0xFF11_2233, &mut layers);
            layers
        };
        // Two ticks a frame at ten frames a second; the fifth frame is the first again.
        let flames: Vec<String> = [0.0, 1.9, 2.0, 5.5, 6.0, 8.0, 801.0]
            .iter()
            .map(|&age| at(age)[0].texture.to_string())
            .collect();
        assert_eq!(
            flames,
            [
                "ponyta|1", "ponyta|1", "ponyta|2", "ponyta|3", "ponyta|4", "ponyta|1", "ponyta|1"
            ]
        );
        let layers = at(3.0);
        let drawn: Vec<(&str, &str, bool)> = layers
            .iter()
            .map(|l| (&*l.model, &*l.texture, l.full_bright))
            .collect();
        // An emissive layer glows on the model's own key, translucent or not; of the others the
        // translucent ones alone are on the keys that blend, two-sided unless they cull.
        assert_eq!(
            drawn,
            [
                (own::LAYER, "ponyta|2", true),
                (own::TRANSLUCENT_LAYER, "ponyta|5", false),
                (own::LAYER, "ponyta|6", false),
                (own::LAYER, "ponyta|7", true),
                (own::TRANSLUCENT_CULL_LAYER, "ponyta|8", false),
            ]
        );
        // Each in the colour it is given (the model's own), none scrolling.
        assert!(
            layers
                .iter()
                .all(|l| l.color == 0xFF11_2233 && l.scroll == 0.0)
        );
        assert!(animator.two_sided());
        // Nothing is drawn over a model without layers.
        assert!(!Animator::still().two_sided());
        let mut none = Vec::new();
        Animator::still().overlays_at(3.0, 0xFFFF_FFFF, &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn a_texture_of_several_frames_is_the_body_s_by_its_age() {
        let coat = Texture::Animated {
            frames: vec!["golurk".into(), "golurk|1".into(), "golurk|2".into()],
            fps: 5.0,
            looping: false,
            interpolation: false,
        };
        let animator = Animator::still().dressed(Some(coat), Vec::new());
        let frames: Vec<_> = [0.0, 3.9, 4.0, 8.0, 400.0]
            .iter()
            .map(|&age| OwnAnimation::texture(&animator, age))
            .collect();
        // Four ticks a frame, and the last one stays: it does not start over.
        assert_eq!(
            frames,
            [
                Some("golurk"),
                Some("golurk"),
                Some("golurk|1"),
                Some("golurk|2"),
                Some("golurk|2")
            ]
        );
        // A still texture is the type's own all along.
        assert_eq!(
            OwnAnimation::texture(&Animator::still(), 12.0),
            Option::None
        );
    }
}
