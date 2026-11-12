// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon's particle effects, which its sources call "Snowstorm" after the editor they are made in:
//! Bedrock's particle format (`assets/cobblemon/bedrock/particles/**/*.particle.json`), as
//! `SnowstormParticleReader.loadEffect` of Cobblemon 1.8.1 reads it. [`crate::storm`] runs them.
//!
//! What is read is what Cobblemon reads, with its defaults and its oddities: a component it has no reader
//! for is no more here than there (`minecraft:particle_kill_plane`), and a flipbook without `max_frame`
//! counts its frames from -1 as it does there.
//!
//! An effect's expressions are MoLang with variables ([`Expression::parse_scoped`]): each name an effect
//! reads or assigns gets a slot, the ones Cobblemon's own code sets first ([`slot`]), then the effect's
//! own (`variable.size`). An effect keeps in [`Effect::unhandled`] what it asks for that is not done
//! here, for whoever plays it to say so: the game's functions it calls (`query.sound`), what the MoLang
//! of this crate leaves out, and what the simulation does not do yet (collisions with blocks).
use crate::molang::Expression;
use horizon_protocol::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The slots of the variables and functions Cobblemon's code gives an effect's runtime
/// (`ParticleStorm`, `SnowstormParticle`, the emitter's lifetime and rate), in [`FIXED`]'s order.
pub mod slot {
    pub const EMITTER_AGE: usize = 0;
    pub const EMITTER_LIFETIME: usize = 1;
    /// `emitter_random_1` to `_4`.
    pub const EMITTER_RANDOM: usize = 2;
    pub const PARTICLE_AGE: usize = 6;
    pub const PARTICLE_LIFETIME: usize = 7;
    /// `particle_random_1` to `_4`.
    pub const PARTICLE_RANDOM: usize = 8;
    /// `variable.entity_width`, `_height`, `_size`, `_radius`, `_scale`: what the emitter's entity
    /// measured when the effect started.
    pub const ENTITY: usize = 12;
    /// `variable.target_deltax`, `_deltay`, `_deltaz`, `target_distance`: from the emitter to what the
    /// effect is aimed at, in the emitter's space, as it was when the emitter or the particle started.
    pub const TARGET: usize = 17;
    /// `query.entity_width` and the four after it.
    pub const QUERY_ENTITY: usize = 21;
    /// `query.target_deltax` and the three after it, which follow the emitter.
    pub const QUERY_TARGET: usize = 26;
    /// The first slot of an effect's own variables.
    pub const OWN: usize = 30;
}

/// The names of the first [`slot::OWN`] slots.
pub const FIXED: [&str; slot::OWN] = [
    "variable.emitter_age",
    "variable.emitter_lifetime",
    "variable.emitter_random_1",
    "variable.emitter_random_2",
    "variable.emitter_random_3",
    "variable.emitter_random_4",
    "variable.particle_age",
    "variable.particle_lifetime",
    "variable.particle_random_1",
    "variable.particle_random_2",
    "variable.particle_random_3",
    "variable.particle_random_4",
    "variable.entity_width",
    "variable.entity_height",
    "variable.entity_size",
    "variable.entity_radius",
    "variable.entity_scale",
    "variable.target_deltax",
    "variable.target_deltay",
    "variable.target_deltaz",
    "variable.target_distance",
    "query.entity_width",
    "query.entity_height",
    "query.entity_size",
    "query.entity_radius",
    "query.entity_scale",
    "query.target_deltax",
    "query.target_deltay",
    "query.target_deltaz",
    "query.target_distance",
];

/// Three expressions: a point or a direction.
pub type Triple = [Expression; 3];

/// `ParticleMaterial`: how a particle's quad is blended. `Alpha` and `Blend` blend alike in Cobblemon
/// (`ParticleMaterials`), and differ by which faces are culled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Alpha,
    Opaque,
    Blend,
    Add,
}

/// One of the `;`-separated expressions of a `creation_expression` or `per_update_expression`.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    /// The slot `variable.name = ...` assigns. Without it the expression is only evaluated, for the
    /// random numbers it draws.
    pub assign: Option<u16>,
    pub value: Expression,
}

/// `ParticleEmitterRate`.
#[derive(Clone, Debug, PartialEq)]
pub enum Rate {
    /// `minecraft:emitter_rate_instant`: all at once, when the emitter starts.
    Instant { amount: Expression },
    /// `minecraft:emitter_rate_steady`: so many a second, while fewer than `maximum` are alive.
    Steady {
        rate: Expression,
        maximum: Expression,
    },
}

/// `ParticleEmitterLifetime`.
#[derive(Clone, Debug, PartialEq)]
pub enum Lifetime {
    Once {
        active: Expression,
    },
    Looping {
        active: Expression,
        sleep: Expression,
    },
    Expression {
        activation: Expression,
        expiration: Expression,
    },
}

/// `ParticleEmitterShape`: where an emitter's particles appear.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Point {
        offset: Triple,
    },
    Sphere {
        offset: Triple,
        radius: Expression,
        surface_only: bool,
    },
    Disc {
        offset: Triple,
        radius: Expression,
        normal: Triple,
        surface_only: bool,
    },
    Box {
        offset: Triple,
        half: Triple,
        surface_only: bool,
    },
    /// `minecraft:emitter_shape_entity_aabb`: the emitter's entity's box.
    EntityBox {
        surface_only: bool,
    },
}

/// `AttachedType`: what of the locator an emitter is attached to it keeps following.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub position: bool,
    pub rotation: bool,
    pub scale: bool,
}

/// `ParticleMotionDirection`: which way a particle leaves.
#[derive(Clone, Debug, PartialEq)]
pub enum Direction {
    Outwards,
    Inwards,
    Custom(Triple),
}

/// `ParticleMotion`.
#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    /// `minecraft:particle_motion_dynamic`: a speed, then an acceleration and a drag.
    Dynamic {
        direction: Direction,
        speed: Expression,
        acceleration: Triple,
        drag: Expression,
    },
    /// `minecraft:particle_motion_parametric`: where the particle is, in the emitter's space.
    Parametric {
        offset: Triple,
        direction: Triple,
    },
    Static,
}

/// `ParticleRotation`: a particle's turn about the axis it is looked at along, in degrees.
#[derive(Clone, Debug, PartialEq)]
pub enum Rotation {
    Parametric(Expression),
    Dynamic {
        start: Expression,
        speed: Expression,
        acceleration: Expression,
        drag: Expression,
    },
}

/// `ParticleUVMode`: the part of the effect's texture a particle shows, in texels of a texture
/// `texture` wide and high.
#[derive(Clone, Debug, PartialEq)]
pub struct Uv {
    pub texture: [i32; 2],
    pub start: [Expression; 2],
    pub size: [Expression; 2],
    pub flipbook: Option<Flipbook>,
}

/// `AnimatedParticleUVMode`: the frames after the first, each a `step` further.
#[derive(Clone, Debug, PartialEq)]
pub struct Flipbook {
    pub step: [Expression; 2],
    pub max_frame: Expression,
    pub fps: Expression,
    pub stretch_to_lifetime: bool,
    pub looping: bool,
}

/// `ParticleTinting`.
#[derive(Clone, Debug, PartialEq)]
pub enum Tint {
    /// Red, green, blue, alpha.
    Expression([Expression; 4]),
    Gradient {
        interpolant: Expression,
        /// Where each colour is, and its red, green, blue and alpha; in the file's order, one per key.
        stops: Vec<(f64, [f32; 4])>,
    },
}

/// `ParticleCameraMode`: how a particle's quad is turned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    RotateXyz,
    RotateY,
    LookAtXyz,
    LookAtY,
    LookAtDirection,
    DirectionX,
    DirectionY,
    DirectionZ,
    EmitterXy,
    EmitterXz,
    EmitterYz,
}

/// `ParticleViewDirection`: the direction the `Direction*` and `LookAtDirection` facings follow.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewDirection {
    /// The way the particle moves, while it is faster than this (blocks a second).
    FromMotion {
        min_speed: f64,
    },
    Custom(Triple),
}

/// `ParticleCollision`. Read, and not simulated yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Collision {
    pub enabled: Expression,
    pub radius: Expression,
    pub friction: Expression,
    pub bounciness: Expression,
    pub expires_on_contact: bool,
}

/// `MoLangCurve`: a variable computed from another value, each time the effect's expressions are
/// about to be evaluated.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    /// The slot of the variable the curve sets.
    pub slot: u16,
    pub input: Expression,
    pub kind: CurveKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CurveKind {
    Linear {
        range: Expression,
        nodes: Vec<f64>,
    },
    CatmullRom {
        range: Expression,
        nodes: Vec<f64>,
    },
    Bezier {
        range: Expression,
        nodes: [f64; 4],
    },
    /// Its pieces: where each starts and ends, and its four control values.
    BezierChain(Vec<(f64, f64, [f64; 4])>),
}

/// `EventParticleOptions.EventParticleType`: where the effect an event starts is, and what it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Emitter,
    EmitterBound,
    Particle,
    ParticleWithVelocity,
}

/// `ParticleEvent`: what happens when an emitter or a particle triggers it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Event {
    /// The effect to start, by its identifier, and how.
    pub particle: Option<(String, EventKind)>,
    /// The sound to play where the event happens.
    pub sound: Option<String>,
    /// The event's own expression, run in the emitter's runtime.
    pub expression: Vec<Statement>,
}

/// `BedrockParticleEmitter`.
#[derive(Clone, Debug, PartialEq)]
pub struct Emitter {
    pub start: Vec<Statement>,
    pub update: Vec<Statement>,
    pub rate: Rate,
    pub lifetime: Lifetime,
    pub shape: Shape,
    pub attachment: Attachment,
    /// `cobblemon:emitter_space`'s `scaling` is `entity`: the emitter takes its locator's scale.
    pub entity_scaling: bool,
    pub creation_events: Vec<String>,
    pub expiration_events: Vec<String>,
    /// The events of each time, in seconds of the emitter's age.
    pub timeline: Vec<(f64, Vec<String>)>,
    pub travel_distance_events: Vec<(f64, Vec<String>)>,
    pub looping_travel_distance_events: Vec<(f64, Vec<String>)>,
}

/// `BedrockParticle`: what each particle of an effect is.
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    pub motion: Motion,
    pub rotation: Rotation,
    pub uv: Uv,
    pub size: [Expression; 2],
    /// `max_lifetime`, in seconds.
    pub max_age: Expression,
    /// `expiration_expression`: the particle goes as soon as this is not 0.
    pub kill: Expression,
    pub update: Vec<Statement>,
    pub render: Vec<Statement>,
    pub view_direction: ViewDirection,
    pub facing: Facing,
    pub tint: Tint,
    pub collision: Collision,
    /// `minecraft:particle_appearance_lighting`: lit by the world where it is, and not fully bright.
    pub environment_lighting: bool,
    pub creation_events: Vec<String>,
    pub expiration_events: Vec<String>,
    pub timeline: Vec<(f64, Vec<String>)>,
}

/// `ParticleSpace` (`minecraft:emitter_local_space`): what of a particle stays in its emitter's space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Space {
    pub local_position: bool,
    pub local_rotation: bool,
    pub local_velocity: bool,
}

/// `BedrockParticleOptions`: one effect.
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    /// `description.identifier`, which names the effect everywhere (`cobblemon:pokeball/casual/sendflash`).
    pub id: String,
    /// The sprite of the particle atlas the effect draws from, as Cobblemon derives it from the file's
    /// texture: `cobblemon:balls/pokeball/casual/sendcasual`, which is the file
    /// `assets/cobblemon/textures/particle/balls/pokeball/casual/sendcasual.png` ([`Effect::texture_path`]).
    pub texture: String,
    pub material: Material,
    pub curves: Vec<Curve>,
    pub events: HashMap<String, Event>,
    pub emitter: Emitter,
    pub particle: Particle,
    pub space: Space,
    /// The name of each slot of the effect's runtime, [`FIXED`]'s first.
    pub variables: Vec<String>,
    /// What the effect asks for that is not done: for the log of whoever plays it.
    pub unhandled: Vec<String>,
}

fn bad(message: impl std::fmt::Display) -> Error {
    Error::new(format!("particle effect: {message}"))
}

/// Gson's `JsonElement.getAsString` for what the reader calls it on: a string as it is, a number or a
/// boolean as it is written. A number is never written with an exponent here, which MoLang would read
/// as its mantissa alone.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => {
            let written = number.to_string();
            if !written.contains(['e', 'E']) {
                return Some(written);
            }
            let plain = format!("{:.17}", number.as_f64()?);
            Some(plain.trim_end_matches('0').trim_end_matches('.').to_owned())
        }
        // A one-element array is its element to Gson.
        Value::Array(list) if list.len() == 1 => text(&list[0]),
        _ => None,
    }
}

/// Gson's `getAsDouble`: a number, or a string that is one.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// `JsonElement.normalizeToArray` of event names.
fn names(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(list)) => list.iter().filter_map(text).collect(),
        Some(one) => text(one).into_iter().collect(),
        None => Vec::new(),
    }
}

/// A timeline: `{"0.70": "event"}` or `{"0.70": ["a", "b"]}`, in the order of its times.
fn timeline(value: Option<&Value>) -> Result<Vec<(f64, Vec<String>)>> {
    let Some(map) = value.and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    // One entry a time, the last of those that are the same number.
    let mut by_time: Vec<(f64, Vec<String>)> = Vec::new();
    for (key, events) in map {
        let time: f64 = key
            .trim()
            .parse()
            .map_err(|_| bad(format_args!("`{key}` is not a time")))?;
        by_time.retain(|(at, _)| *at != time);
        by_time.push((time, names(Some(events))));
    }
    by_time.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(by_time)
}

/// `SnowstormParticleReader.parseHex`: `#AARRGGBB` as red, green, blue, alpha.
fn hex(written: &str) -> Result<[f32; 4]> {
    let cleaned = written.replace('#', "");
    let part = |at: usize| -> Result<f32> {
        cleaned
            .get(at..at + 2)
            .and_then(|digits| u8::from_str_radix(digits, 16).ok())
            .map(|value| f32::from(value) / 255.0)
            .ok_or_else(|| bad(format_args!("`{written}` is not a colour")))
    };
    Ok([part(2)?, part(4)?, part(6)?, part(0)?])
}

/// The names an effect's expressions use, and what of them is not there to read.
struct Names {
    variables: Vec<String>,
    unhandled: BTreeSet<String>,
}

impl Names {
    /// The slot of `name` as the library looks it up, a new one for a variable not seen before.
    fn slot(&mut self, name: &str) -> Option<u16> {
        if let Some(at) = self.variables.iter().position(|known| known == name) {
            return u16::try_from(at).ok();
        }
        if name.starts_with("variable.") {
            self.variables.push(name.to_owned());
            return u16::try_from(self.variables.len() - 1).ok();
        }
        // The game's other functions, `temp.` and `context.`: 0 here.
        self.unhandled.insert(format!("`{name}` reads as 0"));
        None
    }

    /// `text` compiled, 0 with a note when this crate's MoLang does not take it.
    fn expression(&mut self, text: &str) -> Expression {
        // `String.asExpression`.
        let text = if text.is_empty() { "0.0" } else { text };
        match Expression::parse_scoped(text, &mut |name| self.slot(name)) {
            Ok(expression) => expression,
            Err(error) => {
                self.unhandled
                    .insert(format!("`{text}` is taken as 0 ({})", error.0));
                Expression::number(0.0)
            }
        }
    }

    /// The expression at `key` of `object`, `default` when it is not there.
    fn field(&mut self, object: Option<&Value>, key: &str, default: &str) -> Expression {
        let written = object.and_then(|o| o.get(key)).and_then(text);
        self.expression(written.as_deref().unwrap_or(default))
    }

    /// The three expressions at `key` of `object`, zeros when it is not there.
    fn triple(&mut self, object: Option<&Value>, key: &str) -> Result<Triple> {
        match object.and_then(|o| o.get(key)) {
            None | Some(Value::Null) => Ok(std::array::from_fn(|_| Expression::number(0.0))),
            Some(value) => self.three(value, key),
        }
    }

    fn three(&mut self, value: &Value, of: &str) -> Result<Triple> {
        let list = value
            .as_array()
            .filter(|list| list.len() >= 3)
            .ok_or_else(|| bad(format_args!("`{of}` is not three values")))?;
        let mut parts = Vec::with_capacity(3);
        for part in &list[..3] {
            let written =
                text(part).ok_or_else(|| bad(format_args!("`{of}` holds what is no value")))?;
            parts.push(self.expression(&written));
        }
        let [a, b, c]: [Expression; 3] = parts
            .try_into()
            .map_err(|_| bad(format_args!("`{of}` is not three values")))?;
        Ok([a, b, c])
    }

    /// The two expressions of the array `value`, which `of` names.
    fn pair(&mut self, value: Option<&Value>, of: &str) -> Result<[Expression; 2]> {
        let list = value
            .and_then(Value::as_array)
            .filter(|list| list.len() >= 2)
            .ok_or_else(|| bad(format_args!("`{of}` is not two values")))?;
        let mut part = |at: usize| -> Result<Expression> {
            let written = text(&list[at])
                .ok_or_else(|| bad(format_args!("`{of}` holds what is no value")))?;
            Ok(self.expression(&written))
        };
        Ok([part(0)?, part(1)?])
    }

    /// The `;`-separated expressions of `text`, an assignment among them kept as one.
    fn statements(&mut self, text: Option<&str>) -> Vec<Statement> {
        text.unwrap_or_default()
            .split(';')
            .filter(|part| !part.trim().is_empty())
            .map(|part| self.statement(part))
            .collect()
    }

    fn statement(&mut self, text: &str) -> Statement {
        if let Some((target, value)) = assignment(text) {
            // `fixNameShortcut`, in lower case as every name is.
            let mut name = target.to_ascii_lowercase();
            if let Some(rest) = name.strip_prefix("v.") {
                name = format!("variable.{rest}");
            }
            if name.starts_with("variable.") {
                let assign = self.slot(&name);
                return Statement {
                    assign,
                    value: self.expression(value),
                };
            }
            self.unhandled
                .insert(format!("`{}` assigns nothing", text.trim()));
        }
        Statement {
            assign: None,
            value: self.expression(text),
        }
    }
}

/// `name = value` split at its `=`, when `text` is an assignment to a name.
fn assignment(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let at = (0..bytes.len()).find(|&i| {
        bytes[i] == b'='
            && bytes.get(i + 1) != Some(&b'=')
            && !matches!(
                i.checked_sub(1).map(|p| bytes[p]),
                Some(b'=' | b'!' | b'<' | b'>')
            )
    })?;
    let target = text[..at].trim();
    let named = target
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
        && target
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic());
    named.then(|| (target, &text[at + 1..]))
}

impl Effect {
    /// Reads a `*.particle.json` file. An error is a file Cobblemon does not load either: it lacks
    /// what the reader takes for granted, or names a curve, a material or a number it does not know.
    pub fn parse(json: &str) -> Result<Self> {
        let file: Value = crate::json::parse(json, "particle effect")?;
        let effect = file
            .get("particle_effect")
            .filter(|v| v.is_object())
            .ok_or_else(|| bad("no `particle_effect`"))?;
        let description = effect
            .get("description")
            .filter(|v| v.is_object())
            .ok_or_else(|| bad("no `description`"))?;
        let render = description
            .get("basic_render_parameters")
            .filter(|v| v.is_object())
            .ok_or_else(|| bad("no `basic_render_parameters`"))?;
        let none = Value::Object(serde_json::Map::new());
        let components = effect.get("components").unwrap_or(&none);
        let component = |name: &str| components.get(name).filter(|v| v.is_object());
        let billboard = component("minecraft:particle_appearance_billboard")
            .ok_or_else(|| bad("no `minecraft:particle_appearance_billboard`"))?;
        let mut names = Names {
            variables: FIXED.iter().map(|name| (*name).to_owned()).collect(),
            unhandled: BTreeSet::new(),
        };

        let id = description
            .get("identifier")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no `identifier`"))?
            .to_owned();
        let material = render
            .get("material")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no `material`"))?;
        // `substringAfter("_")`: the whole name when it has no underscore.
        let material = match material
            .split_once('_')
            .map_or(material, |(_, after)| after)
            .to_ascii_uppercase()
            .as_str()
        {
            "ALPHA" => Material::Alpha,
            "OPAQUE" => Material::Opaque,
            "BLEND" => Material::Blend,
            "ADD" => Material::Add,
            other => return Err(bad(format_args!("`{other}` is no material"))),
        };
        let texture = render
            .get("texture")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no `texture`"))?;
        let texture = sprite_of(texture);

        let lifetime_json = component("minecraft:particle_lifetime_expression");
        let max_age = names.field(lifetime_json, "max_lifetime", "0.0");
        let kill = names.field(lifetime_json, "expiration_expression", "0.0");
        let size_json = billboard.get("size").and_then(Value::as_array);
        let mut size = |at: usize| {
            let written = size_json.and_then(|list| list.get(at)).and_then(text);
            names.expression(written.as_deref().unwrap_or("1.0"))
        };
        let size = [size(0), size(1)];
        let spin = component("minecraft:particle_initial_spin");

        let mut curves = Vec::new();
        if let Some(map) = effect.get("curves").and_then(Value::as_object) {
            for (name, curve) in map {
                curves.push(read_curve(&mut names, name, curve)?);
            }
        }

        let mut events = HashMap::new();
        if let Some(map) = effect.get("events").and_then(Value::as_object) {
            for (name, event) in map {
                events.insert(name.clone(), read_event(&mut names, name, event)?);
            }
        }

        let initialization = component("minecraft:emitter_initialization");
        let own = |key: &str| {
            initialization
                .and_then(|o| o.get(key))
                .and_then(Value::as_str)
        };
        let start = names.statements(own("creation_expression"));
        let update = names.statements(own("per_update_expression"));
        let particle_initialization = component("minecraft:particle_initialization");
        let own = |key: &str| {
            particle_initialization
                .and_then(|o| o.get(key))
                .and_then(Value::as_str)
        };
        let particle_update = names.statements(own("per_update_expression"));
        let particle_render = names.statements(own("per_render_expressions"));

        let speed = components
            .get("minecraft:particle_initial_speed")
            .and_then(text);
        let speed = names.expression(speed.as_deref().unwrap_or("0.0"));

        let rate = if let Some(instant) = component("minecraft:emitter_rate_instant") {
            Rate::Instant {
                amount: names.field(Some(instant), "num_particles", "1.0"),
            }
        } else if let Some(steady) = component("minecraft:emitter_rate_steady") {
            if steady.get("max_particles").and_then(text).is_none() {
                return Err(bad("a steady rate without `max_particles`"));
            }
            Rate::Steady {
                rate: names.field(Some(steady), "spawn_rate", "1.0"),
                maximum: names.field(Some(steady), "max_particles", "1.0"),
            }
        } else {
            return Err(bad("no emitter rate"));
        };

        let lifetime = if let Some(once) = component("minecraft:emitter_lifetime_once") {
            Lifetime::Once {
                active: names.field(Some(once), "active_time", ""),
            }
        } else if let Some(looping) = component("minecraft:emitter_lifetime_looping") {
            Lifetime::Looping {
                active: names.field(Some(looping), "active_time", ""),
                sleep: names.field(Some(looping), "sleep_time", "0.0"),
            }
        } else if let Some(expression) = component("minecraft:emitter_lifetime_expression") {
            Lifetime::Expression {
                activation: names.field(Some(expression), "activation_expression", ""),
                expiration: names.field(Some(expression), "expiration_expression", ""),
            }
        } else {
            return Err(bad("no emitter lifetime"));
        };

        let flag = |object: Option<&Value>, key: &str| {
            object.and_then(|o| o.get(key)).and_then(Value::as_bool)
        };
        let space_json = component("minecraft:emitter_local_space");
        let local_rotation = flag(space_json, "rotation").unwrap_or(false);
        let space = Space {
            local_position: local_rotation || flag(space_json, "position").unwrap_or(false),
            local_rotation,
            local_velocity: flag(space_json, "velocity").unwrap_or(false),
        };

        // `resolveDirection`, and `parseAttachment` over the defaults of `ParticleEmitterShape`.
        let mut attachment = Attachment {
            position: true,
            rotation: false,
            scale: false,
        };
        let mut attach = |shape: &Value, rotation_follows: bool| {
            let options = shape.get("attachment_options");
            let option = |key: &str| flag(options, key);
            attachment.position = option("position").unwrap_or(true);
            attachment.scale = option("scale").unwrap_or(false);
            attachment.rotation =
                option("rotation").unwrap_or(rotation_follows && space.local_rotation);
        };
        let (shape, shape_json) = if let Some(point) = component("minecraft:emitter_shape_point") {
            attach(point, true);
            (
                Shape::Point {
                    offset: names.triple(Some(point), "offset")?,
                },
                point,
            )
        } else if let Some(sphere) = component("minecraft:emitter_shape_sphere") {
            attach(sphere, false);
            (
                Shape::Sphere {
                    offset: names.triple(Some(sphere), "offset")?,
                    radius: names.field(Some(sphere), "radius", "0.0"),
                    surface_only: flag(Some(sphere), "surface_only").unwrap_or(false),
                },
                sphere,
            )
        } else if let Some(disc) = component("minecraft:emitter_shape_disc") {
            attach(disc, true);
            let normal = match disc.get("plane_normal") {
                Some(list @ Value::Array(_)) => names.three(list, "plane_normal")?,
                axis => {
                    let axis = axis.and_then(Value::as_str).unwrap_or("y");
                    let unit = |on: &str| Expression::number(if axis == on { 1.0 } else { 0.0 });
                    // Anything that is neither `x` nor `y` is z.
                    let z = Expression::number(if matches!(axis, "x" | "y") { 0.0 } else { 1.0 });
                    [unit("x"), unit("y"), z]
                }
            };
            (
                Shape::Disc {
                    offset: names.triple(Some(disc), "offset")?,
                    radius: names.field(Some(disc), "radius", "0.0"),
                    normal,
                    surface_only: flag(Some(disc), "surface_only").unwrap_or(false),
                },
                disc,
            )
        } else if let Some(cuboid) = component("minecraft:emitter_shape_box") {
            attach(cuboid, true);
            (
                Shape::Box {
                    offset: names.triple(Some(cuboid), "offset")?,
                    half: names.triple(Some(cuboid), "half_dimensions")?,
                    surface_only: flag(Some(cuboid), "surface_only").unwrap_or(false),
                },
                cuboid,
            )
        } else if let Some(entity) = component("minecraft:emitter_shape_entity_aabb") {
            (
                Shape::EntityBox {
                    surface_only: flag(Some(entity), "surface_only").unwrap_or(false),
                },
                entity,
            )
        } else {
            return Err(bad("no emitter shape Cobblemon has"));
        };
        let direction = match shape_json.get("direction") {
            None | Some(Value::Null) => Direction::Outwards,
            Some(list @ Value::Array(_)) => Direction::Custom(names.three(list, "direction")?),
            Some(named) => match named.as_str() {
                Some("outwards") => Direction::Outwards,
                _ => Direction::Inwards,
            },
        };

        let dynamic = component("minecraft:particle_motion_dynamic");
        let parametric = component("minecraft:particle_motion_parametric");
        let motion = if dynamic.is_some() {
            Motion::Dynamic {
                direction,
                speed,
                acceleration: names.triple(dynamic, "linear_acceleration")?,
                drag: names.field(dynamic, "linear_drag_coefficient", "0.0"),
            }
        } else if parametric.is_some() {
            Motion::Parametric {
                offset: names.triple(parametric, "relative_position")?,
                direction: names.triple(parametric, "direction")?,
            }
        } else {
            Motion::Static
        };

        let view_direction = match billboard.get("direction").filter(|v| v.is_object()) {
            Some(direction) if direction.get("mode").and_then(Value::as_str) == Some("custom") => {
                let custom = direction
                    .get("custom_direction")
                    .ok_or_else(|| bad("a custom direction without `custom_direction`"))?;
                ViewDirection::Custom(names.three(custom, "custom_direction")?)
            }
            direction => ViewDirection::FromMotion {
                min_speed: direction
                    .and_then(|d| d.get("min_speed_threshold"))
                    .and_then(number)
                    .unwrap_or(0.01),
            },
        };
        let facing = match billboard
            .get("facing_camera_mode")
            .and_then(Value::as_str)
            .unwrap_or("rotate_xyz")
        {
            "rotate_xyz" => Facing::RotateXyz,
            "rotate_y" => Facing::RotateY,
            "lookat_xyz" => Facing::LookAtXyz,
            "lookat_y" => Facing::LookAtY,
            "lookat_direction" => Facing::LookAtDirection,
            "direction_x" => Facing::DirectionX,
            "direction_y" => Facing::DirectionY,
            "direction_z" => Facing::DirectionZ,
            "emitter_transform_xy" => Facing::EmitterXy,
            "emitter_transform_xz" => Facing::EmitterXz,
            // What Cobblemon makes of any other name.
            _ => Facing::EmitterYz,
        };

        let uv_json = billboard
            .get("uv")
            .filter(|v| v.is_object())
            .ok_or_else(|| bad("a billboard without `uv`"))?;
        let dimension = |key: &str| {
            uv_json
                .get(key)
                .and_then(number)
                .map_or(128, |value| value as i32)
        };
        let texture_size = [dimension("texture_width"), dimension("texture_height")];
        let uv = if let Some(flipbook) = uv_json.get("flipbook").filter(|v| v.is_object()) {
            Uv {
                texture: texture_size,
                start: names.pair(flipbook.get("base_UV"), "base_UV")?,
                size: names.pair(flipbook.get("size_UV"), "size_UV")?,
                flipbook: Some(Flipbook {
                    step: names.pair(flipbook.get("step_UV"), "step_UV")?,
                    max_frame: names.field(Some(flipbook), "max_frame", "0.0"),
                    fps: names.field(Some(flipbook), "frames_per_second", "0.0"),
                    stretch_to_lifetime: flag(Some(flipbook), "stretch_to_lifetime")
                        .unwrap_or(false),
                    looping: flag(Some(flipbook), "loop").unwrap_or(false),
                }),
            }
        } else {
            let whole = Value::Array(vec![Value::from("128"), Value::from("128")]);
            Uv {
                texture: texture_size,
                start: names.pair(uv_json.get("uv"), "uv")?,
                size: names.pair(Some(uv_json.get("uv_size").unwrap_or(&whole)), "uv_size")?,
                flipbook: None,
            }
        };

        let rotation = match dynamic
            .or(parametric)
            .and_then(|motion| motion.get("rotation"))
            .and_then(text)
        {
            Some(expression) => Rotation::Parametric(names.expression(&expression)),
            None => Rotation::Dynamic {
                start: names.field(spin, "rotation", "0.0"),
                speed: names.field(spin, "rotation_rate", "0.0"),
                acceleration: names.field(dynamic, "rotation_acceleration", "0.0"),
                drag: names.field(dynamic, "rotation_drag_coefficient", "0.0"),
            },
        };

        let colour =
            component("minecraft:particle_appearance_tinting").and_then(|t| t.get("color"));
        let tint = match colour {
            Some(gradient @ Value::Object(_)) => {
                let interpolant = gradient
                    .get("interpolant")
                    .and_then(text)
                    .ok_or_else(|| bad("a gradient without `interpolant`"))?;
                let colours = gradient
                    .get("gradient")
                    .and_then(Value::as_object)
                    .ok_or_else(|| bad("a gradient that is no map of colours"))?;
                let mut stops: Vec<(f64, [f32; 4])> = Vec::new();
                for (key, colour) in colours {
                    let at: f64 = key
                        .trim()
                        .parse()
                        .map_err(|_| bad(format_args!("`{key}` is no place in a gradient")))?;
                    let colour = hex(colour.as_str().unwrap_or_default())?;
                    stops.retain(|(known, _)| *known != at);
                    stops.push((at, colour));
                }
                Tint::Gradient {
                    interpolant: names.expression(&interpolant),
                    stops,
                }
            }
            Some(Value::Array(list)) => {
                if list.len() < 4 {
                    return Err(bad("a colour of fewer than four values"));
                }
                let mut channel = |at: usize| -> Result<Expression> {
                    let written =
                        text(&list[at]).ok_or_else(|| bad("a colour holds what is no value"))?;
                    Ok(names.expression(&written))
                };
                Tint::Expression([channel(0)?, channel(1)?, channel(2)?, channel(3)?])
            }
            _ => Tint::Expression(std::array::from_fn(|_| Expression::number(1.0))),
        };

        let collision = match component("minecraft:particle_motion_collision") {
            Some(json) => {
                let collides = json.get("collision_radius").and_then(text).is_some();
                // Whatever `enabled` says, nothing collides here.
                names
                    .unhandled
                    .insert("its particles pass through blocks".to_owned());
                Collision {
                    enabled: names.field(
                        Some(json),
                        "enabled",
                        if collides { "1.0" } else { "0.0" },
                    ),
                    radius: names.field(Some(json), "collision_radius", "0.1"),
                    friction: names.field(Some(json), "collision_drag", "10.0"),
                    bounciness: names.field(Some(json), "coefficient_of_restitution", "0.0"),
                    expires_on_contact: flag(Some(json), "expire_on_contact").unwrap_or(false),
                }
            }
            None => Collision {
                enabled: Expression::number(0.0),
                radius: Expression::number(0.1),
                friction: Expression::number(10.0),
                bounciness: Expression::number(0.0),
                expires_on_contact: false,
            },
        };

        let particle_events = component("minecraft:particle_lifetime_events");
        let emitter_events = component("minecraft:emitter_lifetime_events");
        let event = |from: Option<&Value>, key: &str| names_of(from, key);
        let looping_travel = emitter_events
            .and_then(|e| e.get("looping_travel_distance_events"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|entry| {
                        let distance = entry
                            .get("distance")
                            .and_then(number)
                            .ok_or_else(|| bad("a travel event without `distance`"))?;
                        Ok((distance, names_of(Some(entry), "events")))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let emitter = Emitter {
            start,
            update,
            rate,
            lifetime,
            shape,
            attachment,
            entity_scaling: component("cobblemon:emitter_space")
                .and_then(|space| space.get("scaling"))
                .and_then(Value::as_str)
                .is_some_and(|scaling| scaling.eq_ignore_ascii_case("entity")),
            creation_events: event(emitter_events, "creation_event"),
            expiration_events: event(emitter_events, "expiration_event"),
            timeline: timeline(emitter_events.and_then(|e| e.get("timeline")))?,
            travel_distance_events: timeline(
                emitter_events.and_then(|e| e.get("travel_distance_events")),
            )?,
            looping_travel_distance_events: looping_travel,
        };
        let particle = Particle {
            motion,
            rotation,
            uv,
            size,
            max_age,
            kill,
            update: particle_update,
            render: particle_render,
            view_direction,
            facing,
            tint,
            collision,
            environment_lighting: components
                .get("minecraft:particle_appearance_lighting")
                .is_some(),
            creation_events: event(particle_events, "creation_event"),
            expiration_events: event(particle_events, "expiration_event"),
            timeline: timeline(particle_events.and_then(|e| e.get("timeline")))?,
        };
        Ok(Self {
            id,
            texture,
            material,
            curves,
            events,
            emitter,
            particle,
            space,
            variables: names.variables,
            unhandled: names.unhandled.into_iter().collect(),
        })
    }

    /// Where the effect's texture is under `assets/<namespace>/`: the namespace, and
    /// `textures/particle/<path>.png`, which is where the particle atlas takes its sprites from.
    pub fn texture_path(&self) -> (&str, String) {
        let (namespace, path) = self
            .texture
            .split_once(':')
            .unwrap_or(("cobblemon", &self.texture));
        (namespace, format!("textures/particle/{path}.png"))
    }

    /// The effects this one's events start, by their identifiers.
    pub fn children(&self) -> impl Iterator<Item = &str> {
        self.events
            .values()
            .filter_map(|event| event.particle.as_ref())
            .map(|(effect, _)| effect.as_str())
    }
}

fn names_of(from: Option<&Value>, key: &str) -> Vec<String> {
    names(from.and_then(|object| object.get(key)))
}

/// `SnowstormParticleReader`'s way from a file's `texture` to a sprite of the particle atlas: without
/// its `.png`, without any `particles/` and then any `textures/`, in lower case, in Cobblemon's
/// namespace unless it names one.
fn sprite_of(texture: &str) -> String {
    let texture = if texture.ends_with(".png") {
        texture.replace(".png", "")
    } else {
        texture.to_owned()
    };
    let id = texture
        .replace("particles/", "")
        .replace("textures/", "")
        .to_lowercase();
    if id.contains(':') {
        id
    } else {
        format!("cobblemon:{id}")
    }
}

fn read_curve(names: &mut Names, name: &str, curve: &Value) -> Result<Curve> {
    // `substringAfter(".")`: the whole name when it has no dot.
    let variable = name.split_once('.').map_or(name, |(_, after)| after);
    let slot = names
        .slot(&format!("variable.{}", variable.to_ascii_lowercase()))
        .ok_or_else(|| bad("too many variables"))?;
    let input = curve
        .get("input")
        .and_then(text)
        .ok_or_else(|| bad(format_args!("curve `{name}` has no `input`")))?;
    let input = names.expression(&input);
    let mut range = || -> Result<Expression> {
        let written = curve
            .get("horizontal_range")
            .and_then(text)
            .ok_or_else(|| bad(format_args!("curve `{name}` has no `horizontal_range`")))?;
        Ok(names.expression(&written))
    };
    let nodes = || -> Result<Vec<f64>> {
        curve
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| bad(format_args!("curve `{name}` has no list of nodes")))?
            .iter()
            .map(|node| {
                number(node).ok_or_else(|| bad(format_args!("curve `{name}`: a node is no number")))
            })
            .collect()
    };
    let kind = match curve.get("type").and_then(Value::as_str) {
        Some("linear") => CurveKind::Linear {
            range: range()?,
            nodes: nodes()?,
        },
        Some("catmull_rom") => CurveKind::CatmullRom {
            range: range()?,
            nodes: nodes()?,
        },
        Some("bezier") => {
            let range = range()?;
            let nodes: [f64; 4] = nodes()?
                .get(..4)
                .and_then(|four| four.try_into().ok())
                .ok_or_else(|| bad(format_args!("curve `{name}` needs four nodes")))?;
            CurveKind::Bezier { range, nodes }
        }
        Some("bezier_chain") => {
            let map = curve
                .get("nodes")
                .and_then(Value::as_object)
                .ok_or_else(|| bad(format_args!("curve `{name}` has no map of nodes")))?;
            // By time; the last of two nodes at one time.
            let mut sorted: BTreeMap<u64, (f64, f64, f64)> = BTreeMap::new();
            for (key, node) in map {
                let time: f64 = key
                    .trim()
                    .parse()
                    .map_err(|_| bad(format_args!("curve `{name}`: `{key}` is no time")))?;
                let part = |of: &str| {
                    node.get(of)
                        .and_then(number)
                        .ok_or_else(|| bad(format_args!("curve `{name}`: a node without `{of}`")))
                };
                // The bits of a time that is not negative sort as the time does.
                sorted.insert(
                    time.max(0.0).to_bits(),
                    (time, part("value")?, part("slope")?),
                );
            }
            let sorted: Vec<(f64, f64, f64)> = sorted.into_values().collect();
            CurveKind::BezierChain(
                sorted
                    .windows(2)
                    .map(|pair| {
                        let ((start, v1, s1), (end, v2, s2)) = (pair[0], pair[1]);
                        (start, end, [v1, v1 + s1 / 3.0, v2 - s2 / 3.0, v2])
                    })
                    .collect(),
            )
        }
        other => {
            return Err(bad(format_args!(
                "curve `{name}` is of a type Cobblemon does not have: {other:?}"
            )));
        }
    };
    Ok(Curve { slot, input, kind })
}

fn read_event(names: &mut Names, name: &str, event: &Value) -> Result<Event> {
    let particle = match event.get("particle_effect").filter(|v| v.is_object()) {
        Some(particle) => {
            let effect = particle
                .get("effect")
                .and_then(Value::as_str)
                .ok_or_else(|| bad(format_args!("event `{name}` names no effect")))?
                .to_lowercase();
            let effect = if effect.contains(':') {
                effect
            } else {
                format!("cobblemon:{effect}")
            };
            let kind = match particle
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str()
            {
                "EMITTER" => EventKind::Emitter,
                "EMITTER_BOUND" => EventKind::EmitterBound,
                "PARTICLE" => EventKind::Particle,
                "PARTICLE_WITH_VELOCITY" => EventKind::ParticleWithVelocity,
                other => {
                    return Err(bad(format_args!(
                        "event `{name}` starts its effect as `{other}`"
                    )));
                }
            };
            if particle.get("pre_effect_expression").is_some() {
                names.unhandled.insert(format!(
                    "event `{name}`'s `pre_effect_expression` is not run"
                ));
            }
            Some((effect, kind))
        }
        None => None,
    };
    let sound = event
        .get("sound_effect")
        .and_then(|sound| sound.get("event_name"))
        .and_then(Value::as_str)
        .map(|sound| {
            let sound = sound.to_lowercase();
            if sound.contains(':') {
                sound
            } else {
                format!("cobblemon:{sound}")
            }
        });
    let expression = names.statements(event.get("expression").and_then(Value::as_str));
    Ok(Event {
        particle,
        sound,
        expression,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLASH: &str = r#"{"format_version":"1.10.0","particle_effect":{
        "description":{"identifier":"cobblemon:pokeball/casual/sendflash","basic_render_parameters":{
            "material":"particles_alpha","texture":"textures/particles/balls/pokeball/casual/sendcasual"}},
        "components":{
            "minecraft:emitter_rate_instant":{"num_particles":1},
            "minecraft:emitter_lifetime_once":{"active_time":1},
            "minecraft:emitter_shape_point":{"offset":[0,0.5,0]},
            "minecraft:particle_lifetime_expression":{"max_lifetime":1},
            "minecraft:particle_initial_speed":0,
            "minecraft:particle_motion_dynamic":{},
            "minecraft:particle_appearance_billboard":{"size":[1.25,1.25],"facing_camera_mode":"rotate_xyz",
                "uv":{"texture_width":384,"texture_height":32,"flipbook":{"base_UV":[0,0],"size_UV":[32,32],
                    "step_UV":[32,0],"frames_per_second":24,"max_frame":12}}},
            "cobblemon:emitter_space":{"scaling":"entity"}}}}"#;

    fn constant(expression: &Expression) -> f32 {
        expression.constant().expect("a constant")
    }

    #[test]
    fn the_send_flash_reads_as_cobblemon_reads_it() {
        let effect = Effect::parse(FLASH).unwrap();
        assert_eq!(effect.id, "cobblemon:pokeball/casual/sendflash");
        assert_eq!(effect.texture, "cobblemon:balls/pokeball/casual/sendcasual");
        assert_eq!(
            effect.texture_path(),
            (
                "cobblemon",
                "textures/particle/balls/pokeball/casual/sendcasual.png".to_owned()
            )
        );
        assert_eq!(effect.material, Material::Alpha);
        assert!(
            matches!(&effect.emitter.rate, Rate::Instant { amount } if constant(amount) == 1.0)
        );
        assert!(
            matches!(&effect.emitter.lifetime, Lifetime::Once { active } if constant(active) == 1.0)
        );
        let Shape::Point { offset } = &effect.emitter.shape else {
            panic!("a point");
        };
        assert_eq!(offset.each_ref().map(constant), [0.0, 0.5, 0.0]);
        assert!(effect.emitter.entity_scaling);
        assert_eq!(
            effect.emitter.attachment,
            Attachment {
                position: true,
                rotation: false,
                scale: false
            }
        );
        // No direction: outwards, and a speed of 0.
        let Motion::Dynamic {
            direction, speed, ..
        } = &effect.particle.motion
        else {
            panic!("dynamic");
        };
        assert_eq!((direction, constant(speed)), (&Direction::Outwards, 0.0));
        assert_eq!(effect.particle.size.each_ref().map(constant), [1.25, 1.25]);
        assert_eq!(effect.particle.uv.texture, [384, 32]);
        let flipbook = effect.particle.uv.flipbook.as_ref().unwrap();
        assert_eq!(constant(&flipbook.max_frame), 12.0);
        assert_eq!(constant(&flipbook.fps), 24.0);
        assert_eq!(flipbook.step.each_ref().map(constant), [32.0, 0.0]);
        assert!(!flipbook.looping && !flipbook.stretch_to_lifetime);
        assert_eq!(effect.particle.facing, Facing::RotateXyz);
        assert!(!effect.particle.environment_lighting);
        assert_eq!(effect.variables.len(), slot::OWN);
        assert!(effect.unhandled.is_empty(), "{:?}", effect.unhandled);
    }

    #[test]
    fn a_texture_becomes_a_sprite_of_the_particle_atlas() {
        assert_eq!(
            sprite_of("textures/particles/generic/sparkle/shinesparkle_rainbow"),
            "cobblemon:generic/sparkle/shinesparkle_rainbow"
        );
        assert_eq!(
            sprite_of("textures/particle/Smoke.png"),
            "cobblemon:particle/smoke"
        );
        assert_eq!(
            sprite_of("cobblemon:textures/particle/vanilla/poof"),
            "cobblemon:particle/vanilla/poof"
        );
        assert_eq!(sprite_of("textures/blocks/lava"), "cobblemon:blocks/lava");
    }

    #[test]
    fn names_get_slots_and_assignments_are_kept() {
        let json = FLASH
            .replace(
                r#""minecraft:emitter_rate_instant":{"num_particles":1}"#,
                r#""minecraft:emitter_initialization":{"creation_expression":"v.Size = 0.5;variable.n=math.random(1,2);q.sound('a', 1)",
                    "per_update_expression":"v.size = v.size + 1"},
                   "minecraft:emitter_rate_instant":{"num_particles":"v.n + q.entity_size"}"#,
            )
            .replace(
                r#""size":[1.25,1.25]"#,
                r#""size":["v.size * variable.particle_age","t.x"]"#,
            );
        let effect = Effect::parse(&json).unwrap();
        assert_eq!(
            effect.variables[slot::OWN..],
            ["variable.size".to_owned(), "variable.n".to_owned()]
        );
        let start = &effect.emitter.start;
        assert_eq!(start.len(), 3);
        assert_eq!(start[0].assign, Some(slot::OWN as u16));
        assert_eq!(start[1].assign, Some(slot::OWN as u16 + 1));
        assert_eq!(start[2].assign, None);
        assert_eq!(effect.emitter.update[0].assign, Some(slot::OWN as u16));
        // What is not there to read is said.
        assert_eq!(
            effect.unhandled,
            ["`query.sound` reads as 0", "`temp.x` reads as 0"]
        );
        // The size reads its own variable and the particle's age.
        let mut slots = vec![0.0; effect.variables.len()];
        slots[slot::OWN] = 3.0;
        slots[slot::PARTICLE_AGE] = 0.5;
        let random = std::cell::Cell::new(1);
        let scope = crate::molang::Scope {
            slots: &slots,
            random: &random,
        };
        assert_eq!(effect.particle.size[0].eval_scoped(&scope), 1.5);
    }

    #[test]
    fn an_assignment_is_told_from_a_comparison() {
        assert_eq!(assignment("v.x = 1"), Some(("v.x", " 1")));
        assert_eq!(assignment("variable.a=b==c"), Some(("variable.a", "b==c")));
        assert_eq!(assignment("v.x == 1"), None);
        assert_eq!(assignment("v.x <= 1"), None);
        assert_eq!(assignment("v.x != 1"), None);
        assert_eq!(assignment("math.max(1, 2)"), None);
        assert_eq!(assignment("(v.x) = 1"), None);
    }

    #[test]
    fn tints_curves_and_events_are_read() {
        let json = FLASH
            .replace(
                r#""components":{"#,
                r##""curves":{"variable.fade":{"type":"linear","input":"v.particle_age","horizontal_range":"v.particle_lifetime","nodes":[1,0.5,0]},
                    "variable.arc":{"type":"bezier_chain","input":"v.particle_age","nodes":{"1.0":{"value":1,"slope":0},"0.0":{"value":0,"slope":3}}}},
                  "events":{"spark":{"particle_effect":{"effect":"cobblemon:Shine","type":"particle"}},
                    "chime":{"sound_effect":{"event_name":"cobblemon:particle.shiny_chime"}}},
                  "components":{"minecraft:particle_appearance_tinting":{"color":{"interpolant":"v.particle_age / v.particle_lifetime",
                      "gradient":{"0.0":"#FFFF0000","1.0":"#0000FF80"}}},
                    "minecraft:emitter_lifetime_events":{"creation_event":["spark","chime"],"timeline":{"0.70":"spark","0.2":["chime"]}},
                    "minecraft:particle_lifetime_events":{"expiration_event":"spark"},"##,
            );
        let effect = Effect::parse(&json).unwrap();
        let Tint::Gradient { stops, .. } = &effect.particle.tint else {
            panic!("a gradient");
        };
        assert_eq!(stops[0], (0.0, [1.0, 0.0, 0.0, 1.0]));
        assert_eq!(stops[1].0, 1.0);
        assert_eq!(stops[1].1[..2], [0.0, 1.0]);
        assert_eq!(effect.curves.len(), 2);
        assert!(effect.curves.iter().any(|curve| matches!(
            &curve.kind,
            CurveKind::BezierChain(pieces) if pieces == &[(0.0, 1.0, [0.0, 1.0, 1.0, 1.0])]
        )));
        assert_eq!(
            effect.events["spark"].particle,
            Some(("cobblemon:shine".to_owned(), EventKind::Particle))
        );
        assert_eq!(
            effect.events["chime"].sound.as_deref(),
            Some("cobblemon:particle.shiny_chime")
        );
        assert_eq!(effect.emitter.creation_events, ["spark", "chime"]);
        assert_eq!(
            effect.emitter.timeline,
            [
                (0.2, vec!["chime".to_owned()]),
                (0.7, vec!["spark".to_owned()])
            ]
        );
        assert_eq!(effect.particle.expiration_events, ["spark"]);
        assert_eq!(effect.children().collect::<Vec<_>>(), ["cobblemon:shine"]);
    }

    #[test]
    fn what_cobblemon_does_not_load_is_an_error() {
        let without = |part: &str| Effect::parse(&FLASH.replace(part, r#""x":{}"#));
        assert!(without(r#""minecraft:emitter_rate_instant":{"num_particles":1}"#).is_err());
        assert!(without(r#""minecraft:emitter_lifetime_once":{"active_time":1}"#).is_err());
        assert!(without(r#""minecraft:emitter_shape_point":{"offset":[0,0.5,0]}"#).is_err());
        assert!(Effect::parse(&FLASH.replace("particles_alpha", "particles_glow")).is_err());
        assert!(Effect::parse("{}").is_err());
        assert!(hex("#FFFFFF").is_err());
    }
}
