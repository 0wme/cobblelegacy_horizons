// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! How a Pokémon holds itself: Cobblemon's poser files (`bedrock/pokemon/posers/**/<name>.json`, read by
//! `JsonPose` and `PoseAdapter`), the pose `PosableModel` picks among a poser's for an entity, and the
//! animations Cobblemon works out instead of reading them from an animation file (`q.look`,
//! `q.biped_walk`...).
//!
//! A poser names its animations in MoLang, which Cobblemon runs once, when it builds the model: each
//! string is asked for a `PoseAnimation`, and an entry that gives none is dropped without a word. Nothing
//! here runs MoLang. What is written as one call of a query function is read as that call ([`Call`]), cut
//! the way Cobblemon's own parser cuts it (`com.bedrockk.molang` 1.1.20, whose tokens these follow); the
//! rest is kept as written ([`Expression::Molang`]) for whoever has an evaluator.
//!
//! Conditions are MoLang too, run for each entity: they are kept as their source and answered by the
//! caller. Cobblemon has two ideas of a true one, so the caller answers with the number: a pose's
//! condition holds when it is exactly 1 (`ExpressionLike.resolveBoolean`), an animation's and a part's
//! visibility when it is anything but 0 (`MoLangRuntime.resolveBoolean`). A string counts as 1
//! (`MoValue.asDouble`).
//!
//! The JSON is read strictly where Gson is lenient, but for what the posers of Cobblemon 1.8.1 need: a
//! boolean written as a string, and a `poseName` written as a list of one.
use crate::{json, pose::Delta, wire::PoseType};
use horizon_protocol::{Error, Result};
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
};
use std::{fmt, marker::PhantomData};

/// One poser file (a `PokemonPosableModel`, as Gson fills it).
#[derive(Clone, Debug, PartialEq)]
pub struct Poser {
    /// `rootBone`: the name of the model's root part this poser draws, when the file gives one
    /// ([`crate::geo::Geometry::drawn_root`] finds the part).
    pub root_bone: Option<String>,
    /// `portraitScale` and `portraitTranslation`: how the model is framed where only its head shows
    /// (the party overlay, a battle's tiles), 1 and no move when the file says nothing
    /// (`drawPosablePortrait`).
    pub portrait_scale: f32,
    pub portrait_translation: [f32; 3],
    /// `profileScale` and `profileTranslation`: the same for the whole model in a screen (the summary).
    pub profile_scale: f32,
    pub profile_translation: [f32; 3],
    /// `profileSummaryScale` and `profileSummaryTranslation`: the summary's own, for the few posers
    /// whose model does not sit well there with the profile's.
    pub profile_summary_scale: Option<f32>,
    pub profile_summary_translation: Option<[f32; 3]>,
    /// In the order of the file, which is the order Cobblemon looks through them in: `poses` is an object
    /// keyed by pose name, and both of Gson's maps it goes through keep the order they were filled in.
    pub poses: Vec<Pose>,
    /// The top-level `animations`: what `cry`, `recoil`, `physical`... play in any pose that has no named
    /// animation of its own for them (`PosableModel.getAnimation`).
    pub named_animations: Vec<(String, Expression)>,
    /// The top-level `transformedParts`, which hold in every pose. Cobblemon applies them twice a frame,
    /// once in `setDefault` and once more in `applyAnimations`, before the pose's own: a part ends up moved
    /// and turned by twice the delta and scaled by its square. Four posers of Cobblemon 1.8.1 have some.
    pub transformed_parts: Vec<TransformedPart>,
    /// `properties`: open-ended words about the model, in the file's order. Cobblemon reads one kind:
    /// `<locator>_display_context`, the way an item shown at that locator is held
    /// ([`Self::display_context`]).
    pub properties: Vec<(String, String)>,
    /// What Cobblemon reads past in this file without a word, most likely not as its author meant.
    pub oddities: Vec<Oddity>,
}

/// One pose of a poser (`Pose`): what the model does while its entity is in one of `pose_types` and
/// `conditions` hold.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    /// The pose's key in `poses`. Cobblemon reads a `poseName` too, then puts the key in its place
    /// (`loadJsonPoser`).
    pub name: String,
    /// `poseTypes`, without repeats (Cobblemon makes a set of them), then every type when `allPoseTypes`
    /// is true. A name is matched whatever its case.
    pub pose_types: Vec<PoseType>,
    pub conditions: Conditions,
    /// `animations`, in the order Cobblemon runs them in every frame (`Pose.apply`).
    pub animations: Vec<PoseAnimation>,
    /// `quirks`: what the model does now and then on its own (a blink), each a call that gives Cobblemon a
    /// `SimpleQuirk`, such as `q.bedrock_quirk('pikachu', 'blink')`.
    pub quirks: Vec<Expression>,
    /// The quirks written as objects, the form that came before MoLang, after those of `quirks`.
    pub legacy_quirks: Vec<LegacyQuirk>,
    /// `namedAnimations`: what a named animation plays in this pose, ahead of the poser's own of that name.
    pub named_animations: Vec<(String, Expression)>,
    /// `transitions`: the name of another pose, and what Cobblemon plays to move from this one to it
    /// instead of blending (`moveToPose`).
    pub transitions: Vec<(String, Expression)>,
    /// `transformedParts`: applied whole in every frame the model is in this pose, before any animation
    /// (`applyPose`).
    pub transformed_parts: Vec<TransformedPart>,
    /// `transformTicks`, 10 when the file gives none: how many ticks Cobblemon blends for when it leaves
    /// this pose for one that has no `transform_to_ticks`. At 0 it does not blend at all.
    pub transform_ticks: u32,
    /// `transformToTicks`: how many ticks Cobblemon blends for when it enters this pose.
    pub transform_to_ticks: Option<u32>,
}

/// A quirk written as an object (`{"name": "blink", "animations": ["bedrock(marill, blink)"]}`), which
/// `JsonPose` still reads and resource packs still carry: every one of its animations plays together,
/// now and then.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyQuirk {
    /// `animations`, and `animation` after them: each MoLang that gives an active animation
    /// (`q.bedrock_stateful('marill', 'blink')`), or the older `bedrock(marill, blink)`, as written.
    pub animations: Vec<String>,
    /// `loopTimes`, 1 without one: it plays from once to that many times in a row.
    pub loop_times: i32,
    /// `minSecondsBetweenOccurrences` and `maxSecondsBetweenOccurrences`, 8 and 30 without them.
    pub seconds: (f32, f32),
    /// `condition`: MoLang, asked in every frame; the quirk only starts while it is not 0.
    pub condition: Option<String>,
}

/// What a pose asks of the entity, each None when the file does not ask (`PoseAdapter`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conditions {
    /// `isRideStyle`: the names of the `RidingStyle`s (`LAND`, `LIQUID`, `AIR`) it has to be ridden in one
    /// of. An entity that is not ridden fails it.
    pub ride_styles: Option<Vec<String>>,
    /// `isTouchingWater`.
    pub touching_water: Option<bool>,
    /// `isInWaterOrRain`.
    pub in_water_or_rain: Option<bool>,
    /// `isUnderWater`.
    pub under_water: Option<bool>,
    /// `isDusk`.
    pub dusk: Option<bool>,
    /// `isBattle`.
    pub battle: Option<bool>,
    /// `conditions` and then `condition`, MoLang sources: one of them has to be exactly 1. A pose with an
    /// empty `conditions` is never suitable.
    pub molang: Option<Vec<String>>,
    /// `isWild`, which twelve posers of Cobblemon 1.8.1 write (a `wild_sleep` pose after their `sleep`)
    /// and nothing of Cobblemon 1.8.1 reads: [`Conditions::hold`] does not look at it either.
    pub wild: Option<bool>,
}

/// One entry of a pose's `animations`.
#[derive(Clone, Debug, PartialEq)]
pub struct PoseAnimation {
    /// The `condition` of an entry written as an object: MoLang, asked in every frame, and the animation
    /// only runs while it is not 0. None for an entry written as a string, which always runs.
    pub condition: Option<String>,
    pub animation: Animation,
}

/// What a pose plays for as long as the model is in it.
///
/// Cobblemon finds the parts an animation names when it builds the model, and drops the animation whose
/// part the model lacks ([`Animation::bones`]). All of those it computes add to a part's rotation
/// (`addRotation`); none sets it.
#[derive(Clone, Debug, PartialEq)]
pub enum Animation {
    /// `q.bedrock('pikachu', 'ground_idle')`: the animation `animation.pikachu.ground_idle` of the group
    /// `pikachu`, which is the file `pikachu.animation.json` whatever folder it is in
    /// (`PosableModel.bedrock`). Arguments after the second are not read.
    Bedrock {
        group: String,
        name: String,
    },
    Look(Look),
    PitchTilt(PitchTilt),
    BipedWalk(BipedWalk),
    QuadrupedWalk(QuadrupedWalk),
    BimanualSwing(BimanualSwing),
    SineWingFlap(SineWingFlap),
    /// Anything else, kept as parsed. Either one of the functions above with an argument that is not
    /// written out, which Cobblemon works out as it builds the model
    /// (`q.bedrock('golem', math.random(0, 1) < 0.4 ? 'sleep2' : 'sleep')`), or something that gives
    /// Cobblemon no pose animation, which it drops: another function (`q.bedrock_stateful`,
    /// `q.bedrock_quirk`), an argument of the wrong kind, MoLang it cannot read.
    Other(Expression),
}

/// MoLang that stands for an animation or a quirk.
#[derive(Clone, Debug, PartialEq)]
pub enum Expression {
    /// Written as one call of a query function.
    Call(Call),
    /// Anything else, as written: MoLang that is more than one call (`q.is_ridden ? q.a('b') : q.c('d')`),
    /// a call in brackets or after `return`, a reference to a script (`cobblemon:some_script`), or
    /// nothing Cobblemon can read.
    Molang(String),
}

/// `q.<function>(<arguments>)`, or `q.<function>` alone, which MoLang calls without arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// In lower case, as MoLang takes names: `bedrock` for `q.bedrock` and for `query.BEDROCK`.
    pub function: String,
    pub arguments: Vec<Argument>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Argument {
    /// `'ground_idle'`. Only single quotes make a string.
    String(String),
    /// `0.5`, `-45`. A name that none of MoLang's structs has (`O.5`, typed for `0.5`) is a 0 here, as it
    /// is to Cobblemon, and an [`OddityKind::UnknownName`].
    Number(f64),
    /// `q.curve('symmetrical_wide')`, `q.array('quirk1', 'quirk2')`.
    Call(Call),
    /// Anything else, as written and not looked into, so not sure to be MoLang Cobblemon can read:
    /// `math.random(0, 1) < 0.4 ? 'sleep2' : 'sleep'`, `q.is_ridden`, `(1)`.
    Molang(String),
}

/// One entry of `transformedParts` (`ModelPartTransformation`): a part of the model held differently.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformedPart {
    /// The part's name. Cobblemon fails to build the model when it has no such part.
    pub part: String,
    /// `position`, `rotation` and `scale`, already in part space: the position is added as written
    /// (pixels, Y down, not a Bedrock animation's upward one), the rotation is written in degrees and
    /// added with its signs kept (divided by 57.2958, `toRadians`), the scale multiplies.
    pub delta: Delta,
    /// `isVisible`: whether the part and all under it are drawn. None leaves the part as the model has it.
    /// A `visible` key is not read (one transformed part of Cobblemon 1.8.1 writes that).
    pub visible: Option<Visible>,
}

/// `isVisible`, which Cobblemon reads as MoLang whatever it is written as.
#[derive(Clone, Debug, PartialEq)]
pub enum Visible {
    /// Written as a boolean.
    Is(bool),
    /// Written as a string: MoLang, asked in every frame, visible while it is not 0
    /// (`q.has_aspect('regrown-tail-1')`).
    Molang(String),
}

impl Visible {
    /// Whether the part is drawn, `molang` giving the value of a source.
    pub fn get(&self, molang: &mut dyn FnMut(&str) -> f64) -> bool {
        match self {
            Self::Is(visible) => *visible,
            Self::Molang(source) => molang(source) != 0.0,
        }
    }
}

/// Something Cobblemon reads past without a word.
#[derive(Clone, Debug, PartialEq)]
pub struct Oddity {
    /// The pose it is in, None for the poser's own `animations`.
    pub pose: Option<String>,
    /// The string it is in, empty when it is not in one.
    pub source: String,
    pub kind: OddityKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OddityKind {
    /// What follows the first expression of the string without a `;` between them. Cobblemon's parser
    /// stops there, so `q.bedrock_quirk('goodra', 'idle_quirk'), 30, 60, 1)` is
    /// `q.bedrock_quirk('goodra', 'idle_quirk')`, and is read as that here.
    Unread(String),
    /// A name none of MoLang's structs has (`query`, `variable`, `temp`, `context`, `math`, `array`),
    /// given as an argument: it is worth 0.
    UnknownName(String),
    /// An object among a pose's `animations` without an `animation`. Cobblemon skips it, and it is not
    /// among [`Pose::animations`].
    NoAnimation,
    /// A quirk written as an object (`{"name": "blink", "animations": ["bedrock(marill, blink)"]}`),
    /// Cobblemon's older form, which `JsonPose` still reads and resource packs still carry. It is not
    /// among [`Pose::quirks`].
    LegacyQuirk,
}

/// `RidingStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RideStyle {
    Land,
    Liquid,
    Air,
}

impl RideStyle {
    /// The name `isRideStyle` knows it by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Land => "LAND",
            Self::Liquid => "LIQUID",
            Self::Air => "AIR",
        }
    }
}

/// What a pose's conditions read of an entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entity {
    /// `getCurrentPoseType`: the pose the server gives it ([`crate::wire::SpawnPokemon::pose`], then the
    /// entity's metadata).
    pub pose: PoseType,
    /// `PokemonEntity.isBattling`: it has a battle id ([`crate::wire::SpawnPokemon::battle`]).
    pub battle: bool,
    /// Vanilla's `Entity.isInWater`.
    pub touching_water: bool,
    /// Vanilla's `Entity.isUnderWater`.
    pub under_water: bool,
    /// Vanilla's `Entity.isInWaterOrRain`.
    pub in_water_or_rain: bool,
    /// Whether the world's day time, modulo 24000, is from 12000 to 13000 (`Entity.isDusk`).
    pub dusk: bool,
    /// The style it is being ridden in (`ridingController.context.style`), None when it is not ridden.
    pub ride_style: Option<RideStyle>,
}

impl Default for Entity {
    /// A Pokémon standing on dry land by day, in no battle: `STAND` is the pose a `PokemonEntity` has
    /// until the server gives it another.
    fn default() -> Self {
        Self {
            pose: PoseType::Stand,
            battle: false,
            touching_water: false,
            under_water: false,
            in_water_or_rain: false,
            dusk: false,
            ride_style: None,
        }
    }
}

impl Conditions {
    /// Whether the entity meets them all (`Pose.isSuitable`), `molang` giving the value of a condition's
    /// source. They are asked in `PoseAdapter`'s order and no further than the first that fails, so the
    /// MoLang last.
    pub fn hold(&self, entity: &Entity, molang: &mut dyn FnMut(&str) -> f64) -> bool {
        let is = |wanted: Option<bool>, actual: bool| wanted.is_none_or(|wanted| wanted == actual);
        self.ride_styles.as_ref().is_none_or(|styles| {
            entity
                .ride_style
                .is_some_and(|style| styles.iter().any(|name| name == style.name()))
        }) && is(self.touching_water, entity.touching_water)
            && is(self.in_water_or_rain, entity.in_water_or_rain)
            && is(self.under_water, entity.under_water)
            && is(self.dusk, entity.dusk)
            && is(self.battle, entity.battle)
            && self.molang.as_ref().is_none_or(|conditions| {
                conditions.iter().any(|condition| molang(condition) == 1.0)
            })
    }
}

impl Pose {
    /// Whether the entity can be in this pose: it is one of the pose's types and meets its conditions.
    pub fn fits(&self, entity: &Entity, molang: &mut dyn FnMut(&str) -> f64) -> bool {
        self.pose_types.contains(&entity.pose) && self.conditions.hold(entity, molang)
    }
}

impl PoseAnimation {
    /// Whether the animation runs in this frame (`PoseAnimation.apply`), `molang` giving the value of the
    /// condition's source.
    pub fn runs(&self, molang: &mut dyn FnMut(&str) -> f64) -> bool {
        self.condition
            .as_deref()
            .is_none_or(|condition| molang(condition) != 0.0)
    }
}

impl Poser {
    /// `PosableModel.getLocatorDisplayContext`: the name of the `ItemDisplayContext` an item shown at
    /// `locator` is drawn in (`THIRD_PERSON_RIGHT_HAND` for a model that holds it in its hand), None
    /// for a model that does not say.
    pub fn display_context(&self, locator: &str) -> Option<&str> {
        let wanted = format!("{locator}_display_context");
        let found = self.properties.iter().rev().find(|(key, _)| *key == wanted);
        found.map(|(_, context)| context.as_str())
    }

    /// The pose Cobblemon picks for an entity that is in none, or in one that no longer fits
    /// (`getFirstSuitablePose`): the first, in the file's order, that the entity [`Pose::fits`], and
    /// when none does the file's first pose, whatever its types and conditions. None for a poser without
    /// poses, which Cobblemon cannot draw. `molang` gives the value of a condition's source.
    ///
    /// Cobblemon does not ask in every frame (`validatePose`): the entity stays in its pose for as long as
    /// it fits it, even when an earlier pose has come to fit too. When it has to change, it changes at
    /// once if it was in no pose; otherwise it waits for the primary animation that is playing, then
    /// plays the old pose's [`Pose::transitions`] entry for the new one, or blends for the new pose's
    /// [`Pose::transform_to_ticks`], or for the old pose's [`Pose::transform_ticks`] (`moveToPose`).
    pub fn pose_for(&self, entity: &Entity, molang: &mut dyn FnMut(&str) -> f64) -> Option<&Pose> {
        self.poses
            .iter()
            .find(|pose| pose.fits(entity, molang))
            .or(self.poses.first())
    }
}

/// What the animations Cobblemon computes read in one frame: the arguments of `PoseAnimation.setupAnim`,
/// which are those vanilla's `LivingEntityRenderer` hands a model, and what else they ask the entity and
/// its `PosableState`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    /// `limbSwing`: how far the entity has walked (`walkAnimation.position`).
    pub limb_swing: f32,
    /// `limbSwingAmount`: how fast it is walking, from 0 to 1 (`walkAnimation.speed`).
    pub limb_swing_amount: f32,
    /// `ageInTicks`: the entity's age in ticks, with the partial tick.
    pub age_in_ticks: f32,
    /// `PosableState.animationSeconds`: the age in ticks the state counts for itself, with the partial
    /// tick, over 20.
    pub animation_seconds: f32,
    /// `headYaw`, degrees: the head's yaw less the body's. Cobblemon gives 0 for a ridden Pokémon whose
    /// ride says not to turn its head.
    pub head_yaw: f32,
    /// `headPitch`, degrees, 0 likewise.
    pub head_pitch: f32,
    /// `Entity.getDeltaMovement`, blocks per tick.
    pub velocity: [f64; 3],
    /// Whether the entity has passengers.
    pub ridden: bool,
    /// `PosableState.getIdleIntensity`: 1, but while a primary animation that excludes the animation
    /// fades in or out, where it is what that leaves (`poseIntensity`).
    pub intensity: f32,
}

impl Default for Motion {
    /// An entity that stands still and looks straight ahead, at full intensity.
    fn default() -> Self {
        Self {
            limb_swing: 0.0,
            limb_swing_amount: 0.0,
            age_in_ticks: 0.0,
            animation_seconds: 0.0,
            head_yaw: 0.0,
            head_pitch: 0.0,
            velocity: [0.0; 3],
            ridden: false,
            intensity: 1.0,
        }
    }
}

/// What `PitchTiltAnimation` keeps in an entity's `PosableState`, for itself and for
/// `SingleBoneLookAnimation`, which leans the head back by as much as the body was tilted. One per entity,
/// handed to every animation of the frame in their order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tilt {
    /// `numbers["previous_pitch_tilt_angle"]`, degrees: how far the last pitch tilt had tilted.
    pub previous: Option<f32>,
    /// `numbers["corrected_pitch_tilt_angle"]`, degrees: what the last look took back of it.
    pub corrected: Option<f32>,
    /// The render marker `has_pitched_tilt`: a pitch tilt has run in this frame.
    pub pitched: bool,
}

impl Tilt {
    /// Before a frame's animations, as `applyAnimations` clears the state's render markers.
    pub fn begin_frame(&mut self) {
        self.pitched = false;
    }
}

/// `x`, `y` or `z`, of a part's rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// `q.look(bone, pitchMultiplier = 1, yawMultiplier = 1, maxPitch = 70, minPitch = -45, maxYaw = 45,
/// minYaw = -45)`: the bone follows the head (`SingleBoneLookAnimation`). The limits are degrees.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub bone: String,
    /// The bone that turns when the model has no `bone`. Only the bare word `look`, an older way to
    /// write this, has one: `head_ai`, or else `head`, and nothing turns in a model with neither.
    pub or_bone: Option<String>,
    pub pitch_multiplier: f32,
    pub yaw_multiplier: f32,
    pub max_pitch: f32,
    pub min_pitch: f32,
    pub max_yaw: f32,
    pub min_yaw: f32,
}

/// `q.pitch_tilt(bone = 'root', maxChangePerTick = 1.5, minPitch = -45, maxPitch = 45)`: the bone leans
/// the way the entity is moving, up or down (`PitchTiltAnimation`). The limits are degrees.
#[derive(Clone, Debug, PartialEq)]
pub struct PitchTilt {
    pub bone: String,
    /// Degrees. Despite its name it is what the tilt changes by in one frame at most: Cobblemon runs this
    /// in every frame it draws, so the tilt turns faster at a higher frame rate.
    pub max_change_per_tick: f32,
    pub min_pitch: f32,
    pub max_pitch: f32,
}

/// `q.biped_walk(periodMultiplier = 0.6662, amplitudeMultiplier = 1.4, leftLeg = 'leg_left', rightLeg =
/// 'leg_right')`: two legs swing in turn (`BipedWalkAnimation`).
#[derive(Clone, Debug, PartialEq)]
pub struct BipedWalk {
    pub period_multiplier: f32,
    pub amplitude_multiplier: f32,
    pub left_leg: String,
    pub right_leg: String,
}

/// `q.quadruped_walk(periodMultiplier = 0.6662, amplitudeMultiplier = 1.4, legFrontLeft =
/// 'leg_front_left', legFrontRight = 'leg_front_right', legBackLeft = 'leg_back_left', legBackRight =
/// 'leg_back_right')`: four legs swing, each diagonal pair together (`QuadrupedWalkAnimation`).
#[derive(Clone, Debug, PartialEq)]
pub struct QuadrupedWalk {
    pub period_multiplier: f32,
    pub amplitude_multiplier: f32,
    pub leg_front_left: String,
    pub leg_front_right: String,
    pub leg_back_left: String,
    pub leg_back_right: String,
}

/// `q.bimanual_swing(swingPeriodMultiplier = 0.6662, amplitudeMultiplier = 1, leftArm = 'arm_left',
/// rightArm = 'arm_right')`: two arms swing as the entity walks and sway while it lives
/// (`BimanualSwingAnimation`).
#[derive(Clone, Debug, PartialEq)]
pub struct BimanualSwing {
    pub swing_period_multiplier: f32,
    pub amplitude_multiplier: f32,
    pub left_arm: String,
    pub right_arm: String,
}

/// `q.sine_wing_flap(amplitude = 0.9, period = 0.9, verticalShift = 0, axis = 'y', wingLeft =
/// 'wing_left', wingRight = 'wing_right')`: two wings beat against each other (`WingFlapIdleAnimation`
/// over a `sineFunction`).
#[derive(Clone, Debug, PartialEq)]
pub struct SineWingFlap {
    /// Radians.
    pub amplitude: f32,
    /// Seconds.
    pub period: f32,
    /// Radians, though the posers of Cobblemon 1.8.1 write it as if it were degrees (25 in most).
    pub vertical_shift: f32,
    /// `x`, `y` or `z`; `y` for anything else.
    pub axis: Axis,
    pub wing_left: String,
    pub wing_right: String,
}

/// `RADIAN_IN_DEGREES`, which is how Cobblemon's `toRadians` and `toDegrees` convert.
const RADIAN_IN_DEGREES: f32 = 57.2958;

fn radians(degrees: f32) -> f32 {
    degrees / RADIAN_IN_DEGREES
}

/// `Math.PI.toFloat()` and `Mth.PI`.
const PI: f32 = std::f32::consts::PI;

/// Kotlin's `coerceIn`: None where it throws, which is when the range is empty.
fn coerce(value: f32, min: f32, max: f32) -> Option<f32> {
    if min > max {
        None
    } else if value < min {
        Some(min)
    } else if value > max {
        Some(max)
    } else {
        Some(value)
    }
}

/// An entry of vanilla 1.21.1's `Mth.SIN`, its table of 65,536 sines around the circle, worked out
/// instead of kept.
fn sine_table(index: i32) -> f32 {
    (f64::from(index & 65535) * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32
}

/// `Mth.sin`.
fn mth_sin(angle: f32) -> f32 {
    sine_table((angle * 10430.378) as i32)
}

/// `Mth.cos`.
fn mth_cos(angle: f32) -> f32 {
    sine_table((angle * 10430.378 + 16384.0) as i32)
}

/// A turn around one axis.
fn turn(axis: Axis, angle: f32) -> Delta {
    let mut delta = Delta::NONE;
    delta.rotation[axis as usize] = angle;
    delta
}

impl Look {
    /// The bare word `look`, as `JsonPose` reads it for a model that is no `HeadedFrame`, which no
    /// Pokémon's is.
    fn bare() -> Self {
        Self {
            bone: "head_ai".to_owned(),
            or_bone: Some("head".to_owned()),
            pitch_multiplier: 1.0,
            yaw_multiplier: 1.0,
            max_pitch: 70.0,
            min_pitch: -45.0,
            max_yaw: 45.0,
            min_yaw: -45.0,
        }
    }

    /// What the bone turns by, to add to its rotation:
    ///
    /// - x: `radians(pitch_multiplier * head_pitch.coerceIn(min_pitch, max_pitch)) * intensity
    ///   - radians(counter)`,
    /// - y: `radians(yaw_multiplier * head_yaw.coerceIn(min_yaw, max_yaw)) * intensity`,
    ///
    /// `counter` being, in degrees, what a pitch tilt that ran earlier in this frame tilted by
    /// ([`Tilt::previous`]) less what a look has already taken back ([`Tilt::corrected`]), and 0 when none
    /// ran. It becomes the new [`Tilt::corrected`].
    ///
    /// None, with `tilt` left alone, where Cobblemon throws: when a minimum is above its maximum.
    pub fn delta(&self, motion: &Motion, tilt: &mut Tilt) -> Option<Delta> {
        let pitch =
            self.pitch_multiplier * coerce(motion.head_pitch, self.min_pitch, self.max_pitch)?;
        let yaw = self.yaw_multiplier * coerce(motion.head_yaw, self.min_yaw, self.max_yaw)?;
        let mut counter = 0.0;
        if tilt.pitched {
            counter = tilt.previous.unwrap_or(0.0) - tilt.corrected.unwrap_or(0.0);
        }
        tilt.corrected = Some(counter);
        Some(Delta {
            rotation: [
                radians(pitch) * motion.intensity - radians(counter),
                radians(yaw) * motion.intensity,
                0.0,
            ],
            ..Delta::NONE
        })
    }

    /// Reports [`Look::delta`] for `bone`. For the bare `look` in a model without a `head_ai`, it is
    /// `or_bone` that takes it.
    pub fn apply(&self, motion: &Motion, tilt: &mut Tilt, out: &mut dyn FnMut(&str, Delta)) {
        if let Some(delta) = self.delta(motion, tilt) {
            out(&self.bone, delta);
        }
    }
}

impl PitchTilt {
    /// Adds to the bone's x rotation. With `angle` the slope of the entity's motion in degrees, upwards
    /// positive (`atan2(y, hypot(x, z))`; the maximum, the minimum or 0 for a motion with no horizontal
    /// part), `target = -angle.coerceIn(min_pitch, max_pitch)` and
    /// `change = (target - previous).coerceIn(-max_change_per_tick, max_change_per_tick)`, `previous`
    /// being [`Tilt::previous`] or 0:
    ///
    /// - the frame's first pitch tilt adds `radians((previous + change) * intensity)`, and that angle is
    ///   the new `previous`;
    /// - a later one adds `radians(change) * intensity`, and `previous + change * intensity` is the new
    ///   `previous`.
    ///
    /// Nothing for an entity with passengers, nor where Cobblemon throws (a minimum above its maximum, a
    /// negative `max_change_per_tick`).
    ///
    /// Cobblemon takes the slope from vanilla's `Mth.atan2`, a table that is off by up to eight
    /// millionths of a radian; this takes it from the real arctangent.
    pub fn apply(&self, motion: &Motion, tilt: &mut Tilt, out: &mut dyn FnMut(&str, Delta)) {
        if motion.ridden {
            return;
        }
        let [x, vertical, z] = motion.velocity;
        let horizontal = (x * x + z * z).sqrt();
        let angle = if horizontal == 0.0 {
            if vertical > 0.0 {
                self.max_pitch
            } else if vertical < 0.0 {
                self.min_pitch
            } else {
                0.0
            }
        } else {
            vertical.atan2(horizontal) as f32 * RADIAN_IN_DEGREES
        };
        let Some(angle) = coerce(angle, self.min_pitch, self.max_pitch) else {
            return;
        };
        let previous = tilt.previous.unwrap_or(0.0);
        let Some(change) = coerce(
            -angle - previous,
            -self.max_change_per_tick,
            self.max_change_per_tick,
        ) else {
            return;
        };
        if tilt.pitched {
            out(
                &self.bone,
                turn(Axis::X, radians(change) * motion.intensity),
            );
            tilt.previous = Some(previous + change * motion.intensity);
        } else {
            tilt.corrected = None;
            let corrected = (previous + change) * motion.intensity;
            out(&self.bone, turn(Axis::X, radians(corrected)));
            tilt.previous = Some(corrected);
        }
        tilt.pitched = true;
    }
}

impl BipedWalk {
    /// Adds to each leg's x rotation, in radians:
    ///
    /// - right: `Mth.cos(limb_swing * period_multiplier + PI) * limb_swing_amount * amplitude_multiplier
    ///   * intensity`,
    /// - left: the same without the `PI`.
    pub fn apply(&self, motion: &Motion, out: &mut dyn FnMut(&str, Delta)) {
        let swing = motion.limb_swing * self.period_multiplier;
        let stride = |cos: f32| {
            turn(
                Axis::X,
                cos * motion.limb_swing_amount * self.amplitude_multiplier * motion.intensity,
            )
        };
        out(&self.right_leg, stride(mth_cos(swing + PI)));
        out(&self.left_leg, stride(mth_cos(swing)));
    }
}

impl QuadrupedWalk {
    /// Adds to each leg's x rotation, in radians:
    ///
    /// - back right and front left: `Mth.cos(limb_swing * period_multiplier) * limb_swing_amount
    ///   * amplitude_multiplier * intensity`,
    /// - back left and front right: the same with `+ PI` inside the cosine.
    pub fn apply(&self, motion: &Motion, out: &mut dyn FnMut(&str, Delta)) {
        let swing = motion.limb_swing * self.period_multiplier;
        let stride = |cos: f32| {
            turn(
                Axis::X,
                cos * motion.limb_swing_amount * self.amplitude_multiplier * motion.intensity,
            )
        };
        out(&self.leg_back_right, stride(mth_cos(swing)));
        out(&self.leg_back_left, stride(mth_cos(swing + PI)));
        out(&self.leg_front_right, stride(mth_cos(swing + PI)));
        out(&self.leg_front_left, stride(mth_cos(swing)));
    }
}

impl BimanualSwing {
    /// Adds to each arm's rotation, in radians. Both arms swing around y by
    /// `Mth.cos(limb_swing * swing_period_multiplier) * limb_swing_amount * amplitude_multiplier
    /// * intensity`, and then sway:
    ///
    /// - right: z by `(Mth.cos(age_in_ticks * 0.09) * 0.05 + 0.05) * intensity`, y by
    ///   `Mth.sin(age_in_ticks * 0.067) * 0.05 * intensity`,
    /// - left: the opposite of both.
    ///
    /// Each arm is reported twice, its swing and then its sway, as Cobblemon adds them.
    pub fn apply(&self, motion: &Motion, out: &mut dyn FnMut(&str, Delta)) {
        let swing = turn(
            Axis::Y,
            mth_cos(motion.limb_swing * self.swing_period_multiplier)
                * motion.limb_swing_amount
                * self.amplitude_multiplier
                * motion.intensity,
        );
        out(&self.right_arm, swing);
        out(&self.left_arm, swing);
        let roll = mth_cos(motion.age_in_ticks * 0.09) * 0.05 + 0.05;
        let yaw = mth_sin(motion.age_in_ticks * 0.067) * 0.05;
        let sway = |side: f32| Delta {
            rotation: [
                0.0,
                side * yaw * motion.intensity,
                side * roll * motion.intensity,
            ],
            ..Delta::NONE
        };
        out(&self.right_arm, sway(1.0));
        out(&self.left_arm, sway(-1.0));
    }
}

impl SineWingFlap {
    /// Adds `sin(2 * PI / period * animation_seconds) * amplitude + vertical_shift`, in radians, to the
    /// left wing's rotation around `axis`, and takes it from the right wing's. The sine is the real one,
    /// and the intensity is not read.
    pub fn apply(&self, motion: &Motion, out: &mut dyn FnMut(&str, Delta)) {
        let phase = 2.0 * PI / self.period * motion.animation_seconds;
        let angle = f64::from(phase).sin() as f32 * self.amplitude + self.vertical_shift;
        out(&self.wing_left, turn(self.axis, angle));
        out(&self.wing_right, turn(self.axis, -angle));
    }
}

impl Animation {
    /// What the animation turns parts by in this frame, one report per part and delta. Nothing for
    /// [`Animation::Bedrock`], whose keyframes are in its animation file, nor for [`Animation::Other`].
    pub fn apply(&self, motion: &Motion, tilt: &mut Tilt, out: &mut dyn FnMut(&str, Delta)) {
        match self {
            Self::Look(look) => look.apply(motion, tilt, out),
            Self::PitchTilt(pitch_tilt) => pitch_tilt.apply(motion, tilt, out),
            Self::BipedWalk(walk) => walk.apply(motion, out),
            Self::QuadrupedWalk(walk) => walk.apply(motion, out),
            Self::BimanualSwing(swing) => swing.apply(motion, out),
            Self::SineWingFlap(flap) => flap.apply(motion, out),
            Self::Bedrock { .. } | Self::Other(_) => {}
        }
    }

    /// The parts the animation names. Cobblemon looks them up as it builds the model (`getPart`) and
    /// drops the animation when the model lacks one of them; when the entry has a condition, it fails
    /// to build the model instead. The bare `look` is the exception: it wants its [`Look::bone`] or else
    /// its [`Look::or_bone`], both listed, and stays without either.
    pub fn bones(&self) -> Vec<&str> {
        let bones: Vec<&String> = match self {
            Self::Look(look) => std::iter::once(&look.bone).chain(&look.or_bone).collect(),
            Self::PitchTilt(pitch_tilt) => vec![&pitch_tilt.bone],
            Self::BipedWalk(walk) => vec![&walk.left_leg, &walk.right_leg],
            Self::QuadrupedWalk(walk) => vec![
                &walk.leg_front_left,
                &walk.leg_front_right,
                &walk.leg_back_left,
                &walk.leg_back_right,
            ],
            Self::BimanualSwing(swing) => vec![&swing.left_arm, &swing.right_arm],
            Self::SineWingFlap(flap) => vec![&flap.wing_left, &flap.wing_right],
            Self::Bedrock { .. } | Self::Other(_) => Vec::new(),
        };
        bones.into_iter().map(String::as_str).collect()
    }
}

/// One of the pieces Cobblemon's `TokenIterator` cuts MoLang into.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    /// A letter, then letters, digits, `_` and `.`, as written. MoLang lowers its case.
    Name(&'a str),
    /// A name that is one of [`KEYWORDS`].
    Keyword,
    /// A digit, then digits and at most one `.`.
    Number(&'a str),
    /// What is between two `'`, a `\'` not ending it and still written so.
    String(&'a str),
    /// One of [`SYMBOLS`].
    Symbol(&'static str),
    End,
}

/// `TokenType`'s symbols, the longer ones first, as the tokenizer tries them.
const SYMBOLS: [&str; 26] = [
    "==", "!=", "??", "&&", "||", ">=", "<=", "->", ">", "<", "(", ")", "[", "]", "{", "}", ",",
    "=", "+", "-", "*", "/", "?", ":", ";", "!",
];

/// The names `TokenType` takes for itself.
const KEYWORDS: [&str; 8] = [
    "return", "continue", "break", "for_each", "loop", "this", "true", "false",
];

/// The symbols that carry an expression on (`MoLangParser.infixParselets`).
const INFIX: [&str; 17] = [
    "?", "[", "+", "-", "/", "*", "==", "!=", ">", ">=", "<", "<=", "&&", "||", "??", "->", "=",
];

/// The tokens of `source`, each with where it starts, [`Token::End`] last. Whatever is no token is skipped,
/// as Cobblemon's tokenizer skips it: spaces, a `"`, a `.` on its own. None for a source with anything
/// outside ASCII that is not in a string, which Java might take for a letter or a digit where this would
/// not.
fn tokens(source: &str) -> Option<Vec<(Token<'_>, usize)>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let start = at;
        let token = if let Some(&symbol) = SYMBOLS
            .iter()
            .find(|symbol| bytes[at..].starts_with(symbol.as_bytes()))
        {
            at += symbol.len();
            Token::Symbol(symbol)
        } else if byte == b'\'' {
            let mut end = at + 1;
            while bytes
                .get(end)
                .is_some_and(|&next| next != b'\'' || bytes[end - 1] == b'\\' && end - 1 > at)
            {
                end += 1;
            }
            at = end + 1;
            Token::String(&source[start + 1..end])
        } else if byte.is_ascii_alphabetic() {
            while bytes
                .get(at)
                .is_some_and(|&next| next.is_ascii_alphanumeric() || next == b'_' || next == b'.')
            {
                at += 1;
            }
            let name = &source[start..at];
            if KEYWORDS
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(name))
            {
                Token::Keyword
            } else {
                Token::Name(name)
            }
        } else if byte.is_ascii_digit() {
            let mut dot = false;
            while bytes.get(at).is_some_and(|&next| {
                next.is_ascii_digit() || next == b'.' && !std::mem::replace(&mut dot, true)
            }) {
                at += 1;
            }
            Token::Number(&source[start..at])
        } else if byte.is_ascii() {
            at += 1;
            continue;
        } else {
            return None;
        };
        if bytes.get(at).is_some_and(|next| !next.is_ascii())
            && matches!(token, Token::Name(_) | Token::Keyword | Token::Number(_))
        {
            return None;
        }
        tokens.push((token, start));
    }
    tokens.push((Token::End, source.len()));
    Some(tokens)
}

/// The function a name calls, when the name is `q.<function>` or `query.<function>`.
fn query_function(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    // Java's `split` leaves out the empty strings a name ending in dots would end with.
    let mut parts = name.trim_end_matches('.').split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("q" | "query"), Some(function), None) if !function.is_empty() => {
            Some(function.to_owned())
        }
        _ => None,
    }
}

/// Whether a name starts with one of the structs MoLang has (`MoLangEnvironment.getStruct`, and the
/// short names `MoLangParser.fixNameShortcut` knows). A name that does not is worth 0.
fn in_a_struct(name: &str) -> bool {
    let head = name.split('.').next().unwrap_or_default();
    [
        "q", "query", "v", "variable", "t", "temp", "c", "context", "math", "array",
    ]
    .iter()
    .any(|known| known.eq_ignore_ascii_case(head))
}

/// `ResourceLocation.tryParse` taking the whole source, with a `:` in it: Cobblemon then reads a reference
/// to a script, not MoLang (`asExpressionLike`).
fn is_reference(source: &str) -> bool {
    let valid = |part: &str, slash: bool| {
        part.bytes().all(|b| {
            matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.') || (slash && b == b'/')
        })
    };
    source
        .split_once(':')
        .is_some_and(|(namespace, path)| valid(namespace, false) && valid(path, true))
}

/// How deep calls may be among a call's arguments before they are kept as written. They are read by
/// recursion, which a file is not allowed to take arbitrarily deep; no poser of Cobblemon 1.8.1 has more
/// than a call in a call.
const MAX_DEPTH: usize = 16;

/// Cobblemon's parser, for as much of MoLang as a call takes.
struct Reader<'a> {
    source: &'a str,
    tokens: Vec<(Token<'a>, usize)>,
    at: usize,
    /// How many calls the next token is inside the arguments of.
    depth: usize,
}

impl<'a> Reader<'a> {
    /// The token `ahead` of the next one.
    fn peek(&self, ahead: usize) -> Token<'a> {
        self.tokens
            .get(self.at + ahead)
            .map_or(Token::End, |&(token, _)| token)
    }

    /// Where the next token starts.
    fn position(&self) -> usize {
        self.tokens
            .get(self.at)
            .map_or(self.source.len(), |&(_, start)| start)
    }

    fn ends_an_argument(&self) -> bool {
        matches!(self.peek(0), Token::Symbol("," | ")"))
    }

    /// A query function's name and what it is called with (`NameParselet`, `parseArgs`). None when the
    /// next tokens are no such call, or one Cobblemon's parser fails on.
    fn call(&mut self, notes: &mut Vec<OddityKind>) -> Option<Call> {
        let Token::Name(name) = self.peek(0) else {
            return None;
        };
        let function = query_function(name)?;
        self.at += 1;
        let mut arguments = Vec::new();
        if self.peek(0) == Token::Symbol("(") {
            self.at += 1;
            if self.peek(0) == Token::Symbol(")") {
                self.at += 1;
            } else {
                loop {
                    arguments.push(self.argument(notes)?);
                    let closed = self.peek(0) == Token::Symbol(")");
                    if !closed && self.peek(0) != Token::Symbol(",") {
                        return None;
                    }
                    self.at += 1;
                    if closed {
                        break;
                    }
                }
            }
        }
        Some(Call {
            function,
            arguments,
        })
    }

    /// One argument, up to the `,` or the `)` after it.
    fn argument(&mut self, notes: &mut Vec<OddityKind>) -> Option<Argument> {
        let start = self.at;
        let mut found = Vec::new();
        if let Some(argument) = self.plain_argument(&mut found)
            && self.ends_an_argument()
        {
            notes.append(&mut found);
            return Some(argument);
        }
        // Anything else is kept as written, up to the `,` or the `)` that is in no bracket of its own.
        self.at = start;
        let mut depth = 0usize;
        loop {
            match self.peek(0) {
                Token::End => return None,
                Token::Symbol("(" | "[" | "{") => depth += 1,
                Token::Symbol(")" | "]" | "}") if depth > 0 => depth -= 1,
                Token::Symbol("," | ")") if depth == 0 => break,
                Token::Symbol(";") if depth == 0 => return None,
                _ => {}
            }
            self.at += 1;
        }
        let written = self.source[self.tokens[start].1..self.position()].trim();
        (!written.is_empty()).then(|| Argument::Molang(written.to_owned()))
    }

    /// An argument that is a string, a number with its signs, a call of a query function, or a name worth
    /// 0. The tokens after it are not looked at.
    fn plain_argument(&mut self, notes: &mut Vec<OddityKind>) -> Option<Argument> {
        match self.peek(0) {
            Token::String(written) => {
                self.at += 1;
                Some(Argument::String(written.replace("\\'", "'")))
            }
            Token::Number(_) | Token::Symbol("-" | "+") => {
                let mut negative = false;
                while let Token::Symbol(sign @ ("-" | "+")) = self.peek(0) {
                    negative ^= sign == "-";
                    self.at += 1;
                }
                let Token::Number(written) = self.peek(0) else {
                    return None;
                };
                self.at += 1;
                let number: f64 = written.parse().ok()?;
                Some(Argument::Number(if negative { -number } else { number }))
            }
            Token::Name(_) if self.peek(1) == Token::Symbol("(") && self.depth < MAX_DEPTH => {
                self.depth += 1;
                let call = self.call(notes);
                self.depth -= 1;
                call.map(Argument::Call)
            }
            Token::Name(name) if !in_a_struct(name) => {
                self.at += 1;
                notes.push(OddityKind::UnknownName(name.to_owned()));
                Some(Argument::Number(0.0))
            }
            _ => None,
        }
    }
}

impl Expression {
    /// Reads MoLang that stands for an animation: a pose's, a quirk, or one a server names. One call of a
    /// query function, with or without a `;` after it, is an [`Expression::Call`]; so is such a call
    /// followed by what Cobblemon's parser never reads ([`OddityKind::Unread`]). Anything else is kept
    /// as written.
    pub fn parse(source: &str) -> Self {
        Self::read(source, &mut Vec::new())
    }

    fn read(source: &str, notes: &mut Vec<OddityKind>) -> Self {
        let written = || Self::Molang(source.to_owned());
        if is_reference(source) {
            return written();
        }
        let Some(tokens) = tokens(source) else {
            return written();
        };
        let mut reader = Reader {
            source,
            tokens,
            at: 0,
            depth: 0,
        };
        let mut found = Vec::new();
        let Some(call) = reader.call(&mut found) else {
            return written();
        };
        match reader.peek(0) {
            Token::End => {}
            // One statement and its `;`. With more of them it is the last one's value that counts.
            Token::Symbol(";") if reader.peek(1) == Token::End => {}
            Token::Symbol(";") => return written(),
            // The call is only a part of the expression.
            Token::Symbol(symbol) if INFIX.contains(&symbol) => return written(),
            _ => found.push(OddityKind::Unread(
                source[reader.position()..].trim().to_owned(),
            )),
        }
        notes.append(&mut found);
        Self::Call(call)
    }
}

/// A call's arguments, as `MoParams` hands them to a function: a missing one is its default, one of the
/// wrong kind fails the call (`getString` and `getDouble` cast), and so does one that is not written out,
/// since nothing here works it out.
struct Arguments<'a>(&'a [Argument]);

impl Arguments<'_> {
    fn string(&self, index: usize) -> Option<String> {
        match self.0.get(index)? {
            Argument::String(string) => Some(string.clone()),
            _ => None,
        }
    }

    fn string_or(&self, index: usize, default: &str) -> Option<String> {
        if index < self.0.len() {
            self.string(index)
        } else {
            Some(default.to_owned())
        }
    }

    fn number_or(&self, index: usize, default: f32) -> Option<f32> {
        match self.0.get(index) {
            None => Some(default),
            Some(Argument::Number(number)) => Some(*number as f32),
            Some(_) => None,
        }
    }
}

/// The pose animation a call gives, with the arguments and defaults of `ClientMoLangFunctions`. None when
/// it is none of those functions, or when its arguments do not say which animation as they are written.
fn pose_animation(call: &Call) -> Option<Animation> {
    let arguments = Arguments(&call.arguments);
    Some(match call.function.as_str() {
        "bedrock" => Animation::Bedrock {
            group: arguments.string(0)?,
            name: arguments.string(1)?,
        },
        "look" => Animation::Look(Look {
            bone: arguments.string(0)?,
            or_bone: None,
            pitch_multiplier: arguments.number_or(1, 1.0)?,
            yaw_multiplier: arguments.number_or(2, 1.0)?,
            max_pitch: arguments.number_or(3, 70.0)?,
            min_pitch: arguments.number_or(4, -45.0)?,
            max_yaw: arguments.number_or(5, 45.0)?,
            min_yaw: arguments.number_or(6, -45.0)?,
        }),
        "pitch_tilt" => Animation::PitchTilt(PitchTilt {
            bone: arguments.string_or(0, "root")?,
            max_change_per_tick: arguments.number_or(1, 1.5)?,
            min_pitch: arguments.number_or(2, -45.0)?,
            max_pitch: arguments.number_or(3, 45.0)?,
        }),
        "biped_walk" => Animation::BipedWalk(BipedWalk {
            period_multiplier: arguments.number_or(0, 0.6662)?,
            amplitude_multiplier: arguments.number_or(1, 1.4)?,
            left_leg: arguments.string_or(2, "leg_left")?,
            right_leg: arguments.string_or(3, "leg_right")?,
        }),
        "quadruped_walk" => Animation::QuadrupedWalk(QuadrupedWalk {
            period_multiplier: arguments.number_or(0, 0.6662)?,
            amplitude_multiplier: arguments.number_or(1, 1.4)?,
            leg_front_left: arguments.string_or(2, "leg_front_left")?,
            leg_front_right: arguments.string_or(3, "leg_front_right")?,
            leg_back_left: arguments.string_or(4, "leg_back_left")?,
            leg_back_right: arguments.string_or(5, "leg_back_right")?,
        }),
        "bimanual_swing" => Animation::BimanualSwing(BimanualSwing {
            swing_period_multiplier: arguments.number_or(0, 0.6662)?,
            amplitude_multiplier: arguments.number_or(1, 1.0)?,
            left_arm: arguments.string_or(2, "arm_left")?,
            right_arm: arguments.string_or(3, "arm_right")?,
        }),
        "sine_wing_flap" => Animation::SineWingFlap(SineWingFlap {
            amplitude: arguments.number_or(0, 0.9)?,
            period: arguments.number_or(1, 0.9)?,
            vertical_shift: arguments.number_or(2, 0.0)?,
            axis: match arguments.string_or(3, "y")?.as_str() {
                "x" => Axis::X,
                "z" => Axis::Z,
                _ => Axis::Y,
            },
            wing_left: arguments.string_or(4, "wing_left")?,
            wing_right: arguments.string_or(5, "wing_right")?,
        }),
        _ => return None,
    })
}

/// A boolean as Gson reads one: `true`, `false`, or a string, true when it says `true` in any case
/// (`Boolean.parseBoolean`). Twenty-eight conditions of Cobblemon 1.8.1's posers are strings.
struct Flag(bool);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Boolean;
        impl Visitor<'_> for Boolean {
            type Value = Flag;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a boolean")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Flag, E> {
                Ok(Flag(value))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Flag, E> {
                Ok(Flag(value.eq_ignore_ascii_case("true")))
            }
        }
        deserializer.deserialize_any(Boolean)
    }
}

/// A `poseName`: a string, or a list of one (`JsonArray.getAsString`), as three poses of Cobblemon 1.8.1
/// write theirs. It is only checked: the pose's key takes its place.
struct Name;

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct OneName;
        impl<'de> Visitor<'de> for OneName {
            type Value = Name;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a name or a list of one")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Name, E> {
                Ok(Name)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Name, A::Error> {
                let name = seq.next_element::<String>()?;
                match (name, seq.next_element::<IgnoredAny>()?) {
                    (Some(_), None) => Ok(Name),
                    _ => Err(de::Error::custom(
                        "a `poseName` that is a list has one name",
                    )),
                }
            }
        }
        deserializer.deserialize_any(OneName)
    }
}

/// A JSON object's entries in the order of the file, which Gson's maps keep. A key given twice keeps its
/// first place with its last value, as in Gson's map (two poses of one name in a resource pack's poser).
struct Ordered<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Entries<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for Entries<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Ordered<T>, A::Error> {
                let mut entries: Vec<(String, T)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value()?;
                    match entries.iter_mut().find(|(known, _)| *known == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(Ordered(entries))
            }
        }
        deserializer.deserialize_map(Entries(PhantomData))
    }
}

/// A key that is there has a value: Cobblemon takes these out of the JSON by hand (`getAsString` and its
/// like), which fails on a null.
fn given<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase")]
struct RawPoser {
    #[serde(default, deserialize_with = "given")]
    root_bone: Option<String>,
    #[serde(default, deserialize_with = "given")]
    portrait_scale: Option<f32>,
    #[serde(default, deserialize_with = "given")]
    portrait_translation: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "given")]
    profile_scale: Option<f32>,
    #[serde(default, deserialize_with = "given")]
    profile_translation: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "given")]
    profile_summary_scale: Option<f32>,
    #[serde(default, deserialize_with = "given")]
    profile_summary_translation: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "given")]
    animations: Option<Ordered<String>>,
    #[serde(default, deserialize_with = "given")]
    poses: Option<Ordered<RawPose>>,
    #[serde(default, deserialize_with = "given")]
    transformed_parts: Option<Vec<RawPart>>,
    #[serde(default, deserialize_with = "given")]
    properties: Option<Ordered<String>>,
}

#[derive(Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase")]
struct RawPose {
    #[serde(default, deserialize_with = "given")]
    pose_name: Option<Name>,
    #[serde(default, deserialize_with = "given")]
    pose_types: Option<Vec<String>>,
    #[serde(default, deserialize_with = "given")]
    all_pose_types: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    transform_ticks: Option<u32>,
    #[serde(default, deserialize_with = "given")]
    transform_to_ticks: Option<u32>,
    #[serde(default, deserialize_with = "given")]
    transformed_parts: Option<Vec<RawPart>>,
    #[serde(default, deserialize_with = "given")]
    animations: Option<Vec<RawEntry>>,
    /// Each a MoLang string, or the older object (`{"name": .., "animations": [..]}`) that resource
    /// packs still carry and `JsonPose` still reads.
    #[serde(default, deserialize_with = "given")]
    quirks: Option<Vec<serde_json::Value>>,
    #[serde(default, deserialize_with = "given")]
    named_animations: Option<Ordered<String>>,
    #[serde(default, deserialize_with = "given")]
    transitions: Option<Ordered<String>>,
    #[serde(default, deserialize_with = "given")]
    condition: Option<String>,
    #[serde(default, deserialize_with = "given")]
    conditions: Option<Vec<String>>,
    #[serde(default, deserialize_with = "given")]
    is_ride_style: Option<Vec<String>>,
    #[serde(default, deserialize_with = "given")]
    is_touching_water: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    is_in_water_or_rain: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    is_under_water: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    is_dusk: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    is_battle: Option<Flag>,
    #[serde(default, deserialize_with = "given")]
    is_wild: Option<Flag>,
    is_standing_on_red_sand: Option<IgnoredAny>,
    is_standing_on_sand: Option<IgnoredAny>,
    is_standing_on_sand_or_red_sand: Option<IgnoredAny>,
}

/// One of a pose's `animations`: the animation's MoLang, or an object with it and a condition.
enum RawEntry {
    Plain(String),
    Conditional(RawConditional),
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawConditional {
    #[serde(default, deserialize_with = "given")]
    condition: Option<String>,
    #[serde(default, deserialize_with = "given")]
    animation: Option<String>,
}

impl<'de> Deserialize<'de> for RawEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Entry;
        impl<'de> Visitor<'de> for Entry {
            type Value = RawEntry;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an animation, its MoLang or an object with it")
            }
            fn visit_str<E: de::Error>(self, source: &str) -> std::result::Result<RawEntry, E> {
                Ok(RawEntry::Plain(source.to_owned()))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<RawEntry, A::Error> {
                RawConditional::deserialize(MapAccessDeserializer::new(map))
                    .map(RawEntry::Conditional)
            }
        }
        deserializer.deserialize_any(Entry)
    }
}

#[derive(Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase")]
struct RawPart {
    part: String,
    #[serde(default, deserialize_with = "given")]
    position: Option<[f64; 3]>,
    #[serde(default, deserialize_with = "given")]
    rotation: Option<[f64; 3]>,
    #[serde(default, deserialize_with = "given")]
    scale: Option<[f64; 3]>,
    #[serde(default, deserialize_with = "given")]
    is_visible: Option<Visible>,
}

impl<'de> Deserialize<'de> for Visible {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visibility;
        impl Visitor<'_> for Visibility {
            type Value = Visible;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a boolean or MoLang")
            }
            fn visit_bool<E: de::Error>(self, visible: bool) -> std::result::Result<Visible, E> {
                Ok(Visible::Is(visible))
            }
            fn visit_str<E: de::Error>(self, source: &str) -> std::result::Result<Visible, E> {
                Ok(Visible::Molang(source.to_owned()))
            }
        }
        deserializer.deserialize_any(Visibility)
    }
}

json::object_only!(RawPoser, RawPose, RawConditional, RawPart);

fn bad(message: impl fmt::Display) -> Error {
    Error::new(format!("poser: {message}"))
}

/// `PoseType`'s names, in the order of [`PoseType::ALL`].
const POSE_TYPES: [&str; 14] = [
    "STAND",
    "WALK",
    "SLEEP",
    "HOVER",
    "FLY",
    "FLOAT",
    "SWIM",
    "GLIDE",
    "SHOULDER_LEFT",
    "SHOULDER_RIGHT",
    "PROFILE",
    "PORTRAIT",
    "OPEN",
    "NONE",
];

fn transformed_part(raw: RawPart) -> TransformedPart {
    let floats = |written: Option<[f64; 3]>, default: f32| {
        written.map_or([default; 3], |written| written.map(|n| n as f32))
    };
    TransformedPart {
        part: raw.part,
        delta: Delta {
            position: floats(raw.position, 0.0),
            rotation: floats(raw.rotation, 0.0).map(radians),
            scale: floats(raw.scale, 1.0),
        },
        visible: raw.is_visible,
    }
}

/// Reads `source` as an [`Expression`], and notes what was odd about it.
fn expression(pose: Option<&str>, source: &str, oddities: &mut Vec<Oddity>) -> Expression {
    let mut notes = Vec::new();
    let expression = Expression::read(source, &mut notes);
    oddities.extend(notes.into_iter().map(|kind| Oddity {
        pose: pose.map(str::to_owned),
        source: source.to_owned(),
        kind,
    }));
    expression
}

fn named(
    pose: Option<&str>,
    entries: Option<Ordered<String>>,
    oddities: &mut Vec<Oddity>,
) -> Vec<(String, Expression)> {
    let entries = entries.map_or_else(Vec::new, |entries| entries.0);
    entries
        .into_iter()
        .map(|(name, source)| (name, expression(pose, &source, oddities)))
        .collect()
}

/// One of a pose's `animations`, as `JsonPose` reads it. `plain` is whether the entry is a string and
/// not an object, which is where the way of writing that came before MoLang still works:
/// `bedrock(<group>, <animation>)`, without quotes (`BedrockAnimationReferenceFactory`).
fn animation(
    pose: &str,
    source: &str,
    plain: bool,
    oddities: &mut Vec<Oddity>,
) -> Result<Animation> {
    if source == "look" {
        return Ok(Animation::Look(Look::bare()));
    }
    if plain && source.split('(').next() == Some("bedrock") {
        let inside = source.replace("bedrock(", "").replace(')', "");
        let mut parts = inside.split(',').map(str::trim);
        return match (parts.next(), parts.next()) {
            (Some(group), Some(name)) => Ok(Animation::Bedrock {
                group: group.to_owned(),
                name: name.to_owned(),
            }),
            _ => Err(bad(format_args!(
                "pose `{pose}`: `{source}` names no animation"
            ))),
        };
    }
    Ok(match expression(Some(pose), source, oddities) {
        Expression::Call(call) => {
            pose_animation(&call).unwrap_or(Animation::Other(Expression::Call(call)))
        }
        written => Animation::Other(written),
    })
}

/// A quirk written as an object, as `JsonPose` reads it: `animation` joins `animations`, either one
/// string or a list of them, and what is not there has its default. Read leniently, as Gson does: a
/// value of the wrong kind is as good as absent.
fn legacy_quirk(object: &serde_json::Map<String, serde_json::Value>) -> LegacyQuirk {
    let mut animations = Vec::new();
    for key in ["animations", "animation"] {
        match object.get(key) {
            Some(serde_json::Value::String(source)) => animations.push(source.clone()),
            Some(serde_json::Value::Array(sources)) => animations.extend(
                sources
                    .iter()
                    .filter_map(|source| source.as_str().map(str::to_owned)),
            ),
            _ => {}
        }
    }
    let number = |key: &str, default: f32| {
        object
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .map_or(default, |number| number as f32)
    };
    LegacyQuirk {
        animations,
        loop_times: number("loopTimes", 1.0) as i32,
        seconds: (
            number("minSecondsBetweenOccurrences", 8.0),
            number("maxSecondsBetweenOccurrences", 30.0),
        ),
        condition: object
            .get("condition")
            .and_then(|condition| condition.as_str().map(str::to_owned)),
    }
}

fn read_pose(name: String, raw: RawPose, oddities: &mut Vec<Oddity>) -> Result<Pose> {
    if raw.is_standing_on_red_sand.is_some()
        || raw.is_standing_on_sand.is_some()
        || raw.is_standing_on_sand_or_red_sand.is_some()
    {
        return Err(bad(format_args!(
            "pose `{name}` asks what the entity stands on"
        )));
    }
    let mut pose_types = Vec::new();
    for written in raw.pose_types.into_iter().flatten() {
        let lower = written.to_lowercase();
        let index = POSE_TYPES
            .iter()
            .position(|known| known.to_lowercase() == lower)
            .ok_or_else(|| bad(format_args!("pose `{name}`: no pose type is `{written}`")))?;
        if !pose_types.contains(&PoseType::ALL[index]) {
            pose_types.push(PoseType::ALL[index]);
        }
    }
    if raw.all_pose_types.is_some_and(|all| all.0) {
        for pose_type in PoseType::ALL {
            if !pose_types.contains(&pose_type) {
                pose_types.push(pose_type);
            }
        }
    }
    // `condition` joins `conditions`, after them (`singularToPluralList`).
    let mut molang = raw.conditions;
    if let Some(condition) = raw.condition {
        molang.get_or_insert_default().push(condition);
    }
    let mut animations = Vec::new();
    for entry in raw.animations.into_iter().flatten() {
        let (condition, source, plain) = match entry {
            RawEntry::Plain(source) => (None, source, true),
            RawEntry::Conditional(RawConditional {
                condition,
                animation: Some(source),
            }) => (condition, source, false),
            RawEntry::Conditional(_) => {
                oddities.push(Oddity {
                    pose: Some(name.clone()),
                    source: String::new(),
                    kind: OddityKind::NoAnimation,
                });
                continue;
            }
        };
        animations.push(PoseAnimation {
            condition,
            animation: animation(&name, &source, plain, oddities)?,
        });
    }
    let mut quirks = Vec::new();
    let mut legacy_quirks = Vec::new();
    for quirk in raw.quirks.into_iter().flatten() {
        match quirk {
            serde_json::Value::String(source) => {
                quirks.push(expression(Some(&name), &source, oddities));
            }
            // The older form: noted, and kept apart from the calls.
            serde_json::Value::Object(object) => {
                oddities.push(Oddity {
                    pose: Some(name.clone()),
                    source: String::new(),
                    kind: OddityKind::LegacyQuirk,
                });
                legacy_quirks.push(legacy_quirk(&object));
            }
            other => {
                return Err(Error::new(format!(
                    "poser: a quirk of `{name}` is neither a string nor an object: {other}"
                )));
            }
        }
    }
    let flag = |flag: Option<Flag>| flag.map(|flag| flag.0);
    Ok(Pose {
        pose_types,
        conditions: Conditions {
            ride_styles: raw.is_ride_style,
            touching_water: flag(raw.is_touching_water),
            in_water_or_rain: flag(raw.is_in_water_or_rain),
            under_water: flag(raw.is_under_water),
            dusk: flag(raw.is_dusk),
            battle: flag(raw.is_battle),
            molang,
            wild: flag(raw.is_wild),
        },
        animations,
        quirks,
        legacy_quirks,
        named_animations: named(Some(&name), raw.named_animations, oddities),
        transitions: named(Some(&name), raw.transitions, oddities),
        transformed_parts: raw
            .transformed_parts
            .into_iter()
            .flatten()
            .map(transformed_part)
            .collect(),
        transform_ticks: raw.transform_ticks.unwrap_or(10),
        transform_to_ticks: raw.transform_to_ticks,
        name,
    })
}

impl Poser {
    /// Reads one poser file.
    ///
    /// An animation Cobblemon would drop does not fail the file: it is kept as an [`Animation::Other`].
    /// What fails it is what fails Cobblemon (an unknown pose type, a transformed part without a `part`,
    /// a null where a value goes), and what no poser of Cobblemon 1.8.1 has: a quirk written as an object
    /// rather than as MoLang, a pose that asks what the entity stands on (`isStandingOnSand` and its
    /// like), a number written as a string.
    pub fn parse(json: &str) -> Result<Self> {
        let file: RawPoser = json::parse(json, "poser")?;
        let mut oddities = Vec::new();
        let named_animations = named(None, file.animations, &mut oddities);
        let poses = file
            .poses
            .map_or_else(Vec::new, |poses| poses.0)
            .into_iter()
            .map(|(name, pose)| read_pose(name, pose, &mut oddities))
            .collect::<Result<_>>()?;
        Ok(Self {
            root_bone: file.root_bone,
            portrait_scale: file.portrait_scale.unwrap_or(1.0),
            portrait_translation: file.portrait_translation.unwrap_or_default(),
            profile_scale: file.profile_scale.unwrap_or(1.0),
            profile_translation: file.profile_translation.unwrap_or_default(),
            profile_summary_scale: file.profile_summary_scale,
            profile_summary_translation: file.profile_summary_translation,
            poses,
            named_animations,
            transformed_parts: file
                .transformed_parts
                .into_iter()
                .flatten()
                .map(transformed_part)
                .collect(),
            properties: file.properties.map_or_else(Vec::new, |written| written.0),
            oddities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTMON: &str = r#"{
        "portraitScale": 1.5,
        "rootBone": "testmon",
        "animations": {
            "cry": "q.bedrock_stateful('testmon', 'cry')",
            "physical": "q.bedrock_primary('testmon', 'physical', q.curve('symmetrical_wide'))"
        },
        "transformedParts": [
            {"part": "tail2", "isVisible": "q.has_aspect('long-tail')"},
            {"part": "hat", "scale": [0.9, 1, 0.9], "position": [0, -2.5, 0]}
        ],
        "poses": {
            "battle-standing": {
                "poseTypes": ["STAND"],
                "isBattle": true,
                "animations": ["q.look('head')", "q.bedrock('testmon', 'battle_idle')"],
                "quirks": [
                    "q.bedrock_quirk('testmon', 'blink')",
                    "q.bedrock_quirk('testmon', q.array('quirk1', 'quirk2'), 20, 60, 1)"
                ],
                "namedAnimations": {"cry": "q.bedrock_stateful('testmon', 'battle_cry')"},
                "transitions": {"sleeping": "q.bedrock_primary('testmon', 'lie_down')"}
            },
            "standing": {
                "poseName": ["something_else"],
                "poseTypes": ["STAND", "none", "Portrait", "stand"],
                "isBattle": "false",
                "isTouchingWater": false,
                "transformTicks": 5,
                "animations": ["q.look('head')", "q.bedrock('testmon', 'ground_idle')"]
            },
            "running": {
                "poseTypes": ["WALK"],
                "condition": "q.is_sprinting",
                "animations": ["q.bedrock('testmon', 'ground_run')"]
            },
            "walking": {
                "poseTypes": ["WALK"],
                "isTouchingWater": false,
                "animations": ["q.bedrock('testmon', 'ground_walk')"]
            },
            "floating": {
                "poseTypes": ["STAND", "FLOAT"],
                "isTouchingWater": true,
                "isUnderWater": false,
                "animations": ["q.bedrock('testmon', 'surface_idle')"]
            },
            "diving": {
                "poseTypes": ["STAND", "FLOAT", "SWIM"],
                "isUnderWater": "TRUE",
                "transformToTicks": 3,
                "animations": ["q.bedrock('testmon', 'water_idle')"]
            },
            "sleeping": {
                "poseTypes": ["SLEEP"],
                "transformTicks": 0,
                "animations": ["q.bedrock('testmon', 'sleep')"]
            }
        }
    }"#;

    fn testmon() -> Poser {
        Poser::parse(TESTMON).unwrap()
    }

    /// A poser of one pose, `p`, with these `animations`.
    fn posed(animations: &str) -> Poser {
        Poser::parse(&format!(
            r#"{{"poses": {{"p": {{"poseTypes": ["STAND"], "animations": {animations}}}}}}}"#
        ))
        .unwrap()
    }

    /// What one string among a pose's `animations` is read as.
    fn read(source: &str) -> Animation {
        let source = source.replace('\\', "\\\\").replace('"', "\\\"");
        let mut poser = posed(&format!(r#"["{source}"]"#));
        poser.poses[0].animations.remove(0).animation
    }

    fn call(function: &str, arguments: Vec<Argument>) -> Call {
        Call {
            function: function.to_owned(),
            arguments,
        }
    }

    fn string(string: &str) -> Argument {
        Argument::String(string.to_owned())
    }

    fn bedrock(group: &str, name: &str) -> Animation {
        Animation::Bedrock {
            group: group.to_owned(),
            name: name.to_owned(),
        }
    }

    /// The name of the pose picked, `molang` being what every condition is worth.
    fn picked<'a>(poser: &'a Poser, entity: &Entity, molang: f64) -> &'a str {
        &poser.pose_for(entity, &mut |_| molang).unwrap().name
    }

    fn in_pose(pose: PoseType) -> Entity {
        Entity {
            pose,
            ..Entity::default()
        }
    }

    /// What an animation reports in one frame.
    fn reports(animation: &Animation, motion: &Motion, tilt: &mut Tilt) -> Vec<(String, Delta)> {
        let mut reports = Vec::new();
        animation.apply(motion, tilt, &mut |bone, delta| {
            reports.push((bone.to_owned(), delta));
        });
        reports
    }

    fn turned(x: f32, y: f32, z: f32) -> Delta {
        Delta {
            rotation: [x, y, z],
            ..Delta::NONE
        }
    }

    #[track_caller]
    fn assert_reports(got: &[(String, Delta)], wanted: &[(&str, Delta)]) {
        assert_eq!(got.len(), wanted.len(), "{got:?}");
        for ((bone, delta), (wanted_bone, wanted_delta)) in got.iter().zip(wanted) {
            assert_eq!(bone, wanted_bone, "{got:?}");
            assert_eq!(delta.position, wanted_delta.position, "{bone}");
            assert_eq!(delta.scale, wanted_delta.scale, "{bone}");
            for axis in 0..3 {
                let (got, wanted) = (delta.rotation[axis], wanted_delta.rotation[axis]);
                assert!(
                    (got - wanted).abs() < 1e-6,
                    "{bone} axis {axis}: {got} is not {wanted}"
                );
            }
        }
    }

    #[test]
    fn poses_keep_the_order_and_the_names_of_the_file() {
        let poser = testmon();
        assert_eq!(poser.root_bone.as_deref(), Some("testmon"));
        // The portrait's framing as the file gives it, and what a file leaves out.
        assert_eq!(poser.portrait_scale, 1.5);
        assert_eq!(poser.portrait_translation, [0.0; 3]);
        assert_eq!(
            (poser.profile_scale, poser.profile_translation),
            (1.0, [0.0; 3])
        );
        assert_eq!(
            (
                poser.profile_summary_scale,
                poser.profile_summary_translation
            ),
            (None, None)
        );
        // Drapion's: the summary moves it where its profile's framing would not.
        let drapion = Poser::parse(
            r#"{"profileScale": 0.5, "profileTranslation": [0, 0.9, 0],
                "profileSummaryTranslation": [-0.30, 0.87, 0], "poses": {}}"#,
        )
        .unwrap();
        assert_eq!(drapion.profile_summary_scale, None);
        assert_eq!(
            drapion.profile_summary_translation,
            Some([-0.30, 0.87, 0.0])
        );
        let names: Vec<&str> = poser.poses.iter().map(|pose| pose.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "battle-standing",
                "standing",
                "running",
                "walking",
                "floating",
                "diving",
                "sleeping"
            ]
        );
        // A `poseName` gives way to the key, a type is matched whatever its case and counted once.
        let standing = &poser.poses[1];
        assert_eq!(
            standing.pose_types,
            [PoseType::Stand, PoseType::None, PoseType::Portrait]
        );
        // 10 ticks unless the file says, and none to blend into a pose unless the file says.
        let ticks = |pose: usize| {
            (
                poser.poses[pose].transform_ticks,
                poser.poses[pose].transform_to_ticks,
            )
        };
        assert_eq!(
            [ticks(0), ticks(1), ticks(5), ticks(6)],
            [(10, None), (5, None), (10, Some(3)), (0, None)]
        );
        // A boolean may be a string, in any case.
        assert_eq!(standing.conditions.battle, Some(false));
        assert_eq!(poser.poses[5].conditions.under_water, Some(true));
        assert_eq!(
            poser.poses[0].conditions,
            Conditions {
                battle: Some(true),
                ..Conditions::default()
            }
        );
        assert_eq!(poser.oddities, []);
    }

    #[test]
    fn the_first_pose_that_fits_is_the_one_picked() {
        let poser = testmon();
        let stand = in_pose(PoseType::Stand);
        assert_eq!(picked(&poser, &stand, 0.0), "standing");
        let battling = Entity {
            battle: true,
            ..stand
        };
        assert_eq!(picked(&poser, &battling, 0.0), "battle-standing");
        // In water: at its surface, then under it. `standing` wants no water, `floating` no diving.
        let wet = Entity {
            touching_water: true,
            ..stand
        };
        assert_eq!(picked(&poser, &wet, 0.0), "floating");
        let under = Entity {
            under_water: true,
            ..wet
        };
        assert_eq!(picked(&poser, &under, 0.0), "diving");
        // A battle in the water: `battle-standing` asks nothing of the water and comes first.
        let wet_battle = Entity {
            battle: true,
            ..under
        };
        assert_eq!(picked(&poser, &wet_battle, 0.0), "battle-standing");
        assert_eq!(picked(&poser, &in_pose(PoseType::Sleep), 0.0), "sleeping");
        let swimming = Entity {
            pose: PoseType::Swim,
            ..under
        };
        assert_eq!(picked(&poser, &swimming, 0.0), "diving");
    }

    #[test]
    fn a_molang_condition_is_asked_of_the_caller_and_holds_at_exactly_one() {
        let poser = testmon();
        let walk = in_pose(PoseType::Walk);
        let mut asked = Vec::new();
        let pose = poser
            .pose_for(&walk, &mut |source| {
                asked.push(source.to_owned());
                1.0
            })
            .unwrap();
        assert_eq!(pose.name, "running");
        // Only the condition of a pose that got that far: `running` is the first of the WALK poses.
        assert_eq!(asked, ["q.is_sprinting"]);
        assert_eq!(picked(&poser, &walk, 0.0), "walking");
        // `ExpressionLike.resolveBoolean` is `== 1.0`: another number is not a yes.
        assert_eq!(picked(&poser, &walk, 2.0), "walking");

        // `conditions` and then `condition`: one of them is enough, and an empty list never holds.
        let poser = Poser::parse(
            r#"{"poses": {
                "never": {"poseTypes": ["STAND"], "conditions": []},
                "either": {"poseTypes": ["STAND"], "conditions": ["q.a", "q.b"], "condition": "q.c"},
                "last": {"poseTypes": ["STAND"]}
            }}"#,
        )
        .unwrap();
        assert_eq!(poser.poses[0].conditions.molang, Some(vec![]));
        assert_eq!(
            poser.poses[1].conditions.molang,
            Some(vec!["q.a".to_owned(), "q.b".to_owned(), "q.c".to_owned()])
        );
        let stand = in_pose(PoseType::Stand);
        let mut asked = Vec::new();
        let pose = poser
            .pose_for(&stand, &mut |source| {
                asked.push(source.to_owned());
                f64::from(source == "q.b")
            })
            .unwrap();
        assert_eq!((pose.name.as_str(), asked.len()), ("either", 2));
        assert_eq!(picked(&poser, &stand, 0.0), "last");
    }

    #[test]
    fn without_a_pose_that_fits_the_first_of_the_file_is_picked() {
        let poser = testmon();
        // No pose of the file is a FLY one: the first is taken, though it wants a battle and a STAND.
        let fly = in_pose(PoseType::Fly);
        let pose = poser.pose_for(&fly, &mut |_| 0.0).unwrap();
        assert_eq!(pose.name, "battle-standing");
        assert!(!pose.fits(&fly, &mut |_| 0.0));
        // A type the file has, with conditions nothing meets: walking in water without sprinting.
        let wading = Entity {
            touching_water: true,
            ..in_pose(PoseType::Walk)
        };
        assert_eq!(picked(&poser, &wading, 0.0), "battle-standing");
        let none = Poser::parse(r#"{"poses": {}}"#).unwrap();
        assert_eq!(none.pose_for(&fly, &mut |_| 1.0), None);
        assert_eq!(Poser::parse("{}").unwrap().poses, []);
    }

    #[test]
    fn the_other_conditions_are_read_and_is_wild_is_not_asked() {
        let poser = Poser::parse(
            r#"{"poses": {
                "boat": {"poseTypes": ["STAND"], "isRideStyle": ["LIQUID", "AIR"]},
                "bubbles": {"poseTypes": ["STAND"], "isDusk": true, "isInWaterOrRain": false},
                "wild": {"poseTypes": ["STAND"], "isWild": true},
                "all": {"allPoseTypes": true, "isWild": false}
            }}"#,
        )
        .unwrap();
        let stand = in_pose(PoseType::Stand);
        // Not ridden, by day: `isWild` is not something Cobblemon asks, so `wild` fits any entity.
        assert_eq!(poser.poses[2].conditions.wild, Some(true));
        assert_eq!(picked(&poser, &stand, 0.0), "wild");
        let dusk = Entity {
            dusk: true,
            ..stand
        };
        assert_eq!(picked(&poser, &dusk, 0.0), "bubbles");
        let rain = Entity {
            in_water_or_rain: true,
            ..dusk
        };
        assert_eq!(picked(&poser, &rain, 0.0), "wild");
        for (style, pose) in [
            (RideStyle::Liquid, "boat"),
            (RideStyle::Air, "boat"),
            (RideStyle::Land, "wild"),
        ] {
            let ridden = Entity {
                ride_style: Some(style),
                ..stand
            };
            assert_eq!(picked(&poser, &ridden, 0.0), pose, "{style:?}");
        }
        assert_eq!(poser.poses[3].pose_types, PoseType::ALL);
        assert_eq!(picked(&poser, &in_pose(PoseType::Glide), 0.0), "all");
    }

    #[test]
    fn each_animation_is_read_with_cobblemon_s_defaults() {
        assert_eq!(
            read("q.bedrock('testmon', 'ground_idle')"),
            bedrock("testmon", "ground_idle")
        );
        let look = Look {
            bone: "head".to_owned(),
            or_bone: None,
            pitch_multiplier: 1.0,
            yaw_multiplier: 1.0,
            max_pitch: 70.0,
            min_pitch: -45.0,
            max_yaw: 45.0,
            min_yaw: -45.0,
        };
        assert_eq!(read("q.look('head')"), Animation::Look(look.clone()));
        // An argument given moves no other from its default.
        assert_eq!(
            read("q.look('head', 0.5)"),
            Animation::Look(Look {
                pitch_multiplier: 0.5,
                ..look
            })
        );
        let pitch_tilt = Animation::PitchTilt(PitchTilt {
            bone: "root".to_owned(),
            max_change_per_tick: 1.5,
            min_pitch: -45.0,
            max_pitch: 45.0,
        });
        assert_eq!(read("q.pitch_tilt()"), pitch_tilt);
        // A name alone is a call without arguments.
        assert_eq!(read("q.pitch_tilt"), pitch_tilt);
        assert_eq!(
            read("q.biped_walk()"),
            Animation::BipedWalk(BipedWalk {
                period_multiplier: 0.6662,
                amplitude_multiplier: 1.4,
                left_leg: "leg_left".to_owned(),
                right_leg: "leg_right".to_owned(),
            })
        );
        assert_eq!(
            read("q.quadruped_walk()"),
            Animation::QuadrupedWalk(QuadrupedWalk {
                period_multiplier: 0.6662,
                amplitude_multiplier: 1.4,
                leg_front_left: "leg_front_left".to_owned(),
                leg_front_right: "leg_front_right".to_owned(),
                leg_back_left: "leg_back_left".to_owned(),
                leg_back_right: "leg_back_right".to_owned(),
            })
        );
        assert_eq!(
            read("q.bimanual_swing()"),
            Animation::BimanualSwing(BimanualSwing {
                swing_period_multiplier: 0.6662,
                amplitude_multiplier: 1.0,
                left_arm: "arm_left".to_owned(),
                right_arm: "arm_right".to_owned(),
            })
        );
        assert_eq!(
            read("q.sine_wing_flap()"),
            Animation::SineWingFlap(SineWingFlap {
                amplitude: 0.9,
                period: 0.9,
                vertical_shift: 0.0,
                axis: Axis::Y,
                wing_left: "wing_left".to_owned(),
                wing_right: "wing_right".to_owned(),
            })
        );
    }

    #[test]
    fn each_animation_is_read_with_its_arguments_in_cobblemon_s_order() {
        assert_eq!(
            read("q.look('neck', 0.5, -1, 20, -10, 30, -30)"),
            Animation::Look(Look {
                bone: "neck".to_owned(),
                or_bone: None,
                pitch_multiplier: 0.5,
                yaw_multiplier: -1.0,
                max_pitch: 20.0,
                min_pitch: -10.0,
                max_yaw: 30.0,
                min_yaw: -30.0,
            })
        );
        assert_eq!(
            read("q.pitch_tilt('body', 0.22, -30, 20)"),
            Animation::PitchTilt(PitchTilt {
                bone: "body".to_owned(),
                max_change_per_tick: 0.22,
                min_pitch: -30.0,
                max_pitch: 20.0,
            })
        );
        assert_eq!(
            read("q.biped_walk(0.4, 0.7, 'foot_left', 'foot_right')"),
            Animation::BipedWalk(BipedWalk {
                period_multiplier: 0.4,
                amplitude_multiplier: 0.7,
                left_leg: "foot_left".to_owned(),
                right_leg: "foot_right".to_owned(),
            })
        );
        assert_eq!(
            read("q.quadruped_walk(1, 1.2, 'fl', 'fr', 'bl', 'br')"),
            Animation::QuadrupedWalk(QuadrupedWalk {
                period_multiplier: 1.0,
                amplitude_multiplier: 1.2,
                leg_front_left: "fl".to_owned(),
                leg_front_right: "fr".to_owned(),
                leg_back_left: "bl".to_owned(),
                leg_back_right: "br".to_owned(),
            })
        );
        // The right arm keeps its default name.
        assert_eq!(
            read("q.bimanual_swing(0.66, 0.6, 'left_upper_arm')"),
            Animation::BimanualSwing(BimanualSwing {
                swing_period_multiplier: 0.66,
                amplitude_multiplier: 0.6,
                left_arm: "left_upper_arm".to_owned(),
                right_arm: "arm_right".to_owned(),
            })
        );
        let flap = SineWingFlap {
            amplitude: 0.7,
            period: 0.4,
            vertical_shift: 25.0,
            axis: Axis::Z,
            wing_left: "wing_open_left".to_owned(),
            wing_right: "wing_open_right".to_owned(),
        };
        assert_eq!(
            read("q.sine_wing_flap(0.7, 0.4, 25, 'z', 'wing_open_left', 'wing_open_right')"),
            Animation::SineWingFlap(flap.clone())
        );
        // An axis that is none of the three is y, and x is x.
        for (axis, read_as) in [("x", Axis::X), ("w", Axis::Y), ("Z", Axis::Y)] {
            assert_eq!(
                read(&format!(
                    "q.sine_wing_flap(0.7, 0.4, 25, '{axis}', 'wing_open_left', 'wing_open_right')"
                )),
                Animation::SineWingFlap(SineWingFlap {
                    axis: read_as,
                    ..flap.clone()
                })
            );
        }
        // What comes after the arguments a function reads is not read.
        assert_eq!(
            read("q.bedrock('testmon', 'ground_idle', q.curve('symmetrical_wide'))"),
            bedrock("testmon", "ground_idle")
        );
    }

    #[test]
    fn molang_is_cut_as_cobblemon_s_parser_cuts_it() {
        // Names in any case, `query` for `q`, spaces anywhere, a `;` after the statement.
        assert_eq!(
            read("  QUERY.Bedrock ( 'TestMon' ,'Idle' ) ; "),
            bedrock("TestMon", "Idle")
        );
        // Signs, a number ending in a dot, a quote escaped in a string.
        assert_eq!(
            Expression::parse(r"q.f(-0.5, +2, - -3, 7., 'it\'s', 'a,b(c')"),
            Expression::Call(call(
                "f",
                vec![
                    Argument::Number(-0.5),
                    Argument::Number(2.0),
                    Argument::Number(3.0),
                    Argument::Number(7.0),
                    string("it's"),
                    string("a,b(c"),
                ]
            ))
        );
        // A call among the arguments, and what is neither a string, a number nor a call, as written.
        assert_eq!(
            Expression::parse(
                "q.bedrock_primary('testmon', q.array('a', 'b'), math.random(0, 1) < 0.4 ? 'x' : (1 + 2), q.is_ridden, true)"
            ),
            Expression::Call(call(
                "bedrock_primary",
                vec![
                    string("testmon"),
                    Argument::Call(call("array", vec![string("a"), string("b")])),
                    Argument::Molang("math.random(0, 1) < 0.4 ? 'x' : (1 + 2)".to_owned()),
                    Argument::Molang("q.is_ridden".to_owned()),
                    Argument::Molang("true".to_owned()),
                ]
            ))
        );
        // Not one call: more than the call in the expression, two statements, another struct's
        // function, a call that is not closed, an argument that is missing, a reference to a script,
        // a letter that is not ASCII outside a string.
        for source in [
            "q.look('head') ? 1 : 2",
            "q.look('head') + 1",
            "q.look('head'); q.bedrock('a', 'b')",
            "math.sin(90)",
            "bedrock_primary('a', 'b')",
            "q.look('head'",
            "q.look('head",
            "q.look('head',)",
            "q.look(,)",
            "q.a.b('c')",
            "q . look('head')",
            "cobblemon:some_script",
            "q.look('head', é)",
            "'just a string'",
            "",
        ] {
            assert_eq!(
                Expression::parse(source),
                Expression::Molang(source.to_owned()),
                "{source}"
            );
        }
        // A letter that is not ASCII is fine in a string.
        assert_eq!(
            Expression::parse("q.look('tête')"),
            Expression::Call(call("look", vec![string("tête")]))
        );
    }

    #[test]
    fn calls_nested_too_deep_are_kept_as_written_not_followed() {
        // Seventeen calls inside one another: the innermost is no longer read as a call.
        let mut source = "q.a(".repeat(MAX_DEPTH + 2);
        source.push_str("'x'");
        source.push_str(&")".repeat(MAX_DEPTH + 2));
        let Expression::Call(mut outer) = Expression::parse(&source) else {
            panic!("not a call");
        };
        for _ in 0..MAX_DEPTH {
            let Some(Argument::Call(inner)) = outer.arguments.pop() else {
                panic!("not a call");
            };
            outer = inner;
        }
        assert_eq!(outer.arguments, [Argument::Molang("q.a('x')".to_owned())]);
        // However deep and however broken, it is read without running out of stack, and in a poser too.
        for source in ["q.a(".repeat(100_000), "((((".repeat(100_000)] {
            assert_eq!(
                Expression::parse(&source),
                Expression::Molang(source.clone())
            );
            let poser = posed(&format!(r#"["{source}"]"#));
            assert_eq!(
                poser.poses[0].animations[0].animation,
                Animation::Other(Expression::Molang(source))
            );
        }
    }

    #[test]
    fn an_entry_with_a_condition_keeps_it_and_runs_while_it_is_not_zero() {
        let poser = posed(
            r#"[
                "q.look('head')",
                {"condition": "q.is_ridden", "animation": "q.bedrock('testmon', 'ridden_idle')"},
                {"animation": "look"}
            ]"#,
        );
        let animations = &poser.poses[0].animations;
        assert_eq!(animations.len(), 3);
        assert_eq!(animations[0].condition, None);
        assert_eq!(
            animations[1],
            PoseAnimation {
                condition: Some("q.is_ridden".to_owned()),
                animation: bedrock("testmon", "ridden_idle"),
            }
        );
        assert_eq!(animations[2].condition, None);
        let mut asked = Vec::new();
        for (worth, runs) in [(0.0, false), (1.0, true), (0.5, true), (-2.0, true)] {
            // `MoLangRuntime.resolveBoolean` is `!= 0.0`: any other number is a yes.
            let ran = animations[1].runs(&mut |source| {
                asked.push(source.to_owned());
                worth
            });
            assert_eq!(ran, runs, "{worth}");
        }
        assert_eq!(asked, ["q.is_ridden"; 4]);
        // Without a condition nothing is asked.
        assert!(animations[0].runs(&mut |_| panic!("nothing to ask")));
    }

    #[test]
    fn transformed_parts_are_deltas_in_part_space() {
        let poser = Poser::parse(
            r#"{"poses": {"p": {"poseTypes": ["STAND"], "transformedParts": [
                {"part": "body", "position": [-6, 2.5, 0]},
                {"part": "arm_left", "rotation": [30, -0.65, 52.5], "isVisible": true},
                {"part": "bulb", "isVisible": false, "comment": "hidden", "visible": true},
                {"part": "tail", "scale": [0.9, 1, 2], "isVisible": "q.has_aspect('tail')"},
                {"part": "eye", "visible": false}
            ]}}}"#,
        )
        .unwrap();
        let parts = &poser.poses[0].transformed_parts;
        // The position as written, the rotation from degrees with its signs, the scale as written.
        assert_eq!(
            parts[0],
            TransformedPart {
                part: "body".to_owned(),
                delta: Delta {
                    position: [-6.0, 2.5, 0.0],
                    ..Delta::NONE
                },
                visible: None,
            }
        );
        // 30 / 57.2958, -0.65 / 57.2958 and 52.5 / 57.2958, as floats.
        assert_eq!(
            parts[1].delta,
            turned(0.523_598_6, -0.011_344_636, 0.916_297_55)
        );
        assert_eq!(parts[1].visible, Some(Visible::Is(true)));
        assert_eq!(
            (&parts[2].delta, &parts[2].visible),
            (&Delta::NONE, &Some(Visible::Is(false)))
        );
        assert_eq!(parts[3].delta.scale, [0.9, 1.0, 2.0]);
        // MoLang visibility is the caller's to answer, visible while it is not 0.
        let visible = parts[3].visible.as_ref().unwrap();
        assert_eq!(*visible, Visible::Molang("q.has_aspect('tail')".to_owned()));
        let mut asked = Vec::new();
        assert!(visible.get(&mut |source| {
            asked.push(source.to_owned());
            1.0
        }));
        assert_eq!(asked, ["q.has_aspect('tail')"]);
        assert!(!visible.get(&mut |_| 0.0));
        assert!(Visible::Is(true).get(&mut |_| panic!("nothing to ask")));
        // `visible` is not a key Cobblemon reads.
        assert_eq!(parts[4].visible, None);

        // A delta is what `ModelPartTransformation.apply` does to a part at full intensity.
        let mut pose = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        parts[0].delta.apply(&mut pose);
        parts[3].delta.apply(&mut pose);
        assert_eq!(pose, [-5.0, 3.5, 1.0, 0.0, 0.0, 0.0, 0.9, 1.0, 2.0]);

        // The poser's own, for every pose.
        let poser = testmon();
        assert_eq!(poser.transformed_parts.len(), 2);
        assert_eq!(
            poser.transformed_parts[0].visible,
            Some(Visible::Molang("q.has_aspect('long-tail')".to_owned()))
        );
        assert_eq!(
            poser.transformed_parts[1].delta,
            Delta {
                position: [0.0, -2.5, 0.0],
                rotation: [0.0; 3],
                scale: [0.9, 1.0, 0.9],
            }
        );
    }

    #[test]
    fn a_model_says_how_it_holds_an_item() {
        let poser = Poser::parse(
            r#"{"properties": {"item_display_context": "THIRD_PERSON_RIGHT_HAND", "other": "x"}}"#,
        )
        .unwrap();
        assert_eq!(
            poser.display_context("item"),
            Some("THIRD_PERSON_RIGHT_HAND")
        );
        assert_eq!(poser.display_context("item_hat"), None);
        // Most say nothing.
        assert!(testmon().properties.is_empty());
        assert_eq!(testmon().display_context("item"), None);
    }

    #[test]
    fn named_animations_quirks_and_transitions_are_kept_as_calls() {
        let poser = testmon();
        assert_eq!(
            poser.named_animations,
            [
                (
                    "cry".to_owned(),
                    Expression::Call(call(
                        "bedrock_stateful",
                        vec![string("testmon"), string("cry")]
                    ))
                ),
                (
                    "physical".to_owned(),
                    Expression::Call(call(
                        "bedrock_primary",
                        vec![
                            string("testmon"),
                            string("physical"),
                            Argument::Call(call("curve", vec![string("symmetrical_wide")])),
                        ]
                    ))
                ),
            ]
        );
        let battle = &poser.poses[0];
        assert_eq!(
            battle.quirks,
            [
                Expression::Call(call(
                    "bedrock_quirk",
                    vec![string("testmon"), string("blink")]
                )),
                Expression::Call(call(
                    "bedrock_quirk",
                    vec![
                        string("testmon"),
                        Argument::Call(call("array", vec![string("quirk1"), string("quirk2")])),
                        Argument::Number(20.0),
                        Argument::Number(60.0),
                        Argument::Number(1.0),
                    ]
                )),
            ]
        );
        assert_eq!(
            battle.named_animations,
            [(
                "cry".to_owned(),
                Expression::Call(call(
                    "bedrock_stateful",
                    vec![string("testmon"), string("battle_cry")]
                ))
            )]
        );
        assert_eq!(
            battle.transitions,
            [(
                "sleeping".to_owned(),
                Expression::Call(call(
                    "bedrock_primary",
                    vec![string("testmon"), string("lie_down")]
                ))
            )]
        );
        assert!(poser.poses[1].quirks.is_empty() && poser.poses[1].named_animations.is_empty());
    }

    #[test]
    fn what_cobblemon_reads_past_does_not_fail_the_file() {
        let poser = Poser::parse(
            r#"{"poses": {"p": {
                "poseTypes": ["STAND"],
                "animations": [
                    "q.look('waist', O.5, 0.5)",
                    {"namedAnimations": {"cry": "q.bedrock_stateful('testmon', 'cry')"}},
                    "q.bedrock('testmon', 'ground_idle')"
                ],
                "quirks": ["q.bedrock_quirk('testmon', 'idle_quirk'), 30, 60, 1)"],
                "transitions": {"q": "q.bedrock_primary('testmon', 'intro'), q.curve('one'))"}
            }}}"#,
        )
        .unwrap();
        let pose = &poser.poses[0];
        // `O.5` is a name no struct has, so a 0, as it is to Cobblemon.
        let Animation::Look(look) = &pose.animations[0].animation else {
            panic!("{:?}", pose.animations[0]);
        };
        assert_eq!(
            (
                look.bone.as_str(),
                look.pitch_multiplier,
                look.yaw_multiplier
            ),
            ("waist", 0.0, 0.5)
        );
        // The object without an `animation` is skipped, and what follows it is still read.
        assert_eq!(pose.animations.len(), 2);
        assert_eq!(
            pose.animations[1].animation,
            bedrock("testmon", "ground_idle")
        );
        // The parser stops at the `)` that closes the call: the timings after it are never read.
        assert_eq!(
            pose.quirks,
            [Expression::Call(call(
                "bedrock_quirk",
                vec![string("testmon"), string("idle_quirk")]
            ))]
        );
        assert_eq!(
            pose.transitions[0].1,
            Expression::Call(call(
                "bedrock_primary",
                vec![string("testmon"), string("intro")]
            ))
        );
        let oddity = |source: &str, kind| Oddity {
            pose: Some("p".to_owned()),
            source: source.to_owned(),
            kind,
        };
        assert_eq!(
            poser.oddities,
            [
                oddity(
                    "q.look('waist', O.5, 0.5)",
                    OddityKind::UnknownName("O.5".to_owned())
                ),
                oddity("", OddityKind::NoAnimation),
                oddity(
                    "q.bedrock_quirk('testmon', 'idle_quirk'), 30, 60, 1)",
                    OddityKind::Unread(", 30, 60, 1)".to_owned())
                ),
                oddity(
                    "q.bedrock_primary('testmon', 'intro'), q.curve('one'))",
                    OddityKind::Unread(", q.curve('one'))".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn the_way_of_writing_animations_that_came_before_molang_still_reads() {
        // `look` is the bone `head_ai`, or `head` in a model without one.
        let look = read("look");
        let Animation::Look(bare) = &look else {
            panic!("{look:?}");
        };
        assert_eq!(
            (bare.bone.as_str(), bare.or_bone.as_deref()),
            ("head_ai", Some("head"))
        );
        assert_eq!((bare.pitch_multiplier, bare.yaw_multiplier), (1.0, 1.0));
        assert_eq!(
            (bare.max_pitch, bare.min_pitch, bare.max_yaw, bare.min_yaw),
            (70.0, -45.0, 45.0, -45.0)
        );
        assert_eq!(look.bones(), ["head_ai", "head"]);
        assert_eq!(
            read("bedrock(testmon, ground_idle)"),
            bedrock("testmon", "ground_idle")
        );
        // Not in an object, where Cobblemon fails on it: there it is kept as written.
        let poser = posed(r#"[{"animation": "bedrock(testmon, ground_idle)"}]"#);
        assert_eq!(
            poser.poses[0].animations[0].animation,
            Animation::Other(Expression::Molang(
                "bedrock(testmon, ground_idle)".to_owned()
            ))
        );
        // One that names no animation fails Cobblemon, and the file.
        for source in ["bedrock(testmon)", "bedrock"] {
            let e = Poser::parse(&format!(
                r#"{{"poses": {{"p": {{"animations": ["{source}"]}}}}}}"#
            ))
            .unwrap_err();
            assert!(e.0.contains("names no animation"), "{}", e.0);
        }
    }

    #[test]
    fn what_gives_cobblemon_no_pose_animation_is_kept_as_parsed() {
        // Functions that give something else than a pose animation.
        assert_eq!(
            read("q.bedrock_stateful('testmon', 'blink')"),
            Animation::Other(Expression::Call(call(
                "bedrock_stateful",
                vec![string("testmon"), string("blink")]
            )))
        );
        // An argument that is not written out: Cobblemon works it out once, nothing here does.
        assert_eq!(
            read("q.bedrock('testmon', math.random(0, 1) < 0.4 ? 'sleep2' : 'sleep')"),
            Animation::Other(Expression::Call(call(
                "bedrock",
                vec![
                    string("testmon"),
                    Argument::Molang("math.random(0, 1) < 0.4 ? 'sleep2' : 'sleep'".to_owned()),
                ]
            )))
        );
        // Arguments of the wrong kind or missing, where `MoParams` throws. Double quotes make no
        // string: `head` is then a name no struct has.
        for (source, arguments) in [
            ("q.look()", vec![]),
            ("q.look(1)", vec![Argument::Number(1.0)]),
            ("q.look('head', 'a')", vec![string("head"), string("a")]),
            (r#"q.look("head")"#, vec![Argument::Number(0.0)]),
            ("q.bedrock('testmon')", vec![string("testmon")]),
            (
                "q.pitch_tilt('body', 'fast')",
                vec![string("body"), string("fast")],
            ),
        ] {
            let function = source[2..].split('(').next().unwrap();
            assert_eq!(
                read(source),
                Animation::Other(Expression::Call(call(function, arguments))),
                "{source}"
            );
        }
        // MoLang that is not one call.
        assert_eq!(
            read("q.is_ridden ? q.look('head') : q.look('neck')"),
            Animation::Other(Expression::Molang(
                "q.is_ridden ? q.look('head') : q.look('neck')".to_owned()
            ))
        );
        let other = read("q.bedrock_stateful('testmon', 'blink')");
        assert!(other.bones().is_empty());
        assert_eq!(
            reports(&other, &Motion::default(), &mut Tilt::default()),
            []
        );
    }

    #[test]
    fn a_file_cobblemon_fails_on_is_an_error() {
        let pose = |body: &str| Poser::parse(&format!(r#"{{"poses": {{"p": {{{body}}}}}}}"#));
        for (body, message) in [
            (r#""poseTypes": ["STANDING"]"#, "no pose type is `STANDING`"),
            (r#""poseTypes": null"#, "null"),
            (r#""isBattle": null"#, "null"),
            (r#""isBattle": 1"#, "expected a boolean"),
            (r#""condition": 1"#, "expected a string"),
            (r#""transformTicks": -1"#, "invalid value"),
            (r#""transformTicks": "5""#, "invalid type"),
            (r#""poseName": ["a", "b"]"#, "list has one name"),
            (r#""animations": [1]"#, "an animation"),
            (r#""animations": [{"animation": null}]"#, "null"),
            (r#""animations": "q.look('head')""#, "expected a sequence"),
            (
                r#""transformedParts": [{"position": [1, 2, 3]}]"#,
                "missing field `part`",
            ),
            (
                r#""transformedParts": [{"part": "a", "position": [1, 2]}]"#,
                "invalid length",
            ),
            (
                r#""transformedParts": [{"part": "a", "isVisible": 1}]"#,
                "a boolean or MoLang",
            ),
            (r#""quirks": [1]"#, "neither a string nor an object"),
            (r#""isStandingOnSand": true"#, "stands on"),
        ] {
            let e = pose(body).unwrap_err();
            assert!(e.0.starts_with("poser: "), "{}", e.0);
            assert!(e.0.contains(message), "{body}: {}", e.0);
        }
        for (file, message) in [
            (r#"[]"#, "an object"),
            (r#"{"poses": []}"#, "an object"),
            (r#"{"rootBone": null}"#, "null"),
            (
                r#"{"animations": {"cry": ["q.a", "q.b"]}}"#,
                "expected a string",
            ),
            (r#"{"poses": {"a": []}}"#, "an object"),
            ("{", "EOF"),
        ] {
            let e = Poser::parse(file).unwrap_err();
            assert!(e.0.contains(message), "{file}: {}", e.0);
        }
        // What a poser has besides, and a byte order mark, are read past.
        let poser = Poser::parse(
            "\u{feff}{\"profileScale\": 0.65, \"head\": \"head\", \"poses\": {\"p\": {\"battle-standing\": {}}}}",
        )
        .unwrap();
        assert_eq!(poser.poses.len(), 1);
        assert_eq!(
            (
                poser.poses[0].pose_types.len(),
                poser.poses[0].transform_ticks
            ),
            (0, 10)
        );
    }

    #[test]
    fn look_turns_the_bone_by_the_head_s_angles_within_its_limits() {
        // Looking 80 degrees down and 60 to the left: kept to 70 and -45, then 70 / 57.2958 and
        // -45 / 57.2958 radians.
        let motion = Motion {
            head_pitch: 80.0,
            head_yaw: -60.0,
            ..Motion::default()
        };
        let mut tilt = Tilt::default();
        let default = read("q.look('head')");
        assert_reports(
            &reports(&default, &motion, &mut tilt),
            &[("head", turned(1.221_730_1, -0.785_397_9, 0.0))],
        );
        // No pitch tilt ran: nothing to take back, and that is noted.
        assert_eq!(
            tilt,
            Tilt {
                previous: None,
                corrected: Some(0.0),
                pitched: false
            }
        );
        // Half of the pitch kept to 20 and the yaw the other way, at half intensity:
        // 0.5 * 20 / 57.2958 * 0.5 and -1 * 15 / 57.2958 * 0.5.
        let motion = Motion {
            head_pitch: 80.0,
            head_yaw: 15.0,
            intensity: 0.5,
            ..Motion::default()
        };
        let explicit = read("q.look('neck', 0.5, -1, 20, -10, 30, -30)");
        assert_reports(
            &reports(&explicit, &motion, &mut Tilt::default()),
            &[("neck", turned(0.087_266_43, -0.130_899_65, 0.0))],
        );
        // Limits the wrong way round are where Kotlin's `coerceIn` throws: nothing turns, nothing is
        // noted.
        let mut tilt = Tilt::default();
        let empty = read("q.look('head', 1, 1, -10, 10)");
        assert_eq!(reports(&empty, &motion, &mut tilt), []);
        assert_eq!(tilt, Tilt::default());
        assert_eq!(default.bones(), ["head"]);
    }

    #[test]
    fn look_takes_back_what_a_pitch_tilt_of_the_frame_tilted() {
        let Animation::Look(head) = read("q.look('head')") else {
            panic!("not a look");
        };
        let still = Motion::default();
        // A body tilted by 12 degrees in this frame: the head goes back by 12 / 57.2958 radians.
        let mut tilt = Tilt {
            previous: Some(12.0),
            corrected: None,
            pitched: true,
        };
        let delta = head.delta(&still, &mut tilt).unwrap();
        assert!((delta.rotation[0] + 0.209_439_44).abs() < 1e-6, "{delta:?}");
        assert_eq!(tilt.corrected, Some(12.0));
        // A second look in the frame has nothing left to take back.
        let delta = head.delta(&still, &mut tilt).unwrap();
        assert_eq!(delta, Delta::NONE);
        assert_eq!(tilt.corrected, Some(0.0));
        // In a frame without a pitch tilt, what an earlier one left is not looked at.
        let mut tilt = Tilt {
            previous: Some(12.0),
            corrected: Some(3.0),
            pitched: true,
        };
        tilt.begin_frame();
        assert_eq!(head.delta(&still, &mut tilt).unwrap(), Delta::NONE);
        assert_eq!(
            tilt,
            Tilt {
                previous: Some(12.0),
                corrected: Some(0.0),
                pitched: false
            }
        );
    }

    #[test]
    fn pitch_tilt_leans_the_bone_towards_the_slope_of_the_motion_a_step_a_frame() {
        let default = read("q.pitch_tilt('body')");
        // Climbing at 45 degrees: the target is -45, reached 1.5 degrees a frame. The first frame adds
        // -1.5 / 57.2958 radians, the second -3 / 57.2958 (the whole tilt so far, parts being reset
        // between frames).
        let climbing = Motion {
            velocity: [0.3, 0.5, 0.4],
            ..Motion::default()
        };
        let mut tilt = Tilt::default();
        assert_reports(
            &reports(&default, &climbing, &mut tilt),
            &[("body", turned(-0.026_179_93, 0.0, 0.0))],
        );
        assert_eq!(
            tilt,
            Tilt {
                previous: Some(-1.5),
                corrected: None,
                pitched: true
            }
        );
        tilt.begin_frame();
        assert_reports(
            &reports(&default, &climbing, &mut tilt),
            &[("body", turned(-0.052_359_86, 0.0, 0.0))],
        );
        assert_eq!(tilt.previous, Some(-3.0));
        // A slope within reach of one step: atan2(0.01, 0.5) is 0.019997334 radians, 1.1457633
        // degrees, and the bone turns by that, the other way.
        let gentle = Motion {
            velocity: [0.4, 0.01, 0.3],
            ..Motion::default()
        };
        let mut tilt = Tilt::default();
        assert_reports(
            &reports(&default, &gentle, &mut tilt),
            &[("body", turned(-0.019_997_334, 0.0, 0.0))],
        );
        assert!(
            (tilt.previous.unwrap() + 1.145_763_3).abs() < 1e-6,
            "{tilt:?}"
        );
        // Straight down: the minimum, -45, so a target of 45. The step is `maxChangePerTick`, here
        // 0.22 degrees: 0.22 / 57.2958.
        let falling = Motion {
            velocity: [0.0, -0.2, 0.0],
            ..Motion::default()
        };
        let slow = read("q.pitch_tilt('body', 0.22)");
        assert_reports(
            &reports(&slow, &falling, &mut Tilt::default()),
            &[("body", turned(0.003_839_723, 0.0, 0.0))],
        );
        // Not moving at all: a target of 0, so a tilt of 10 degrees comes back by 1.5.
        let mut tilt = Tilt {
            previous: Some(10.0),
            ..Tilt::default()
        };
        reports(&default, &Motion::default(), &mut tilt);
        assert_eq!(tilt.previous, Some(8.5));
        // A second pitch tilt in the frame only adds its step: 1.5 / 57.2958 * 0.5 at half intensity,
        // and 10 + 1.5 * 0.5 is what the two have tilted by.
        let mut tilt = Tilt {
            previous: Some(10.0),
            corrected: Some(4.0),
            pitched: true,
        };
        let half = Motion {
            intensity: 0.5,
            ..falling
        };
        assert_reports(
            &reports(&default, &half, &mut tilt),
            &[("body", turned(0.013_089_965, 0.0, 0.0))],
        );
        assert_eq!(
            tilt,
            Tilt {
                previous: Some(10.75),
                corrected: Some(4.0),
                pitched: true
            }
        );
        // Nothing for a ridden entity, nor where `coerceIn` throws.
        let ridden = Motion {
            ridden: true,
            ..climbing
        };
        let mut tilt = Tilt::default();
        assert_eq!(reports(&default, &ridden, &mut tilt), []);
        let backwards = read("q.pitch_tilt('body', -1)");
        assert_eq!(reports(&backwards, &climbing, &mut tilt), []);
        let empty = read("q.pitch_tilt('body', 1.5, 10, -10)");
        assert_eq!(reports(&empty, &climbing, &mut tilt), []);
        assert_eq!(tilt, Tilt::default());
        assert_eq!(default.bones(), ["body"]);
    }

    #[test]
    fn a_pitch_tilt_before_a_look_keeps_the_head_level() {
        let poser = posed(r#"["q.pitch_tilt('body')", "q.look('head')"]"#);
        let falling = Motion {
            velocity: [0.0, -0.2, 0.0],
            ..Motion::default()
        };
        let mut tilt = Tilt::default();
        let mut turns = Vec::new();
        for _frame in 0..2 {
            tilt.begin_frame();
            turns.clear();
            for entry in &poser.poses[0].animations {
                entry
                    .animation
                    .apply(&falling, &mut tilt, &mut |bone, delta| {
                        turns.push((bone.to_owned(), delta.rotation[0]));
                    });
            }
        }
        // After two frames the body is 3 degrees down and the head 3 degrees back up.
        assert_eq!((turns[0].0.as_str(), turns[1].0.as_str()), ("body", "head"));
        assert!((turns[0].1 - 0.052_359_86).abs() < 1e-6, "{turns:?}");
        assert!((turns[0].1 + turns[1].1).abs() < 1e-6, "{turns:?}");
    }

    #[test]
    fn the_walks_swing_legs_by_vanilla_s_table_cosine() {
        // `Mth.cos(2 * 0.6662)` is the table's 0.23618524 (the real cosine is 0.2361446), times a
        // limb swing amount of 0.5 and the amplitude 1.4; the right leg is half a turn ahead.
        let walking = Motion {
            limb_swing: 2.0,
            limb_swing_amount: 0.5,
            ..Motion::default()
        };
        let biped = read("q.biped_walk()");
        assert_reports(
            &reports(&biped, &walking, &mut Tilt::default()),
            &[
                ("leg_right", turned(-0.165_329_66, 0.0, 0.0)),
                ("leg_left", turned(0.165_329_66, 0.0, 0.0)),
            ],
        );
        assert_eq!(biped.bones(), ["leg_left", "leg_right"]);
        // At a limb swing of 0 the cosines are 1 and -1: 0.25 * 1.2 either way, the back right leg with
        // the front left one.
        let quadruped = read("q.quadruped_walk(1, 1.2, 'fl', 'fr', 'bl', 'br')");
        let starting = Motion {
            limb_swing_amount: 0.25,
            ..Motion::default()
        };
        assert_reports(
            &reports(&quadruped, &starting, &mut Tilt::default()),
            &[
                ("br", turned(0.3, 0.0, 0.0)),
                ("bl", turned(-0.3, 0.0, 0.0)),
                ("fr", turned(-0.3, 0.0, 0.0)),
                ("fl", turned(0.3, 0.0, 0.0)),
            ],
        );
        // `Mth.cos(3)` is the table's -0.98999065 (the real one is -0.9899925), times 0.25 * 1.2, at
        // half intensity.
        let later = Motion {
            limb_swing: 3.0,
            limb_swing_amount: 0.25,
            intensity: 0.5,
            ..Motion::default()
        };
        assert_reports(
            &reports(&quadruped, &later, &mut Tilt::default()),
            &[
                ("br", turned(-0.148_498_61, 0.0, 0.0)),
                ("bl", turned(0.148_498_61, 0.0, 0.0)),
                ("fr", turned(0.148_498_61, 0.0, 0.0)),
                ("fl", turned(-0.148_498_61, 0.0, 0.0)),
            ],
        );
        assert_eq!(quadruped.bones(), ["fl", "fr", "bl", "br"]);
        // Standing still, nothing swings.
        for (_, delta) in reports(&biped, &Motion::default(), &mut Tilt::default()) {
            assert_eq!(delta, Delta::NONE);
        }
    }

    #[test]
    fn bimanual_swing_swings_both_arms_together_and_sways_them_apart() {
        // Both arms: `Mth.cos(0) * 0.5 * 1` around y. Then, 10 ticks old: `Mth.cos(0.9) * 0.05 + 0.05`
        // = 0.08108178 around z and `Mth.sin(0.67) * 0.05` = 0.03104797 around y, the left arm the
        // other way.
        let motion = Motion {
            limb_swing_amount: 0.5,
            age_in_ticks: 10.0,
            ..Motion::default()
        };
        let swing = read("q.bimanual_swing()");
        assert_reports(
            &reports(&swing, &motion, &mut Tilt::default()),
            &[
                ("arm_right", turned(0.0, 0.5, 0.0)),
                ("arm_left", turned(0.0, 0.5, 0.0)),
                ("arm_right", turned(0.0, 0.031_047_97, 0.081_081_78)),
                ("arm_left", turned(0.0, -0.031_047_97, -0.081_081_78)),
            ],
        );
        // `Mth.cos(1.5 * 0.66) * 0.8 * 0.6 * 0.5` = 0.13168702 at half intensity, which halves the sway
        // too.
        let motion = Motion {
            limb_swing: 1.5,
            limb_swing_amount: 0.8,
            age_in_ticks: 10.0,
            intensity: 0.5,
            ..Motion::default()
        };
        let swing = read("q.bimanual_swing(0.66, 0.6, 'l', 'r')");
        assert_reports(
            &reports(&swing, &motion, &mut Tilt::default()),
            &[
                ("r", turned(0.0, 0.131_687_02, 0.0)),
                ("l", turned(0.0, 0.131_687_02, 0.0)),
                ("r", turned(0.0, 0.015_523_985, 0.040_540_89)),
                ("l", turned(0.0, -0.015_523_985, -0.040_540_89)),
            ],
        );
        assert_eq!(swing.bones(), ["l", "r"]);
    }

    #[test]
    fn sine_wing_flap_beats_the_wings_against_each_other() {
        // A sixth of the 0.9 second period in: sin(pi / 3) * 0.9 = 0.7794229 around y.
        let motion = Motion {
            animation_seconds: 0.15,
            // Not read: the flap is the same at any intensity.
            intensity: 0.25,
            ..Motion::default()
        };
        let flap = read("q.sine_wing_flap()");
        assert_reports(
            &reports(&flap, &motion, &mut Tilt::default()),
            &[
                ("wing_left", turned(0.0, 0.779_422_94, 0.0)),
                ("wing_right", turned(0.0, -0.779_422_94, 0.0)),
            ],
        );
        // A quarter of the period in, the sine is 1: 0.9 + 25 around z, the shift taken as radians.
        let motion = Motion {
            animation_seconds: 0.225,
            ..Motion::default()
        };
        let flap = read("q.sine_wing_flap(0.9, 0.9, 25, 'z', 'wing_open_left', 'wing_open_right')");
        assert_eq!(
            reports(&flap, &motion, &mut Tilt::default()),
            [
                ("wing_open_left".to_owned(), turned(0.0, 0.0, 25.9)),
                ("wing_open_right".to_owned(), turned(0.0, 0.0, -25.9)),
            ]
        );
        assert_eq!(flap.bones(), ["wing_open_left", "wing_open_right"]);
        // A period of 0 is a division by zero in Cobblemon too: not a number, and no panic.
        let broken = read("q.sine_wing_flap(0.9, 0)");
        let got = reports(&broken, &Motion::default(), &mut Tilt::default());
        assert!(got[0].1.rotation[1].is_nan(), "{got:?}");
    }

    #[test]
    fn the_table_sine_is_vanilla_s() {
        // The entries at the quarter turns are exact, and an angle is cut down to its entry.
        assert_eq!([mth_sin(0.0), mth_cos(0.0)], [0.0, 1.0]);
        assert_eq!(mth_cos(PI), -1.0);
        assert_eq!(mth_sin(PI / 2.0), 1.0);
        assert_eq!(mth_sin(-PI / 2.0), -1.0);
        // 1 * 10430.378 is entry 10430: sin(10430 * 2 pi / 65536) = 0.8414514, where sin(1) is
        // 0.84147098.
        assert!(
            (mth_sin(1.0) - 0.841_451_4).abs() < 1e-7,
            "{}",
            mth_sin(1.0)
        );
        // An angle no entry answers to is cut as a Java cast cuts it: to entry 0 when it is not a
        // number, to the largest int when it is infinite.
        assert_eq!(mth_sin(f32::NAN), 0.0);
        assert_eq!(mth_sin(f32::INFINITY), sine_table(65535));
    }

    #[test]
    fn an_entity_and_a_motion_have_defaults_to_fill_in() {
        let entity = Entity::default();
        assert_eq!(entity.pose, PoseType::Stand);
        assert!(!entity.battle && !entity.touching_water && entity.ride_style.is_none());
        let motion = Motion::default();
        assert_eq!((motion.intensity, motion.limb_swing_amount), (1.0, 0.0));
        assert_eq!(Tilt::default().previous, None);
    }
}
