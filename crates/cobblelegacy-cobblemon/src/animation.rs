// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Bedrock animation file (`*.animation.json`) as Cobblemon 1.8.1 reads it (`BedrockAnimationAdapter`)
//! and plays it (`BedrockAnimation.run`, `BedrockKeyFrameBoneValue.resolve`, `catmullromLerp`): for each
//! bone its `position`, `rotation` and `scale`, each three MoLang expressions or keyframes of them.
//!
//! The bones are read, and the sounds an animation plays as it goes (`sound_effects`: a cry is one of
//! those). An animation's `particle_effects` and `timeline`, which Cobblemon also reads (and a file
//! whose particle effect it does not know is a file it drops), and whatever else is in it are skipped
//! unread.
//!
//! A file Cobblemon would fail to load is an error here, with one exception: a component whose MoLang
//! does not compile (see [`crate::molang`] for what that is) is counted in [`Census::unparsed`] and stands
//! for no change, 0 or, in a scale, 1, where Cobblemon would drop the whole file. The JSON is read
//! strictly where Gson is lenient: no comments or unquoted names, a keyframe's time is a plain number, a
//! component is a number or text and not a one-element array, `lerp_mode` is text.
//!
//! One thing cannot be told apart from here. Gson hands MoLang a number's own text, so a number written
//! with an exponent (`1e-5`) is, to Cobblemon, what precedes the `e`; here it is the number. No file of
//! Cobblemon 1.8.1 has one.
use crate::{
    molang::{self, Context, Expression, Inputs},
    pose::Delta,
};
use horizon_protocol::{Error, Result};
use serde::{
    Deserialize,
    de::{
        self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Unexpected, Visitor,
    },
};
use std::{collections::BTreeMap, fmt};

/// `RADIAN_IN_DEGREES` of Cobblemon's `Number.toRadians`: a rotation's degrees, as an `f32`, are divided by
/// this and not by 180/π.
const RADIAN_IN_DEGREES: f32 = 57.2958;

/// One animation file: Cobblemon's `BedrockAnimationGroup`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationFile {
    /// In file order.
    pub animations: Vec<Animation>,
}

/// One animation: Cobblemon's `BedrockAnimation`, without its particles and instructions.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// Its key in the file, such as `animation.pikachu.ground_idle`.
    pub name: String,
    /// The file's `loop`, when it is `true` (or the text "true"): `"hold_on_last_frame"`, which Cobblemon
    /// does not know, is false. Time only wraps when there is a length too, see [`Self::wraps`].
    pub looping: bool,
    /// `animation_length`, in seconds.
    pub length: Option<f32>,
    /// The bones the animation names, in file order, whether or not it moves them.
    pub bones: Vec<BoneChannels>,
    /// `sound_effects`, by the moment they play at: Cobblemon's `BedrockSoundKeyframe`s.
    pub sounds: Vec<SoundKeyframe>,
}

/// A sound an animation plays `seconds` after it starts (`BedrockSoundKeyframe`).
#[derive(Clone, Debug, PartialEq)]
pub struct SoundKeyframe {
    pub seconds: f32,
    /// The sound event, with its namespace (`cobblemon:pokemon.pikachu.cry`): the file's `effect`
    /// in lower case, in Cobblemon's namespace when it names none
    /// (`asIdentifierDefaultingNamespace`).
    pub sound: String,
}

/// What an animation does to one bone: Cobblemon's `BedrockBoneTimeline`.
#[derive(Clone, Debug, PartialEq)]
pub struct BoneChannels {
    /// The bone's name. `BedrockAnimation.run` looks it up among the model's parts, and takes `root_part`
    /// for the model's root when no part has that name.
    pub bone: String,
    position: Option<Channel>,
    rotation: Option<Channel>,
    scale: Option<Channel>,
}

/// A component whose MoLang did not compile, and that stands for no change instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unparsed {
    pub animation: String,
    pub bone: String,
    /// The component as the file has it.
    pub source: String,
    pub error: String,
}

/// What files are made of, counted as they are read ([`AnimationFile::parse_counting`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Channels written as numbers alone: three of them or, a scale, one.
    pub number_channels: usize,
    /// Channels of which a component at least is written as MoLang text.
    pub molang_channels: usize,
    /// Channels of keyframes. One without any keyframe is no channel and is not counted.
    pub keyframed_channels: usize,
    pub keyframes: usize,
    /// Components written as numbers, in channels and in keyframes.
    pub numbers: usize,
    /// Components written as MoLang text.
    pub expressions: usize,
    /// Of `expressions`, those that are a number once compiled: nothing in them depends on the moment.
    pub constant_expressions: usize,
    /// How often the expressions refer to each name, as [`Expression::parse_noting`] reports names.
    pub names: BTreeMap<String, usize>,
    /// The sounds of every animation's `sound_effects`.
    pub sounds: usize,
    /// How often an animation has each of the keys that are skipped.
    pub ignored_animation_keys: BTreeMap<String, usize>,
    /// Likewise for a bone's keys.
    pub ignored_bone_keys: BTreeMap<String, usize>,
    pub unparsed: Vec<Unparsed>,
}

/// Cobblemon's `Transformation`: which of a bone's three channels.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Transformation {
    Position,
    Rotation,
    Scale,
}

impl Transformation {
    /// `MolangBoneValue.yMul`: a position's y is turned over, Bedrock's pointing up and a part's down.
    fn y(self) -> f64 {
        match self {
            Self::Position => -1.0,
            Self::Rotation | Self::Scale => 1.0,
        }
    }

    /// What changes nothing: where a component that does not compile is left.
    fn rest(self) -> f64 {
        match self {
            Self::Position | Self::Rotation => 0.0,
            Self::Scale => 1.0,
        }
    }
}

/// `MolangBoneValue`: three expressions.
#[derive(Clone, Debug, PartialEq)]
enum Vector {
    /// None of the three depends on the moment.
    Numbers([f64; 3]),
    Expressions(Box<[Expression; 3]>),
}

impl Vector {
    /// `MolangBoneValue.resolve`, with `y` for its `yMul`.
    fn resolve(&self, inputs: &Inputs, y: f64) -> [f64; 3] {
        let [x, up, z] = match self {
            Self::Numbers(numbers) => *numbers,
            Self::Expressions(expressions) => {
                let [x, up, z] = &**expressions;
                [
                    x.eval_exact(inputs),
                    up.eval_exact(inputs),
                    z.eval_exact(inputs),
                ]
            }
        };
        [x, up * y, z]
    }
}

/// `BedrockAnimationKeyFrame`.
#[derive(Clone, Debug, PartialEq)]
struct Keyframe {
    /// Seconds.
    time: f64,
    /// What the channel arrives at; nothing when that is `post` too.
    pre: Option<Box<Vector>>,
    /// What the channel leaves from.
    post: Vector,
    /// `InterpolationType.SMOOTH`: the keyframe's `lerp_mode` is `catmullrom`.
    smooth: bool,
}

impl Keyframe {
    fn pre(&self) -> &Vector {
        self.pre.as_deref().unwrap_or(&self.post)
    }
}

/// A `BedrockBoneValue` that is not empty.
#[derive(Clone, Debug, PartialEq)]
enum Channel {
    Value(Vector),
    /// `BedrockKeyFrameBoneValue`: at least one keyframe, in the order of their times, no two at one time.
    Keyframes(Box<[Keyframe]>),
}

impl Channel {
    fn resolve(&self, time: f64, inputs: &Inputs, y: f64) -> [f64; 3] {
        match self {
            Self::Value(vector) => vector.resolve(inputs, y),
            Self::Keyframes(frames) => resolve_keyframes(frames, time, inputs, y),
        }
    }
}

/// `linearLerpAlpha`.
fn linear_lerp_alpha(before: f64, after: f64, value: f64) -> f64 {
    (value - before) / (after - before)
}

/// `BedrockKeyFrameBoneValue.resolve`: the keyframe at or before `time` and the one after it; between
/// two, a straight line from the first's `post` to the second's `pre`, or a spline if either is smooth;
/// before the first its `pre`, from the last on its `post`.
fn resolve_keyframes(frames: &[Keyframe], time: f64, inputs: &Inputs, y: f64) -> [f64; 3] {
    // The first keyframe whose time is greater than `time`: none when `time` is not a number.
    let after_index = frames.partition_point(|frame| !(frame.time > time));
    let after = frames.get(after_index);
    let before_index = after_index.checked_sub(1);
    let before = before_index.and_then(|index| frames.get(index));
    match (before, after) {
        (Some(before), Some(after)) if before.smooth || after.smooth => {
            // The keyframes on either side of the two, where there are any.
            let before_plus = before_index
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| frames.get(index));
            let after_plus = frames.get(after_index + 1);
            catmullrom_lerp(before_plus, before, after, after_plus, time, inputs, y)
        }
        (Some(before), Some(after)) => {
            let from = before.post.resolve(inputs, y);
            let to = after.pre().resolve(inputs, y);
            let alpha = linear_lerp_alpha(before.time, after.time, time);
            std::array::from_fn(|axis| from[axis] + (to[axis] - from[axis]) * alpha)
        }
        (Some(before), None) => before.post.resolve(inputs, y),
        (None, Some(after)) => after.pre().resolve(inputs, y),
        (None, None) => [0.0; 3],
    }
}

/// `catmullromLerp`: between `b` and `c`, a Catmull-Rom spline through `a`'s `post`, `b`'s `post`, `c`'s
/// `pre` and `d`'s `pre`, spaced evenly whatever their times. A missing `a` or `d` is the point next to it
/// over again.
fn catmullrom_lerp(
    a: Option<&Keyframe>,
    b: &Keyframe,
    c: &Keyframe,
    d: Option<&Keyframe>,
    time: f64,
    inputs: &Inputs,
    y: f64,
) -> [f64; 3] {
    let mut points = [[0.0; 3]; 4];
    let mut count = 0;
    let frames = [
        a.map(|a| &a.post),
        Some(&b.post),
        Some(c.pre()),
        d.map(Keyframe::pre),
    ];
    for vector in frames.into_iter().flatten() {
        points[count] = vector.resolve(inputs, y);
        count += 1;
    }
    let last = count - 1;
    let lead = if a.is_some() { 1.0 } else { 0.0 };
    let alpha = (linear_lerp_alpha(b.time, c.time, time) + lead) / last as f64;
    // `getPointOnSpline`. Its `intPoint` is `b`'s place among the points, or `c`'s when rounding makes
    // the way from `b` to `c` a whole 1; never past the end, where Kotlin would throw.
    let p = last as f64 * alpha;
    let point = (p.floor() as usize).min(last);
    let weight = p - point as f64;
    let p0 = point.saturating_sub(1);
    let p2 = if point + 2 > count { last } else { point + 1 };
    let p3 = if point + 3 > count { last } else { point + 2 };
    std::array::from_fn(|axis| {
        catmullrom(
            weight,
            points[p0][axis],
            points[point][axis],
            points[p2][axis],
            points[p3][axis],
        )
    })
}

/// `catmullrom` of `InterpolationMath.kt`, its operations in its order.
fn catmullrom(t: f64, p0: f64, p1: f64, p2: f64, p3: f64) -> f64 {
    let v0 = (p2 - p0) * 0.5;
    let v1 = (p3 - p1) * 0.5;
    let t2 = t * t;
    let t3 = t * t2;
    (2.0 * p1 - 2.0 * p2 + v0 + v1) * t3 + (-3.0 * p1 + 3.0 * p2 - 2.0 * v0 - v1) * t2 + v0 * t + p1
}

/// What `BedrockAnimation.run` multiplies a part's scale by, at an intensity of 1. The way round by the
/// deviation from 1 is Cobblemon's, and is not always the scale itself to the last bit.
fn scale_at_full_intensity(scale: [f64; 3]) -> [f64; 3] {
    // `scale == Vec3.ZERO`, which a negative zero is not.
    if scale.iter().all(|component| component.to_bits() == 0) {
        return scale;
    }
    scale.map(|component| ((component * -1.0 + 1.0) * 1.0 - 1.0) * -1.0)
}

impl AnimationFile {
    /// Reads a `*.animation.json`.
    pub fn parse(json: &str) -> Result<Self> {
        Self::parse_counting(json, &mut Census::default())
    }

    /// [`Self::parse`], adding what the file is made of to `census`: the only place the components that
    /// did not compile are told.
    pub fn parse_counting(json: &str, census: &mut Census) -> Result<Self> {
        let bad = |e: serde_json::Error| Error::new(format!("animation: {e}"));
        let mut reader = Reader {
            census,
            animation: String::new(),
            bone: String::new(),
            seed: 0,
        };
        // A byte order mark is skipped, as Gson skips it.
        let text = json.strip_prefix('\u{feff}').unwrap_or(json);
        let mut deserializer = serde_json::Deserializer::from_str(text);
        let animations = FileSeed(&mut reader)
            .deserialize(&mut deserializer)
            .map_err(bad)?;
        deserializer.end().map_err(bad)?;
        Ok(Self { animations })
    }

    pub fn get(&self, name: &str) -> Option<&Animation> {
        self.animations
            .iter()
            .find(|animation| animation.name == name)
    }
}

impl Animation {
    /// Cobblemon's `shouldLoop`: whether time starts over at the animation's length. Without a positive
    /// length it does not, `loop` or not: the animation goes on and its keyframes hold their last value.
    pub fn wraps(&self) -> bool {
        self.looping && self.length.is_some_and(|length| length > 0.0)
    }

    /// Whether `BedrockAnimation.run` answers false `seconds` after the animation started: it has a
    /// positive length, does not wrap, and that length is past. From then on it moves nothing, where
    /// Bedrock's `hold_on_last_frame` would keep the last pose.
    ///
    /// (Cobblemon compares with the length as a `double`: that differs for the one `seconds` that is the
    /// length rounded up to an `f32`, which is past there and not yet here.)
    pub fn is_over(&self, seconds: f32) -> bool {
        !self.wraps()
            && self
                .length
                .is_some_and(|length| length > 0.0 && seconds > length)
    }

    /// What the animation does `seconds` after it started, as `BedrockAnimation.run` at an intensity of
    /// 1: for each bone it moves, its index in `bones` and the change to that part's pose. Calls `apply`
    /// at most once a bone, in the order of `bones`, and not at all once the animation [`Self::is_over`];
    /// never allocates.
    ///
    /// `seconds` wraps if the animation [`Self::wraps`]; it is what keyframes are looked up by and what
    /// `variable.anim_time` reads. `query.anim_time` is not that but `context.anim_time`, the state's own
    /// seconds, which a pose's animation is given as `seconds` too and an animation started later is not.
    ///
    /// A [`Delta`] is in part space: its position is the channel's `(x, -y, z)`, its rotation the
    /// channel's degrees over 57.2958 with their signs, its scale the channel's (1 without one). A channel
    /// that divides by zero gives what Cobblemon gets, which is not finite.
    pub fn sample(&self, seconds: f32, context: &Context, apply: &mut dyn FnMut(usize, Delta)) {
        if self.is_over(seconds) {
            return;
        }
        let seconds = match self.length {
            Some(length) if self.wraps() => seconds % length,
            _ => seconds,
        };
        let time = f64::from(seconds);
        let inputs = Inputs::new(context, time);
        for (index, bone) in self.bones.iter().enumerate() {
            if bone.position.is_none() && bone.rotation.is_none() && bone.scale.is_none() {
                continue;
            }
            let mut delta = Delta::NONE;
            let resolve = |channel: &Channel, transformation: Transformation| {
                channel.resolve(time, &inputs, transformation.y())
            };
            if let Some(channel) = &bone.position {
                delta.position =
                    resolve(channel, Transformation::Position).map(|pixels| pixels as f32);
            }
            if let Some(channel) = &bone.rotation {
                delta.rotation = resolve(channel, Transformation::Rotation)
                    .map(|degrees| degrees as f32 / RADIAN_IN_DEGREES);
            }
            if let Some(channel) = &bone.scale {
                delta.scale = scale_at_full_intensity(resolve(channel, Transformation::Scale))
                    .map(|factor| factor as f32);
            }
            apply(index, delta);
        }
    }
}

/// `BedrockAnimationAdapter.cleanExpression`: what Cobblemon does to a component's text before MoLang
/// reads it. None of it changes what the text computes, short of a name that ends in `q`.
fn clean_expression(value: &str) -> String {
    let value = value.strip_prefix('+').unwrap_or(value);
    let value = match value.strip_prefix("-(") {
        Some(rest) => format!("-1*({rest}"),
        None => value.to_owned(),
    };
    value
        .replace("*+", "*")
        .replace("q.", "query.")
        .replace("camera_rotation(0)", "camera_rotation_x")
        .replace("camera_rotation(1)", "camera_rotation_y")
}

fn count(counts: &mut BTreeMap<String, usize>, name: &str) {
    match counts.get_mut(name) {
        Some(count) => *count += 1,
        None => {
            counts.insert(name.to_owned(), 1);
        }
    }
}

/// A component as the file has it: what Gson's `getAsString` makes text of for MoLang to read.
#[derive(Clone, Copy)]
enum Written<'a> {
    Number(f64),
    Bool(bool),
    Text(&'a str),
}

/// What reading a file carries along.
struct Reader<'c> {
    census: &'c mut Census,
    /// The animation and the bone being read, for [`Unparsed`].
    animation: String,
    bone: String,
    /// What tells the next expression's `math.random`s from every other's: it depends on the animation's
    /// name and on the expression's place in it, and so is the same each time the file is read.
    seed: u64,
}

impl Reader<'_> {
    fn next_seed(&mut self) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.seed
    }

    /// The expression a component is and whether it was written as text. Text that does not compile is
    /// told to the census and left at rest.
    fn component(
        &mut self,
        transformation: Transformation,
        written: Written,
    ) -> (Expression, bool) {
        let source = match written {
            // MoLang reads the number's own text back: the same number (see the module's description
            // for the exception).
            Written::Number(value) => {
                self.census.numbers += 1;
                return (Expression::exact(value), false);
            }
            // "true" and "false" are MoLang's 1 and 0.
            Written::Bool(value) => {
                self.census.numbers += 1;
                return (Expression::exact(if value { 1.0 } else { 0.0 }), false);
            }
            Written::Text(source) => source,
        };
        self.census.expressions += 1;
        let seed = self.next_seed();
        let names = &mut self.census.names;
        let compiled = Expression::compile(&clean_expression(source), seed, &mut |name| {
            count(names, name)
        });
        let expression = match compiled {
            Ok(expression) => {
                if expression.constant_exact().is_some() {
                    self.census.constant_expressions += 1;
                }
                expression
            }
            Err(error) => {
                self.census.unparsed.push(Unparsed {
                    animation: self.animation.clone(),
                    bone: self.bone.clone(),
                    source: source.to_owned(),
                    error: error.0,
                });
                Expression::exact(transformation.rest())
            }
        };
        (expression, true)
    }

    /// A scale written as one component: Cobblemon makes it three by reading it three times, so its three
    /// axes roll a `math.random` each.
    fn uniform(&mut self, written: Written) -> (Vector, bool) {
        let (first, text) = self.component(Transformation::Scale, written);
        let vector = match (first.constant_exact(), written) {
            (Some(number), _) => Vector::Numbers([number; 3]),
            (None, Written::Text(source)) => {
                let cleaned = clean_expression(source);
                let mut again = || {
                    Expression::compile(&cleaned, self.next_seed(), &mut |_| {})
                        .unwrap_or_else(|_| first.clone())
                };
                let (second, third) = (again(), again());
                Vector::Expressions(Box::new([first, second, third]))
            }
            // A number or a boolean is always a constant.
            (None, _) => Vector::Expressions(Box::new([first.clone(), first.clone(), first])),
        };
        (vector, text)
    }

    fn count_channel(&mut self, text: bool) {
        match text {
            true => self.census.molang_channels += 1,
            false => self.census.number_channels += 1,
        }
    }
}

/// Makes each of these visitors the seed that hands itself to `$method`: what a seed reads depends on
/// what it holds, which a plain `Deserialize` cannot.
macro_rules! seed {
    ($($name:ident => $method:ident),+ $(,)?) => {$(
        impl<'de> DeserializeSeed<'de> for $name<'_, '_> {
            type Value = <Self as Visitor<'de>>::Value;
            fn deserialize<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> std::result::Result<Self::Value, D::Error> {
                deserializer.$method(self)
            }
        }
    )+};
}

seed!(
    FileSeed => deserialize_map,
    AnimationsSeed => deserialize_map,
    AnimationSeed => deserialize_map,
    BonesSeed => deserialize_map,
    BoneSeed => deserialize_map,
    ChannelSeed => deserialize_any,
    KeyframeSeed => deserialize_any,
    VectorSeed => deserialize_seq,
    ComponentSeed => deserialize_any,
);

/// An object's key: its place among the names its reader knows, or the key itself.
enum Key {
    Known(usize),
    Other(String),
}

struct KeyAmong(&'static [&'static str]);

impl<'de> DeserializeSeed<'de> for KeyAmong {
    type Value = Key;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Key, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for KeyAmong {
    type Value = Key;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a key")
    }
    fn visit_str<E: de::Error>(self, key: &str) -> std::result::Result<Key, E> {
        Ok(match self.0.iter().position(|known| *known == key) {
            Some(place) => Key::Known(place),
            None => Key::Other(key.to_owned()),
        })
    }
}

/// A keyframe's key: its time in seconds. Kotlin's `toDouble` takes more spellings (spaces around, a `d`
/// at the end, hexadecimal, `NaN`); only a plain finite number is taken here.
struct Time;

impl<'de> DeserializeSeed<'de> for Time {
    type Value = f64;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<f64, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for Time {
    type Value = f64;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a keyframe's time")
    }
    fn visit_str<E: de::Error>(self, key: &str) -> std::result::Result<f64, E> {
        match key.parse::<f64>() {
            Ok(time) if time.is_finite() => Ok(time),
            _ => Err(E::invalid_value(Unexpected::Str(key), &self)),
        }
    }
}

/// An animation's `loop` as Gson's `getAsBoolean` reads it: `true`, or the text "true" in any case.
/// `"hold_on_last_frame"`, any other text and any number are false.
struct Loop(bool);

impl<'de> Deserialize<'de> for Loop {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Value;
        impl Visitor<'_> for Value {
            type Value = Loop;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an animation's `loop`: true, false or text")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Loop, E> {
                Ok(Loop(value))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Loop, E> {
                Ok(Loop(value.eq_ignore_ascii_case("true")))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Loop, E> {
                Ok(Loop(false))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Loop, E> {
                Ok(Loop(false))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Loop, E> {
                Ok(Loop(false))
            }
        }
        deserializer.deserialize_any(Value)
    }
}

/// A keyframe's `lerp_mode`: whether it is `catmullrom`, the one mode Cobblemon tells from linear.
struct Smooth(bool);

impl<'de> Deserialize<'de> for Smooth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Value;
        impl Visitor<'_> for Value {
            type Value = Smooth;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a keyframe's `lerp_mode`, text")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Smooth, E> {
                Ok(Smooth(value == "catmullrom"))
            }
        }
        deserializer.deserialize_str(Value)
    }
}

/// The places of `names` in the order of the names, those of one name in the order they came.
fn by_name<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut places: Vec<_> = names.zip(0..).collect();
    places.sort_unstable();
    places
}

/// The file: an object with `animations`. Its `format_version` is not looked at, as in Cobblemon.
struct FileSeed<'r, 'c>(&'r mut Reader<'c>);

impl<'de> Visitor<'de> for FileSeed<'_, '_> {
    type Value = Vec<Animation>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an animation file, an object")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut animations = None;
        while let Some(key) = map.next_key_seed(KeyAmong(&["animations"]))? {
            match key {
                Key::Known(_) => {
                    animations = Some(map.next_value_seed(AnimationsSeed(&mut *self.0))?);
                }
                Key::Other(_) => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        // Cobblemon fails on the null it is left with.
        animations.ok_or_else(|| de::Error::missing_field("animations"))
    }
}

struct AnimationsSeed<'r, 'c>(&'r mut Reader<'c>);

impl<'de> Visitor<'de> for AnimationsSeed<'_, '_> {
    type Value = Vec<Animation>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a file's `animations`, an object")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut animations: Vec<Animation> = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            self.0.animation.clone_from(&name);
            self.0.seed = molang::hash(&name);
            animations.push(map.next_value_seed(AnimationSeed(&mut *self.0, name))?);
        }
        // Gson's map of them refuses a key it already has.
        let places = by_name(animations.iter().map(|animation| animation.name.as_str()));
        if let Some(pair) = places.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(de::Error::custom(format_args!(
                "two animations are named `{}`",
                pair[0].0
            )));
        }
        Ok(animations)
    }
}

/// `BedrockAnimationAdapter.deserialize`. A key given twice counts the last time, as in every object
/// from here down: Gson's `JsonObject` keeps the last value.
struct AnimationSeed<'r, 'c>(&'r mut Reader<'c>, String);

impl<'de> Visitor<'de> for AnimationSeed<'_, '_> {
    type Value = Animation;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an animation, an object")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let AnimationSeed(reader, name) = self;
        let (mut looping, mut length, mut bones) = (false, None, Vec::new());
        let mut sounds = Vec::new();
        const KEYS: &[&str] = &["loop", "animation_length", "bones", "sound_effects"];
        while let Some(key) = map.next_key_seed(KeyAmong(KEYS))? {
            match key {
                Key::Known(0) => looping = map.next_value::<Loop>()?.0,
                Key::Known(1) => length = Some(map.next_value::<f64>()? as f32),
                Key::Known(2) => bones = map.next_value_seed(BonesSeed(&mut *reader))?,
                Key::Known(_) => sounds = map.next_value::<SoundEffects>()?.0,
                Key::Other(key) => {
                    map.next_value::<IgnoredAny>()?;
                    count(&mut reader.census.ignored_animation_keys, &key);
                }
            }
        }
        reader.census.sounds += sounds.len();
        Ok(Animation {
            name,
            looping,
            length,
            bones,
            sounds,
        })
    }
}

/// An animation's `sound_effects` as `BedrockAnimationAdapter` reads them: an object of moments, each
/// with one effect or an array of them, an effect being an object whose `effect` names the sound.
/// What is at a moment and is neither is passed over, as there; an effect without a name, or what is
/// in an array and is no object, fails the file, as there.
struct SoundEffects(Vec<SoundKeyframe>);

impl<'de> Deserialize<'de> for SoundEffects {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Moments;
        impl<'de> Visitor<'de> for Moments {
            type Value = SoundEffects;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an animation's `sound_effects`, an object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<SoundEffects, A::Error> {
                let mut sounds = Vec::new();
                while let Some(seconds) = map.next_key_seed(Time)? {
                    let effects = match map.next_value::<serde_json::Value>()? {
                        serde_json::Value::Array(effects) => effects,
                        effect @ serde_json::Value::Object(_) => vec![effect],
                        _ => continue,
                    };
                    for effect in effects {
                        let name = match effect.as_object().map(|effect| effect.get("effect")) {
                            Some(Some(serde_json::Value::String(name))) => name.clone(),
                            // Gson's `getAsString` of a number or a boolean is its text.
                            Some(Some(
                                name @ (serde_json::Value::Number(_) | serde_json::Value::Bool(_)),
                            )) => name.to_string(),
                            _ => {
                                return Err(de::Error::custom(
                                    "a sound effect that is no object with an `effect`",
                                ));
                            }
                        };
                        let name = name.to_lowercase();
                        sounds.push(SoundKeyframe {
                            seconds: seconds as f32,
                            sound: if name.contains(':') {
                                name
                            } else {
                                format!("cobblemon:{name}")
                            },
                        });
                    }
                }
                // In the order they play; those of one moment in the order they were read.
                sounds.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
                Ok(SoundEffects(sounds))
            }
        }
        deserializer.deserialize_map(Moments)
    }
}

struct BonesSeed<'r, 'c>(&'r mut Reader<'c>);

impl<'de> Visitor<'de> for BonesSeed<'_, '_> {
    type Value = Vec<BoneChannels>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an animation's `bones`, an object")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut bones = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            self.0.bone.clone_from(&name);
            bones.push(map.next_value_seed(BoneSeed(&mut *self.0, name))?);
        }
        merge_repeated(&mut bones);
        Ok(bones)
    }
}

/// Makes the bones of one name one bone: the last one's channels, where the first one stood.
fn merge_repeated(bones: &mut Vec<BoneChannels>) {
    let places = by_name(bones.iter().map(|bone| bone.bone.as_str()));
    let mut kept = vec![true; bones.len()];
    // For each repeated name, its first place and its last.
    let mut repeated = Vec::new();
    for group in places.chunk_by(|a, b| a.0 == b.0) {
        if let [(_, first), rest @ ..] = group
            && let Some((_, last)) = rest.last()
        {
            repeated.push((*first, *last));
            for (_, place) in rest {
                kept[*place] = false;
            }
        }
    }
    for (first, last) in repeated {
        let from = &mut bones[last];
        let channels = (
            from.position.take(),
            from.rotation.take(),
            from.scale.take(),
        );
        let to = &mut bones[first];
        (to.position, to.rotation, to.scale) = channels;
    }
    let mut kept = kept.into_iter();
    bones.retain(|_| kept.next().unwrap_or(true));
}

/// `BedrockAnimationAdapter.deserializeBoneTimeline`.
struct BoneSeed<'r, 'c>(&'r mut Reader<'c>, String);

impl<'de> Visitor<'de> for BoneSeed<'_, '_> {
    type Value = BoneChannels;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a bone's channels, an object")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let BoneSeed(reader, name) = self;
        let mut bone = BoneChannels {
            bone: name,
            position: None,
            rotation: None,
            scale: None,
        };
        while let Some(key) = map.next_key_seed(KeyAmong(&["position", "rotation", "scale"]))? {
            let (channel, transformation) = match key {
                Key::Known(0) => (&mut bone.position, Transformation::Position),
                Key::Known(1) => (&mut bone.rotation, Transformation::Rotation),
                Key::Known(_) => (&mut bone.scale, Transformation::Scale),
                Key::Other(key) => {
                    map.next_value::<IgnoredAny>()?;
                    count(&mut reader.census.ignored_bone_keys, &key);
                    continue;
                }
            };
            *channel = map.next_value_seed(ChannelSeed(&mut *reader, transformation))?;
        }
        Ok(bone)
    }
}

/// One channel of a bone: an object of keyframes, three components, or, for a scale alone, one component
/// for all three axes. Nothing when it is an object without a keyframe: `BedrockKeyFrameBoneValue` is a
/// map, and an empty map is to `BedrockAnimation.run` an empty value.
struct ChannelSeed<'r, 'c>(&'r mut Reader<'c>, Transformation);

impl ChannelSeed<'_, '_> {
    fn uniform<E: de::Error>(
        self,
        written: Written,
        found: Unexpected,
    ) -> std::result::Result<Option<Channel>, E> {
        if self.1 != Transformation::Scale {
            return Err(E::invalid_type(found, &self));
        }
        let (vector, text) = self.0.uniform(written);
        self.0.count_channel(text);
        Ok(Some(Channel::Value(vector)))
    }
}

impl<'de> Visitor<'de> for ChannelSeed<'_, '_> {
    type Value = Option<Channel>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self.1 {
            Transformation::Position => "a bone's position, keyframes or `[x, y, z]`",
            Transformation::Rotation => "a bone's rotation, keyframes or `[x, y, z]`",
            Transformation::Scale => "a bone's scale, keyframes, `[x, y, z]` or one for the three",
        })
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let ChannelSeed(reader, transformation) = self;
        let mut frames = Vec::new();
        while let Some(time) = map.next_key_seed(Time)? {
            frames.push(map.next_value_seed(KeyframeSeed(&mut *reader, transformation, time))?);
        }
        // A `TreeMap`: in the order of the times, the last one put for a time.
        if !frames.is_sorted_by(|a, b| a.time.total_cmp(&b.time).is_lt()) {
            frames.sort_by(|a, b| a.time.total_cmp(&b.time));
            frames.dedup_by(|later, earlier| {
                let same = later.time.to_bits() == earlier.time.to_bits();
                if same {
                    std::mem::swap(later, earlier);
                }
                same
            });
        }
        if frames.is_empty() {
            return Ok(None);
        }
        reader.census.keyframed_channels += 1;
        reader.census.keyframes += frames.len();
        Ok(Some(Channel::Keyframes(frames.into_boxed_slice())))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> std::result::Result<Self::Value, A::Error> {
        let (vector, text) = read_vector(self.0, self.1, seq)?;
        self.0.count_channel(text);
        Ok(Some(Channel::Value(vector)))
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> {
        self.uniform(Written::Bool(value), Unexpected::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
        self.uniform(Written::Number(value as f64), Unexpected::Signed(value))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
        self.uniform(Written::Number(value as f64), Unexpected::Unsigned(value))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
        self.uniform(Written::Number(value), Unexpected::Float(value))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        self.uniform(Written::Text(value), Unexpected::Str(value))
    }
}

/// `BedrockAnimationAdapter.deserializeKeyframe`'s one keyframe, at the time its key gave: three
/// components, or an object of `post`, `pre` and `lerp_mode` with one of the first two at least.
struct KeyframeSeed<'r, 'c>(&'r mut Reader<'c>, Transformation, f64);

impl<'de> Visitor<'de> for KeyframeSeed<'_, '_> {
    type Value = Keyframe;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a keyframe, `[x, y, z]` or an object")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> std::result::Result<Self::Value, A::Error> {
        let (post, _) = read_vector(self.0, self.1, seq)?;
        Ok(Keyframe {
            time: self.2,
            pre: None,
            post,
            smooth: false,
        })
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let KeyframeSeed(reader, transformation, time) = self;
        let (mut pre, mut post, mut smooth) = (None, None, false);
        while let Some(key) = map.next_key_seed(KeyAmong(&["pre", "post", "lerp_mode"]))? {
            match key {
                Key::Known(0) => {
                    pre = Some(map.next_value_seed(VectorSeed(&mut *reader, transformation))?);
                }
                Key::Known(1) => {
                    post = Some(map.next_value_seed(VectorSeed(&mut *reader, transformation))?);
                }
                Key::Known(_) => smooth = map.next_value::<Smooth>()?.0,
                Key::Other(_) => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        // Either stands for the other when the other is missing.
        let (pre, post) = match (pre, post) {
            (pre, Some(post)) => (pre, post),
            (Some(pre), None) => (None, pre),
            (None, None) => {
                return Err(de::Error::custom(
                    "a keyframe that is an object has neither `post` nor `pre`",
                ));
            }
        };
        Ok(Keyframe {
            time,
            pre: pre.filter(|pre| *pre != post).map(Box::new),
            post,
            smooth,
        })
    }
}

/// A keyframe's `pre` or `post`: an array, as Cobblemon's `asJsonArray` insists.
struct VectorSeed<'r, 'c>(&'r mut Reader<'c>, Transformation);

impl<'de> Visitor<'de> for VectorSeed<'_, '_> {
    type Value = Vector;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("`[x, y, z]`")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> std::result::Result<Self::Value, A::Error> {
        Ok(read_vector(self.0, self.1, seq)?.0)
    }
}

/// `BedrockAnimationAdapter.deserializeMolangBoneValue`: the first three of an array's elements, which it
/// fails without; any more are never looked at. Also answers whether any of the three was text.
fn read_vector<'de, A: SeqAccess<'de>>(
    reader: &mut Reader,
    transformation: Transformation,
    mut seq: A,
) -> std::result::Result<(Vector, bool), A::Error> {
    let mut next = |place| {
        seq.next_element_seed(ComponentSeed(&mut *reader, transformation))?
            .ok_or_else(|| de::Error::invalid_length(place, &"three components"))
    };
    let ((x, a), (y, b), (z, c)) = (next(0)?, next(1)?, next(2)?);
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    let numbers = (x.constant_exact(), y.constant_exact(), z.constant_exact());
    let vector = match numbers {
        (Some(x), Some(y), Some(z)) => Vector::Numbers([x, y, z]),
        _ => Vector::Expressions(Box::new([x, y, z])),
    };
    Ok((vector, a || b || c))
}

/// One component: a number or MoLang text (or a boolean, which Gson makes the text "true" or "false").
struct ComponentSeed<'r, 'c>(&'r mut Reader<'c>, Transformation);

impl Visitor<'_> for ComponentSeed<'_, '_> {
    type Value = (Expression, bool);
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a component, a number or MoLang text")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(self.0.component(self.1, Written::Bool(value)))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(self.0.component(self.1, Written::Number(value as f64)))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(self.0.component(self.1, Written::Number(value as f64)))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
        Ok(self.0.component(self.1, Written::Number(value)))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(self.0.component(self.1, Written::Text(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with `animations` (the inside of that object) and no other key.
    fn file(animations: &str) -> AnimationFile {
        counted(animations).0
    }

    fn counted(animations: &str) -> (AnimationFile, Census) {
        let mut census = Census::default();
        let json = format!(r#"{{ "format_version": "1.8.0", "animations": {{ {animations} }} }}"#);
        let file = AnimationFile::parse_counting(&json, &mut census)
            .unwrap_or_else(|e| panic!("{json}: {}", e.0));
        (file, census)
    }

    /// The one animation that moves `bone` this way, named `animation.test`.
    fn one(bone: &str) -> Animation {
        with(bone, "")
    }

    /// [`one`], with more of the animation's own keys.
    fn with(bone: &str, rest: &str) -> Animation {
        let animations = format!(r#""animation.test": {{ {rest} "bones": {{ "bone": {bone} }} }}"#);
        file(&animations).animations.remove(0)
    }

    fn context(anim_time: f32) -> Context {
        Context {
            anim_time,
            ..Context::default()
        }
    }

    /// What `animation` does at `seconds` with the state's seconds at 0: each bone's index and delta.
    fn sampled(animation: &Animation, seconds: f32) -> Vec<(usize, Delta)> {
        let mut deltas = Vec::new();
        animation.sample(seconds, &context(0.0), &mut |bone, delta| {
            deltas.push((bone, delta));
        });
        deltas
    }

    /// The first bone's delta at `seconds`.
    fn delta(animation: &Animation, seconds: f32) -> Delta {
        match sampled(animation, seconds)[..] {
            [(0, delta)] => delta,
            ref other => panic!("{other:?}"),
        }
    }

    /// A rotation's degrees as `BedrockAnimation.run` adds them to a part.
    fn radians(degrees: [f32; 3]) -> [f32; 3] {
        degrees.map(|degrees| degrees / 57.2958)
    }

    fn assert_close(found: [f32; 3], expected: [f32; 3]) {
        for axis in 0..3 {
            let (a, b) = (found[axis], expected[axis]);
            assert!(
                (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0),
                "{found:?} is not {expected:?}"
            );
        }
    }

    #[test]
    fn a_delta_is_in_part_space() {
        let animation =
            one(r#"{ "position": [1, 2, 3], "rotation": [90, -45, 180], "scale": [2, 0.5, 1] }"#);
        assert_eq!(animation.name, "animation.test");
        assert_eq!(animation.bones[0].bone, "bone");
        assert_eq!(
            delta(&animation, 0.0),
            Delta {
                // Bedrock's y points up, a part's down.
                position: [1.0, -2.0, 3.0],
                // Degrees over Cobblemon's 57.2958, each with its sign.
                rotation: [90.0 / 57.2958, -45.0 / 57.2958, 180.0 / 57.2958],
                scale: [2.0, 0.5, 1.0],
            }
        );
        // A channel that is not there changes nothing.
        assert_eq!(
            delta(&one(r#"{ "rotation": [0, 30, 0] }"#), 0.0),
            Delta {
                rotation: [0.0, 30.0 / 57.2958, 0.0],
                ..Delta::NONE
            }
        );
        assert_eq!(
            delta(&one(r#"{ "position": [-0.5, -8, 0.25] }"#), 0.0).position,
            [-0.5, 8.0, 0.25]
        );
    }

    #[test]
    fn a_scale_is_three_components_or_one_for_the_three() {
        for (scale, expected) in [
            ("[1.5, 2, 0.25]", [1.5, 2.0, 0.25]),
            ("3", [3.0; 3]),
            ("0.5", [0.5; 3]),
            (r#""1 + 1""#, [2.0; 3]),
            ("0", [0.0; 3]),
            ("[0, 0, 0]", [0.0; 3]),
            ("[0, 1, 2]", [0.0, 1.0, 2.0]),
            ("true", [1.0; 3]),
        ] {
            let animation = one(&format!(r#"{{ "scale": {scale} }}"#));
            assert_eq!(delta(&animation, 0.0).scale, expected, "{scale}");
        }
        // One text is read three times: three rolls.
        let animation = one(r#"{ "scale": "1 + math.random(0.05, 0.1)" }"#);
        let [x, y, z] = delta(&animation, 0.3).scale;
        for factor in [x, y, z] {
            assert!((1.05..1.1).contains(&factor), "{factor}");
        }
        assert!(x != y && y != z && x != z, "{x} {y} {z}");
        assert_eq!(delta(&animation, 0.3).scale, [x, y, z]);
        assert_ne!(delta(&animation, 0.35).scale, [x, y, z]);
    }

    #[test]
    fn molang_channels_read_the_state_and_the_animation_apart() {
        let animation = with(
            r#"{
                "rotation": ["q.anim_time * 10", "v.anim_time * 10", "math.sin(query.anim_time * 90) * 30"],
                "position": ["v.age_in_ticks", "variable.limb_swing + v.limb_swing_amount", "q.age_in_ticks + 1"]
            }"#,
            r#""loop": true, "animation_length": 2.0,"#,
        );
        let context = Context {
            anim_time: 7.0,
            age_in_ticks: 140.0,
            limb_swing: 3.0,
            limb_swing_amount: 0.5,
        };
        let mut deltas = Vec::new();
        // Half a second into the animation's second turn, seven seconds into the state's life.
        animation.sample(2.5, &context, &mut |bone, delta| deltas.push((bone, delta)));
        let [(0, delta)] = deltas[..] else {
            panic!("{deltas:?}")
        };
        assert_close(delta.rotation, radians([70.0, 5.0, -30.0]));
        assert_eq!(delta.position, [140.0, -3.5, 1.0]);
    }

    #[test]
    fn what_cobblemon_does_to_a_text_before_molang_reads_it() {
        assert_eq!(clean_expression("+5"), "5");
        assert_eq!(clean_expression("-(2+3)*4"), "-1*(2+3)*4");
        assert_eq!(clean_expression("2*+3*+q.anim_time"), "2*3*query.anim_time");
        assert_eq!(
            clean_expression("q.camera_rotation(0)+q.camera_rotation(1)"),
            "query.camera_rotation_x+query.camera_rotation_y"
        );
        assert_eq!(clean_expression("query.anim_time"), "query.anim_time");
        let animation = one(r#"{ "position": ["+5", "-(2+3)*4", "2*+3"] }"#);
        assert_eq!(delta(&animation, 0.0).position, [5.0, 20.0, 6.0]);
    }

    #[test]
    fn keyframes_are_joined_by_straight_lines() {
        let animation = one(r#"{
            "position": { "0.0": [0, 0, 0], "2.0": [4, 8, -2], "3.0": [4, 0, 10] },
            "rotation": { "1.0": [10, 20, 30] },
            "scale": { "0.5": [1, 1, 1], "1.5": [2, 3, 0.5] }
        }"#);
        let at = |seconds| delta(&animation, seconds);
        assert_eq!(at(0.0).position, [0.0, 0.0, 0.0]);
        assert_eq!(at(0.5).position, [1.0, -2.0, -0.5]);
        assert_eq!(at(1.0).position, [2.0, -4.0, -1.0]);
        assert_eq!(at(2.0).position, [4.0, -8.0, -2.0]);
        assert_eq!(at(2.5).position, [4.0, -4.0, 4.0]);
        // Before the first keyframe and from the last one on, that keyframe.
        assert_eq!(at(-1.0).position, [0.0, 0.0, 0.0]);
        assert_eq!(at(3.0).position, [4.0, 0.0, 10.0]);
        assert_eq!(at(50.0).position, [4.0, 0.0, 10.0]);
        // One keyframe is that keyframe always.
        for seconds in [0.0, 1.0, 9.0] {
            assert_close(at(seconds).rotation, radians([10.0, 20.0, 30.0]));
        }
        assert_eq!(at(0.0).scale, [1.0, 1.0, 1.0]);
        assert_close(at(1.0).scale, [1.5, 2.0, 0.75]);
        assert_eq!(at(1.5).scale, [2.0, 3.0, 0.5]);
        // Not a time at all: the last keyframe.
        assert_eq!(at(f32::NAN).position, [4.0, 0.0, 10.0]);
        assert_eq!(at(f32::INFINITY).position, [4.0, 0.0, 10.0]);
        assert_eq!(at(f32::NEG_INFINITY).position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_keyframe_arrives_at_its_pre_and_leaves_from_its_post() {
        let animation = one(r#"{
            "position": {
                "0.0": { "pre": [9, 9, 9], "post": [0, 0, 0] },
                "1.0": { "pre": [2, 0, 0], "post": [10, 0, 0] },
                "2.0": { "post": [20, 0, 0] },
                "3.0": { "pre": [30, 0, 0] }
            }
        }"#);
        let x = |seconds| delta(&animation, seconds).position[0];
        assert_eq!(x(-0.5), 9.0);
        assert_eq!(x(0.0), 0.0);
        assert_eq!(x(0.5), 1.0);
        // The keyframe itself is already left.
        assert_eq!(x(1.0), 10.0);
        assert_eq!(x(1.5), 15.0);
        assert_eq!(x(2.5), 25.0);
        assert_eq!(x(3.0), 30.0);
        // A step: both sides of one moment.
        let step = one(
            r#"{ "scale": { "0.0": [1, 1, 1], "1.0": { "pre": [1, 1, 1], "post": [3, 3, 3] } } }"#,
        );
        assert_eq!(delta(&step, 0.999).scale, [1.0; 3]);
        assert_eq!(delta(&step, 1.0).scale, [3.0; 3]);
    }

    /// The expected numbers are a uniform Catmull-Rom spline's in its textbook form,
    /// `0.5 * (2 p1 + (p2 - p0) t + (2 p0 - 5 p1 + 4 p2 - p3) t² + (3 p1 - p0 - 3 p2 + p3) t³)`, computed
    /// apart from this crate, with an end point twice where there is no keyframe beyond it.
    #[test]
    fn a_smooth_keyframe_makes_a_catmull_rom_spline_of_both_its_sides() {
        let animation = one(r#"{
            "rotation": {
                "0.0": [0, 0, 0],
                "1.0": { "post": [10, 20, -30], "lerp_mode": "catmullrom" },
                "2.0": { "post": [40, 0, 10], "lerp_mode": "catmullrom" },
                "4.0": [0, 80, 20]
            }
        }"#);
        for (seconds, expected) in [
            // Towards a smooth keyframe, with nothing before: the first point twice.
            (0.5, [3.125, 11.25, -17.5]),
            // All four.
            (1.25, [17.734_375, 15.468_75, -24.218_75]),
            (1.5, [28.125, 6.25, -12.5]),
            // From a smooth keyframe, with nothing after: the last point twice. The points count as
            // evenly spaced though these two are two seconds apart.
            (2.5, [33.984_375, 14.843_75, 14.843_75]),
            (3.0, [21.875, 38.75, 17.5]),
            (3.5, [8.828_125, 63.281_25, 18.906_25]),
            // On the keyframes themselves, and outside them.
            (0.0, [0.0, 0.0, 0.0]),
            (1.0, [10.0, 20.0, -30.0]),
            (2.0, [40.0, 0.0, 10.0]),
            (4.0, [0.0, 80.0, 20.0]),
            (-1.0, [0.0, 0.0, 0.0]),
            (9.0, [0.0, 80.0, 20.0]),
        ] {
            assert_close(delta(&animation, seconds).rotation, radians(expected));
        }
        // Two keyframes alone: both twice.
        let two = one(
            r#"{ "position": { "0": [2, 0, 0], "4": { "post": [10, 0, 0], "lerp_mode": "catmullrom" } } }"#,
        );
        assert_close(delta(&two, 1.0).position, [3.625, 0.0, 0.0]);
        assert_close(delta(&two, 2.0).position, [6.0, 0.0, 0.0]);
        // The spline goes through what each keyframe shows the side it is seen from: the `post` of
        // those before the moment, the `pre` of those after it.
        let sides = one(r#"{
            "position": {
                "0.0": { "pre": [5, 0, 0], "post": [1, 0, 0] },
                "1.0": { "pre": [2, 0, 0], "post": [20, 0, 0], "lerp_mode": "catmullrom" },
                "2.0": { "pre": [4, 0, 0], "post": [40, 0, 0] },
                "3.0": { "pre": [8, 0, 0], "post": [80, 0, 0] }
            }
        }"#);
        let x = |seconds| delta(&sides, seconds).position[0];
        assert_close([x(0.5), x(0.75), x(1.5)], [1.375, 1.656_25, 12.9375]);
        assert_eq!(x(1.0), 20.0);
        // Neither of these two is smooth: a straight line from 40 to 8.
        assert_eq!(x(2.5), 24.0);
        assert_eq!([x(-1.0), x(3.0)], [5.0, 80.0]);
        // A position's y is turned over whichever way it is interpolated.
        let up = one(
            r#"{ "position": { "0": [0, 2, 0], "4": { "post": [0, 10, 0], "lerp_mode": "catmullrom" } } }"#,
        );
        assert_close(delta(&up, 1.0).position, [0.0, -3.625, 0.0]);
    }

    #[test]
    fn keyframes_may_hold_molang_and_come_in_any_order() {
        let animation = one(r#"{
            "rotation": {
                "2": ["q.anim_time * 100", 0, 0],
                "0.0": ["v.anim_time * 10", 10, 0],
                "1": [50, 20, 0],
                "1.0": [60, 30, 0],
                "2.0": ["q.anim_time * 10", 40, 0]
            }
        }"#);
        let at = |seconds, state| {
            let mut found = None;
            animation.sample(seconds, &context(state), &mut |_, delta| {
                found = Some(delta)
            });
            found.unwrap().rotation
        };
        // "1" and "1.0" are one time, as are "2" and "2.0": the later one in the file stands.
        assert_close(at(1.0, 0.0), radians([60.0, 30.0, 0.0]));
        assert_close(at(0.5, 0.0), radians([32.5, 20.0, 0.0]));
        assert_close(at(1.5, 3.0), radians([45.0, 35.0, 0.0]));
        assert_close(at(5.0, 3.0), radians([30.0, 40.0, 0.0]));
    }

    #[test]
    fn time_wraps_only_with_loop_and_a_length_and_ends_only_with_a_length() {
        let bone = r#"{ "position": { "0.0": [0, 0, 0], "1.0": [10, 0, 0] } }"#;
        let x = |animation: &Animation, seconds| match sampled(animation, seconds)[..] {
            [(0, delta)] => Some(delta.position[0]),
            [] => None,
            ref other => panic!("{other:?}"),
        };
        let looping = with(bone, r#""loop": true, "animation_length": 2.0,"#);
        assert!(looping.looping && looping.wraps());
        assert_eq!(looping.length, Some(2.0));
        assert_eq!(x(&looping, 0.5), Some(5.0));
        assert_eq!(x(&looping, 1.5), Some(10.0));
        assert_eq!(x(&looping, 2.0), Some(0.0));
        assert_eq!(x(&looping, 2.5), Some(5.0));
        assert_eq!(x(&looping, 40.5), Some(5.0));
        assert!(!looping.is_over(1e9));

        // Not looping, with a length: it holds up to its length and is nothing past it.
        for rest in [
            r#""animation_length": 2.0,"#,
            r#""loop": false, "animation_length": 2.0,"#,
            r#""loop": "hold_on_last_frame", "animation_length": 2.0,"#,
        ] {
            let once = with(bone, rest);
            assert!(!once.looping && !once.wraps(), "{rest}");
            assert_eq!(x(&once, 0.5), Some(5.0));
            assert_eq!(x(&once, 1.5), Some(10.0));
            assert_eq!(x(&once, 2.0), Some(10.0));
            assert_eq!(x(&once, 2.001), None);
            assert_eq!(x(&once, 40.5), None);
            assert!(!once.is_over(2.0) && once.is_over(2.5));
        }

        // Without a length, `loop` or not: it never wraps and never ends.
        for rest in [
            "",
            r#""loop": true,"#,
            r#""loop": true, "animation_length": 0,"#,
        ] {
            let endless = with(bone, rest);
            assert!(!endless.wraps(), "{rest}");
            assert_eq!(x(&endless, 0.5), Some(5.0));
            assert_eq!(x(&endless, 2.5), Some(10.0));
            assert_eq!(x(&endless, 4000.0), Some(10.0));
            assert!(!endless.is_over(4000.0));
        }
        assert!(with(bone, r#""loop": true,"#).looping);
        assert_eq!(with(bone, "").length, None);
        // Gson's reading of `loop`.
        assert!(with(bone, r#""loop": "TRUE", "animation_length": 1,"#).wraps());
        assert!(!with(bone, r#""loop": 1, "animation_length": 1,"#).looping);
        // A negative time wraps towards zero, as Java's `%` does.
        assert_eq!(x(&looping, -2.5), Some(0.0));
    }

    #[test]
    fn each_bone_that_moves_is_told_once_under_its_index() {
        let file = file(
            r#""animation.a": { "bones": {
                "still": {},
                "body": { "rotation": [0, 90, 0] },
                "ignored": { "relative_to": { "rotation": "entity" } },
                "emptied": { "position": {} },
                "tail": { "scale": 2, "position": ["q.anim_time", 0, 0] }
            } },
            "animation.b": { "loop": true },
            "animation.c": { "bones": {} }"#,
        );
        let names: Vec<_> = file.animations.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["animation.a", "animation.b", "animation.c"]);
        let animation = file.get("animation.a").unwrap();
        let bones: Vec<_> = animation.bones.iter().map(|b| b.bone.as_str()).collect();
        assert_eq!(bones, ["still", "body", "ignored", "emptied", "tail"]);
        let mut deltas = Vec::new();
        animation.sample(0.0, &context(4.0), &mut |bone, delta| {
            deltas.push((bone, delta));
        });
        assert_eq!(
            deltas,
            [
                (
                    1,
                    Delta {
                        rotation: [0.0, 90.0 / 57.2958, 0.0],
                        ..Delta::NONE
                    }
                ),
                (
                    4,
                    Delta {
                        position: [4.0, 0.0, 0.0],
                        scale: [2.0; 3],
                        ..Delta::NONE
                    }
                ),
            ]
        );
        assert!(sampled(file.get("animation.b").unwrap(), 0.0).is_empty());
        assert!(sampled(file.get("animation.c").unwrap(), 0.0).is_empty());
        assert!(file.get("animation.d").is_none());
        assert!(file.get("animation.b").unwrap().looping);
    }

    #[test]
    fn a_component_that_does_not_compile_is_counted_and_left_at_rest() {
        let (file, census) = counted(
            r#""animation.broken": { "bones": {
                "arm": {
                    "rotation": ["v.x = 1", 5, "math.sin("],
                    "scale": ["'big'", 2, "q.anim_time +"],
                    "position": { "0.0": ["", 1, 2], "1.0": { "post": [0, "return 3", 4] } }
                },
                "leg": { "scale": "loop(2, 1)" }
            } },
            "animation.fine": { "bones": { "arm": { "rotation": [1, "2", "q.anim_time"] } } }"#,
        );
        let broken = file.get("animation.broken").unwrap();
        let deltas = sampled(broken, 0.5);
        assert_eq!(deltas.len(), 2);
        assert_close(deltas[0].1.rotation, radians([0.0, 5.0, 0.0]));
        assert_eq!(deltas[0].1.scale, [1.0, 2.0, 1.0]);
        assert_eq!(deltas[0].1.position, [0.0, -0.5, 3.0]);
        assert_eq!(deltas[1].1.scale, [1.0; 3]);
        assert_eq!(sampled(file.get("animation.fine").unwrap(), 0.0).len(), 1);
        let told: Vec<_> = census
            .unparsed
            .iter()
            .map(|unparsed| {
                assert_eq!(unparsed.animation, "animation.broken");
                assert!(unparsed.error.starts_with("molang: "), "{}", unparsed.error);
                (unparsed.bone.as_str(), unparsed.source.as_str())
            })
            .collect();
        assert_eq!(
            told,
            [
                ("arm", "v.x = 1"),
                ("arm", "math.sin("),
                ("arm", "'big'"),
                ("arm", "q.anim_time +"),
                ("arm", ""),
                ("arm", "return 3"),
                ("leg", "loop(2, 1)"),
            ]
        );
        // Without a census to tell, the file reads the same.
        let json = r#"{ "animations": { "a": { "bones": { "b": { "scale": "this" } } } } }"#;
        let read = AnimationFile::parse(json).unwrap();
        assert_eq!(delta(&read.animations[0], 0.0).scale, [1.0; 3]);
    }

    #[test]
    fn a_census_counts_what_a_file_is_made_of() {
        let (_, census) = counted(
            r#""animation.a": {
                "loop": true,
                "animation_length": 1.5,
                "sound_effects": { "0.0": { "effect": "cry" } },
                "timeline": { "0.5": "q.has_entity ? { q.sound('flap'); };" },
                "bones": {
                    "body": {
                        "rotation": [0, 1.5, -2],
                        "position": ["math.sin(Q.Anim_Time * 90)", 0, "2 * 3"],
                        "scale": 1.25,
                        "relative_to": {}
                    },
                    "head": {
                        "rotation": {
                            "0.0": [0, 0, 0],
                            "1.0": {
                                "pre": [1, 2, "v.speed"],
                                "post": [0, "math.cos(q.r.dive(0))", 0],
                                "lerp_mode": "catmullrom"
                            }
                        },
                        "position": {},
                        "scale": "1 + math.random(0, q.anim_time)"
                    }
                }
            },
            "animation.b": { "particle_effects": {}, "sound_effects": {} }"#,
        );
        assert_eq!(census.number_channels, 2);
        assert_eq!(census.molang_channels, 2);
        assert_eq!(census.keyframed_channels, 1);
        assert_eq!(census.keyframes, 2);
        assert_eq!(census.numbers, 3 + 1 + 1 + 3 + 2 + 2);
        assert_eq!(census.expressions, 2 + 2 + 1);
        // `2 * 3`, `v.speed` and the cosine of a query that is 0 here.
        assert_eq!(census.constant_expressions, 3);
        let names: Vec<_> = census
            .names
            .iter()
            .map(|(name, count)| (name.as_str(), *count))
            .collect();
        assert_eq!(
            names,
            [
                ("math.cos", 1),
                ("math.random", 1),
                ("math.sin", 1),
                ("query.anim_time", 2),
                ("query.r.dive", 1),
                ("variable.speed", 1),
            ]
        );
        let keys = |counts: &BTreeMap<String, usize>| {
            counts
                .iter()
                .map(|(key, count)| format!("{key} {count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert_eq!(
            keys(&census.ignored_animation_keys),
            "particle_effects 1, timeline 1"
        );
        assert_eq!(census.sounds, 1);
        assert_eq!(keys(&census.ignored_bone_keys), "relative_to 1");
        assert!(census.unparsed.is_empty());
    }

    #[test]
    fn an_animation_s_sounds_are_read_with_their_moments() {
        let cry = file(
            r#""animation.pikachu.cry": {
                "animation_length": 4,
                "sound_effects": {
                    "1.25": [{ "effect": "Cobblemon:Pokemon.Pikachu.Extra" }, { "effect": "mod:beep" }],
                    "0.5": { "effect": "pokemon.pikachu.cry" },
                    "2.0": "nothing Cobblemon reads"
                }
            },
            "animation.pikachu.idle": { "sound_effects": {} }"#,
        );
        let sounds: Vec<_> = cry.animations[0]
            .sounds
            .iter()
            .map(|s| (s.seconds, s.sound.as_str()))
            .collect();
        // By moment; a name without a namespace is Cobblemon's, and any is in lower case.
        assert_eq!(
            sounds,
            [
                (0.5, "cobblemon:pokemon.pikachu.cry"),
                (1.25, "cobblemon:pokemon.pikachu.extra"),
                (1.25, "mod:beep"),
            ]
        );
        assert!(cry.animations[1].sounds.is_empty());
        // What Cobblemon fails on fails here: no object of moments, an effect that names nothing.
        for bad in [
            r#""sound_effects": []"#,
            r#""sound_effects": { "0.0": {} }"#,
            r#""sound_effects": { "0.0": ["cry"] }"#,
            r#""sound_effects": { "soon": { "effect": "cry" } }"#,
        ] {
            let json = format!(r#"{{ "animations": {{ "a": {{ {bad} }} }} }}"#);
            assert!(AnimationFile::parse(&json).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_gson_and_cobblemon_let_pass_reads() {
        // A byte order mark, keys nobody reads, more than three components.
        let json = "\u{feff}{ \"geckolib_format_version\": 2, \"animations\": { \"a\": { \
                    \"anim_time_update\": \"q.anim_time\", \"bones\": { \"b\": { \"position\": \
                    [1, 2, 3, 4, [5]] } } } } }";
        let read = AnimationFile::parse(json).unwrap();
        assert_eq!(delta(&read.animations[0], 0.0).position, [1.0, -2.0, 3.0]);
        // No animation at all.
        let none = AnimationFile::parse(r#"{ "animations": {} }"#).unwrap();
        assert!(none.animations.is_empty());
        // A key twice in an object: the last one. Bones of one name are one bone, where the first stood.
        let animation = file(
            r#""a": { "loop": true, "loop": false, "bones": {
                "arm": { "rotation": [1, 0, 0], "position": [9, 9, 9] },
                "leg": { "scale": 2, "scale": 3 },
                "arm": { "rotation": [0, 2, 0], "rotation": [0, 0, 3] },
                "tail": { "position": {
                    "0": [1, 0, 0],
                    "0": [2, 0, 0],
                    "0.0": { "pre": [0, 0, 0], "pre": [3, 0, 0] }
                } },
                "arm": { "scale": [4, 4, 4] }
            } }"#,
        )
        .animations
        .remove(0);
        assert!(!animation.looping);
        let bones: Vec<_> = animation.bones.iter().map(|b| b.bone.as_str()).collect();
        assert_eq!(bones, ["arm", "leg", "tail"]);
        let scaled = |scale| Delta {
            scale: [scale; 3],
            ..Delta::NONE
        };
        let moved = Delta {
            position: [3.0, 0.0, 0.0],
            ..Delta::NONE
        };
        assert_eq!(
            sampled(&animation, 0.0),
            [(0, scaled(4.0)), (1, scaled(3.0)), (2, moved)]
        );
    }

    #[test]
    fn a_file_cobblemon_fails_on_is_an_error_and_never_a_panic() {
        let animation = |inside: &str| format!(r#"{{ "animations": {{ "a": {inside} }} }}"#);
        let bone = |inside: &str| animation(&format!(r#"{{ "bones": {{ "b": {inside} }} }}"#));
        let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
        let cases = [
            String::new(),
            "null".to_owned(),
            "[]".to_owned(),
            "{".to_owned(),
            "{}".to_owned(),
            r#"{ "format_version": "1.8.0" }"#.to_owned(),
            r#"{ "animations": null }"#.to_owned(),
            r#"{ "animations": [] }"#.to_owned(),
            r#"{ "animations": {} } trailing"#.to_owned(),
            r#"{ "animations": { "a": {}, "b": {}, "a": {} } }"#.to_owned(),
            // "animation json could not be parsed".
            animation("null"),
            animation("[]"),
            animation("3"),
            animation(r#"{ "animation_length": "1.5" }"#),
            animation(r#"{ "animation_length": null }"#),
            animation(r#"{ "loop": null }"#),
            animation(r#"{ "loop": [] }"#),
            animation(r#"{ "bones": null }"#),
            animation(r#"{ "bones": [] }"#),
            animation(r#"{ "bones": { "b": null } }"#),
            animation(r#"{ "bones": { "b": [] } }"#),
            animation(&format!(r#"{{ "bones": {deep} }}"#)),
            // Neither keyframes nor an array, where only a scale may be one component.
            bone(r#"{ "rotation": 0 }"#),
            bone(r#"{ "rotation": "q.anim_time" }"#),
            bone(r#"{ "position": 1.5 }"#),
            bone(r#"{ "position": true }"#),
            bone(r#"{ "position": null }"#),
            bone(r#"{ "scale": null }"#),
            // `array[2]` of a shorter array.
            bone(r#"{ "rotation": [] }"#),
            bone(r#"{ "rotation": [1, 2] }"#),
            bone(r#"{ "scale": [1] }"#),
            // Not something `getAsString` reads.
            bone(r#"{ "rotation": [1, null, 3] }"#),
            bone(r#"{ "rotation": [1, {}, 3] }"#),
            bone(r#"{ "rotation": [1, [2], 3] }"#),
            // "keyframe json could not be parsed".
            bone(r#"{ "rotation": { "0.0": 1 } }"#),
            bone(r#"{ "rotation": { "0.0": "q.anim_time" } }"#),
            bone(r#"{ "rotation": { "0.0": null } }"#),
            bone(r#"{ "rotation": { "0.0": [1, 2] } }"#),
            // "transformation data ('post') could not be found".
            bone(r#"{ "rotation": { "0.0": {} } }"#),
            bone(r#"{ "rotation": { "0.0": { "lerp_mode": "catmullrom" } } }"#),
            bone(r#"{ "rotation": { "0.0": { "post": { "vector": [1, 2, 3] } } } }"#),
            bone(r#"{ "rotation": { "0.0": { "post": 1 } } }"#),
            bone(r#"{ "rotation": { "0.0": { "post": [1, 2, 3], "pre": null } } }"#),
            bone(r#"{ "rotation": { "0.0": { "post": [1, 2, 3], "lerp_mode": null } } }"#),
            bone(r#"{ "rotation": { "0.0": { "post": [1, 2, 3], "lerp_mode": 1 } } }"#),
            // Not a time.
            bone(r#"{ "rotation": { "soon": [1, 2, 3] } }"#),
            bone(r#"{ "rotation": { "": [1, 2, 3] } }"#),
            bone(r#"{ "rotation": { "NaN": [1, 2, 3] } }"#),
            bone(r#"{ "rotation": { "inf": [1, 2, 3] } }"#),
            bone(r#"{ "rotation": { " 1": [1, 2, 3] } }"#),
        ];
        for json in &cases {
            let shown = &json[..json.len().min(200)];
            match AnimationFile::parse(json) {
                Ok(_) => panic!("{shown} reads"),
                Err(error) => assert!(error.0.starts_with("animation: "), "{shown}: {}", error.0),
            }
        }
        // What is skipped is skipped however deep it goes.
        assert!(AnimationFile::parse(&animation(&format!(r#"{{ "skipped": {deep} }}"#))).is_ok());
        // Cut anywhere, a file is an error and nothing worse.
        let whole = bone(
            r#"{ "rotation": { "0.0": [1, "q.anim_time", 3], "1": { "pre": [1, 2, 3], "post": [4, 5, 6], "lerp_mode": "catmullrom" } }, "scale": "2" }"#,
        );
        assert!(AnimationFile::parse(&whole).is_ok());
        for end in 0..whole.len() {
            assert!(AnimationFile::parse(&whole[..end]).is_err(), "{end}");
        }
    }

    #[test]
    fn a_scale_goes_by_its_deviation_from_one_as_in_cobblemon() {
        // Exactly zero is left alone; anything else goes there and back, which is the same number for
        // most and one bit off for some.
        assert_eq!(scale_at_full_intensity([0.0; 3]), [0.0; 3]);
        assert_eq!(scale_at_full_intensity([1.0, 2.0, 0.5]), [1.0, 2.0, 0.5]);
        assert_eq!(
            scale_at_full_intensity([1.2, -3.0, 16.25]),
            [1.2, -3.0, 16.25]
        );
        assert_eq!(
            scale_at_full_intensity([0.1; 3]),
            [0.099_999_999_999_999_98; 3]
        );
        assert_eq!(
            scale_at_full_intensity([0.1; 3]).map(|scale| scale as f32),
            [0.1; 3]
        );
        let zeroes = scale_at_full_intensity([0.0, 1.0, 0.0]);
        assert_eq!(zeroes, [0.0, 1.0, 0.0]);
        assert!(zeroes[0].is_sign_negative());
    }
}
