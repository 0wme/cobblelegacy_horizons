// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Particle effects running: `ParticleStorm`, the emitter of an effect, and `SnowstormParticle`, each of
//! its particles, as Cobblemon 1.8.1 ticks them, twenty times a second.
//!
//! Nothing here knows the game: whoever plays an effect gives each emitter the place it is at
//! ([`Frame`]), keeps the ones that follow something where that is ([`Storm::follow`]), ticks them
//! ([`Storms::tick`]) and draws what [`Storms::quads`] describes. An effect's oddities are Cobblemon's
//! and are kept: a rotation rate is divided by twenty every tick, an event on a timeline at the very
//! time of a tick happens twice, a gradient is no colour at all exactly on one of its stops.
//!
//! Not done: collisions with blocks (`SnowstormParticle.checkCollision`), the turn a particle of an
//! effect in local rotation makes between two ticks (it is drawn where the last tick put it), and the
//! frustum test before a particle is drawn.
use crate::molang::{Expression, Scope};
use crate::snowstorm::{
    Curve, CurveKind, Direction, Effect, EventKind, Lifetime, Motion, Rate, Rotation, Shape,
    Statement, Tint, ViewDirection, slot,
};
use std::{cell::Cell, f64::consts::TAU, sync::Arc};

pub type Vec3 = [f64; 3];

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: Vec3, by: f64) -> Vec3 {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn length(a: Vec3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// `Vec3.normalize`: nothing of a vector too short to have a direction.
fn normalize(a: Vec3) -> Vec3 {
    let length = length(a);
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        scale(a, 1.0 / length)
    }
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A rotation, as the rows of its matrix.
pub type Turn = [[f64; 3]; 3];

pub const NO_TURN: Turn = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn turn(by: &Turn, a: Vec3) -> Vec3 {
    [dot(by[0], a), dot(by[1], a), dot(by[2], a)]
}

/// `turn` by the rotation the other way: its transpose.
fn turn_back(by: &Turn, a: Vec3) -> Vec3 {
    std::array::from_fn(|i| by[0][i] * a[0] + by[1][i] * a[1] + by[2][i] * a[2])
}

/// `Quaternionf.rotateTo`: the shortest rotation that takes the unit vector `from` to `to`.
fn turn_to(from: Vec3, to: Vec3) -> Turn {
    let cosine = dot(from, to);
    if cosine < -1.0 + 1.0e-6 {
        // Opposite: half a turn about any axis square to `from`.
        let axis = normalize(if from[0].abs() < 0.9 {
            cross(from, [1.0, 0.0, 0.0])
        } else {
            cross(from, [0.0, 1.0, 0.0])
        });
        return std::array::from_fn(|r| {
            std::array::from_fn(|c| 2.0 * axis[r] * axis[c] - if r == c { 1.0 } else { 0.0 })
        });
    }
    let v = cross(from, to);
    let k = 1.0 / (1.0 + cosine);
    [
        [
            1.0 - k * (v[1] * v[1] + v[2] * v[2]),
            k * v[0] * v[1] - v[2],
            k * v[0] * v[2] + v[1],
        ],
        [
            k * v[0] * v[1] + v[2],
            1.0 - k * (v[0] * v[0] + v[2] * v[2]),
            k * v[1] * v[2] - v[0],
        ],
        [
            k * v[0] * v[2] - v[1],
            k * v[1] * v[2] + v[0],
            1.0 - k * (v[0] * v[0] + v[1] * v[1]),
        ],
    ]
}

/// A `MatrixWrapper` as emitters have one: a place, a rotation and one scale for the three axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub origin: Vec3,
    pub rotation: Turn,
    pub scale: f64,
}

impl Frame {
    /// A place in the world, not turned: what an effect played at a position has.
    pub fn at(origin: Vec3) -> Self {
        Self {
            origin,
            rotation: NO_TURN,
            scale: 1.0,
        }
    }

    /// A place turned about the vertical by `degrees`, as `Axis.YP.rotationDegrees` turns.
    pub fn turned(origin: Vec3, degrees: f64, scale: f64) -> Self {
        let (sine, cosine) = degrees.to_radians().sin_cos();
        Self {
            origin,
            rotation: [[cosine, 0.0, sine], [0.0, 1.0, 0.0], [-sine, 0.0, cosine]],
            scale,
        }
    }

    /// `Matrix4f.transformDirection`: turned, as long as it was.
    fn direction(&self, of: Vec3) -> Vec3 {
        turn(&self.rotation, of)
    }

    /// `transformPosition`, or `transformPositionWithoutScale`.
    fn position(&self, of: Vec3, scaled: bool) -> Vec3 {
        let turned = turn(&self.rotation, of);
        add(
            self.origin,
            if scaled {
                scale(turned, self.scale)
            } else {
                turned
            },
        )
    }

    /// `transformWorldToParticle` of the difference of two places.
    fn into_local(&self, difference: Vec3) -> Vec3 {
        scale(turn_back(&self.rotation, difference), 1.0 / self.scale)
    }
}

/// The entity an emitter is on, as an effect's expressions ask about it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    /// Its box's width and height.
    pub width: f64,
    pub height: f64,
    /// `query.entity_scale`: a Pokémon's base scale by its own; 1 for anything else.
    pub scale: f64,
    /// The middle of its box, which `minecraft:emitter_shape_entity_aabb` spawns around.
    pub center: Vec3,
}

/// What an emitter follows: whoever plays it keeps it there ([`Storm::follow`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The entity's id on the network.
    pub entity: i32,
    /// The locator of its model the emitter is at.
    pub locator: String,
    /// Whether the emitter moves with it, or was only born there.
    pub bound: bool,
}

/// A sound an event asked for: `world.playLocalSound(..., SoundSource.NEUTRAL, 1, 1, true)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub id: String,
    pub at: Vec3,
}

/// One particle to draw, a quad centred on `center`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub center: Vec3,
    /// Half its width and half its height.
    pub half: [f32; 2],
    /// Its turn about the axis it is looked at along, in degrees.
    pub roll: f32,
    /// The part of the effect's texture it shows: left, top, right, bottom, as fractions of the
    /// texture. Cobblemon lets it leave the texture; the atlas around it shows then.
    pub uv: [f32; 4],
    /// Red, green, blue, alpha.
    pub colour: [f32; 4],
    /// `viewDirection`: the way the `Direction*` facings turn it.
    pub view: Vec3,
    /// The rotation of the emitter's frame, for the `Emitter*` facings.
    pub rotation: Turn,
}

fn eval(expression: &Expression, slots: &[f64], random: &Cell<u64>) -> f64 {
    expression.eval_scoped(&Scope { slots, random })
}

fn eval3(triple: &[Expression; 3], slots: &[f64], random: &Cell<u64>) -> Vec3 {
    [
        eval(&triple[0], slots, random),
        eval(&triple[1], slots, random),
        eval(&triple[2], slots, random),
    ]
}

/// `runtime.execute(expressions)`: each in turn, an assignment kept.
fn run(statements: &[Statement], slots: &mut [f64], random: &Cell<u64>) {
    for statement in statements {
        let value = eval(&statement.value, slots, random);
        if let Some(slot) = statement.assign
            && let Some(variable) = slots.get_mut(usize::from(slot))
        {
            *variable = value;
        }
    }
}

fn roll(random: &Cell<u64>) -> f64 {
    Scope::roll(random)
}

/// `catmullRom`.
fn catmull_rom(t: f64, p0: f64, p1: f64, p2: f64, p3: f64) -> f64 {
    let v0 = (p2 - p0) * 0.5;
    let v1 = (p3 - p1) * 0.5;
    let t2 = t * t;
    let t3 = t * t2;
    (2.0 * p1 - 2.0 * p2 + v0 + v1) * t3 + (-3.0 * p1 + 3.0 * p2 - 2.0 * v0 - v1) * t2 + v0 * t + p1
}

/// `CubedBezierCurve.getY`.
fn bezier(t: f64, [v0, v1, v2, v3]: [f64; 4]) -> f64 {
    let k = 1.0 - t;
    k * k * k * v0 + 3.0 * k * k * t * v1 + 3.0 * k * t * t * v2 + t * t * t * v3
}

/// `MoLangCurve.resolve`.
fn curve_value(curve: &Curve, input: f64, slots: &[f64], random: &Cell<u64>) -> f64 {
    match &curve.kind {
        CurveKind::Linear { range, nodes } => {
            let Some((&first, &last)) = nodes.first().zip(nodes.last()) else {
                return 0.0;
            };
            let between = eval(range, slots, random) / (nodes.len() - 1) as f64;
            let index = (input / between).floor();
            if index < 0.0 || index.is_nan() {
                first
            } else if index + 1.0 >= nodes.len() as f64 {
                last
            } else {
                let (left, right) = (nodes[index as usize], nodes[index as usize + 1]);
                left + (right - left) * (input.rem_euclid(between) / between)
            }
        }
        CurveKind::CatmullRom { range, nodes } => {
            if nodes.is_empty() {
                return 0.0;
            }
            let segments = nodes.len() as f64 - 3.0;
            let position = input / eval(range, slots, random) * segments;
            let p = (nodes.len() - 1) as f64 * ((position + 1.0) / (segments + 2.0));
            let point = p.floor();
            // The library reads past its nodes and throws beyond the last; the last stands here.
            let node = |index: f64| nodes[(index.max(0.0) as usize).min(nodes.len() - 1)];
            catmull_rom(
                p - point,
                node(point - 1.0),
                node(point),
                node(point + 1.0),
                node(point + 2.0),
            )
        }
        CurveKind::Bezier { range, nodes } => bezier(input / eval(range, slots, random), *nodes),
        CurveKind::BezierChain(pieces) => {
            let position = input.clamp(0.0, 1.0);
            let Some(&(start, end, nodes)) = pieces
                .iter()
                .rev()
                .find(|(start, ..)| position >= *start)
                .or(pieces.first())
            else {
                return 0.0;
            };
            bezier((position - start) / (end - start), nodes)
        }
    }
}

/// `effect.curves.forEach { it.apply(runtime) }`.
fn curves(effect: &Effect, slots: &mut [f64], random: &Cell<u64>) {
    for curve in &effect.curves {
        let input = eval(&curve.input, slots, random);
        let value = curve_value(curve, input, slots, random);
        if let Some(variable) = slots.get_mut(usize::from(curve.slot)) {
            *variable = value;
        }
    }
}

/// Emitters an event may start one inside the other, and emitters at once: an effect whose events
/// start it again goes no further.
const DEPTH: usize = 16;
const MOST: usize = 1024;

/// One particle: `SnowstormParticle`.
#[derive(Clone, Debug)]
struct Particle {
    slots: Vec<f64>,
    /// Where it is in the world, as of its last move.
    position: Vec3,
    /// Where it is from `origin`, and where it was a tick ago.
    local: Vec3,
    previous_local: Vec3,
    origin: Vec3,
    /// Blocks a tick.
    velocity: Vec3,
    roll: f32,
    previous_roll: f32,
    angular: f64,
    age: i32,
    lifetime: i32,
    view: Vec3,
    invisible: bool,
    /// Its emitter's frame as it was when the particle started, for an effect whose particles do not
    /// stay in their emitter's space.
    frame: Frame,
}

/// What an emitter's tick leaves for the others: the emitters its events start and the sounds they
/// ask for.
struct Out<'a> {
    effects: &'a dyn Fn(&str) -> Option<Arc<Effect>>,
    storms: Vec<Storm>,
    sounds: &'a mut Vec<Sound>,
    /// How many emitters there are, with those of `storms`.
    count: usize,
    depth: usize,
}

/// One emitter of an effect and its particles: `ParticleStorm`.
#[derive(Clone, Debug)]
pub struct Storm {
    pub effect: Arc<Effect>,
    /// `emitterSpaceMatrix`.
    emitter: Frame,
    /// `attachedMatrix`, when it is not the emitter's own frame.
    attached: Option<Frame>,
    /// What the emitter follows.
    pub tag: Option<Tag>,
    /// What whoever started the effect gave it to find its emitter again ([`Start::source`]); 0 for
    /// the emitters an effect's own events start.
    pub source: u64,
    slots: Vec<f64>,
    random: Cell<u64>,
    age: i32,
    started: bool,
    stopped: bool,
    despawned: bool,
    played_once: bool,
    position: Vec3,
    travelled: f32,
    /// `emitter_overflow`: the part of a particle a steady rate owes.
    overflow: f64,
    particles: Vec<Particle>,
    /// The particles of this tick, which tick from the next one on.
    fresh: Vec<Particle>,
    body: Option<Body>,
    /// What the effect is aimed at.
    target: Option<Vec3>,
    /// `sourceVelocity`, in blocks a tick.
    velocity: Vec3,
    /// `sourceAlive` and `sourceVisible`.
    alive: bool,
    visible: bool,
}

impl Storm {
    /// `ParticleStorm`'s constructor: the emitter's own expressions run, then its creation events.
    fn create(
        effect: Arc<Effect>,
        emitter: Frame,
        attached: Option<Frame>,
        seed: u64,
        out: &mut Out,
    ) -> Self {
        let mut storm = Self {
            slots: vec![0.0; effect.variables.len()],
            emitter,
            attached,
            tag: None,
            source: 0,
            random: Cell::new(seed),
            age: 0,
            started: false,
            stopped: false,
            despawned: false,
            played_once: false,
            position: emitter.origin,
            travelled: 0.0,
            overflow: 0.0,
            particles: Vec::new(),
            fresh: Vec::new(),
            body: None,
            target: None,
            velocity: [0.0; 3],
            alive: true,
            visible: true,
            effect,
        };
        let effect = storm.effect.clone();
        run(&effect.emitter.start, &mut storm.slots, &storm.random);
        for event in &effect.emitter.creation_events {
            storm.trigger(event, None, out);
        }
        storm
    }

    /// `ParticleStorm.spawn`: what the emitter's entity measures, and where its target is.
    fn spawn(&mut self, body: Option<Body>, target: Option<Vec3>) {
        self.body = body;
        self.target = target;
        if let Some(body) = body {
            let size = body.width.max(body.height);
            let measures = [body.width, body.height, size, size / 2.0];
            self.slots[slot::QUERY_ENTITY..slot::QUERY_ENTITY + 4].copy_from_slice(&measures);
            self.slots[slot::QUERY_ENTITY + 4] = body.scale;
            self.slots[slot::ENTITY..slot::ENTITY + 4].copy_from_slice(&measures);
            // `PokemonEntity.scale`, the attribute, which is 1 for every Pokémon.
            self.slots[slot::ENTITY + 4] = 1.0;
            if target.is_some() {
                let aim = self.aim();
                self.slots[slot::QUERY_TARGET..slot::QUERY_TARGET + 4].copy_from_slice(&aim);
                self.slots[slot::TARGET..slot::TARGET + 4].copy_from_slice(&aim);
            }
        }
    }

    /// `distanceTo(target)` as `addAttackingFunctions` gives it: the three differences in the
    /// emitter's space, the vertical one the other way up, and the distance.
    fn aim(&self) -> [f64; 4] {
        let Some(target) = self.target else {
            return [0.0; 4];
        };
        let [x, y, z] = self.emitter.into_local(sub(self.position, target));
        [x, -y, z, length([x, y, z])]
    }

    /// Where the emitter is.
    pub fn position(&self) -> Vec3 {
        self.position
    }

    /// The emitter is gone (`ParticleStorm.remove`, which is when Cobblemon tells whoever started it):
    /// it makes no more particles, though those it made may still be there.
    pub fn despawned(&self) -> bool {
        self.despawned
    }

    /// Nothing is left of it: it stopped and its particles are gone.
    pub fn over(&self) -> bool {
        self.despawned && self.particles.is_empty() && self.fresh.is_empty()
    }

    /// How many particles it has.
    pub fn particles(&self) -> usize {
        self.particles.len() + self.fresh.len()
    }

    /// Where what the emitter follows is now, and whether it is still there and seen: the locator's
    /// frame, the entity's motion in blocks a tick, `!isRemoved`, `!isInvisible`.
    pub fn follow(&mut self, frame: Option<Frame>, velocity: Vec3, alive: bool, visible: bool) {
        (self.alive, self.visible) = (alive, visible);
        self.velocity = velocity;
        let Some(frame) = frame else {
            return;
        };
        match &mut self.attached {
            Some(attached) => *attached = frame,
            // The emitter's frame is the locator's itself.
            None => self.emitter = frame,
        }
    }

    /// What the effect is aimed at, when that moves.
    pub fn aim_at(&mut self, target: Vec3) {
        if self.target.is_some() {
            self.target = Some(target);
        }
    }

    /// `SimpleEventTrigger.trigger` and `ParticleEvent.run`: event `name`, from the emitter or from
    /// one of its particles (where it is, and its motion).
    fn trigger(&mut self, name: &str, particle: Option<(Vec3, Vec3)>, out: &mut Out) {
        let effect = self.effect.clone();
        let Some(event) = effect.events.get(name) else {
            return;
        };
        let at = particle.map_or(self.position, |(at, _)| at);
        if let Some((id, kind)) = &event.particle
            && out.depth < DEPTH
            && out.count < MOST
            && let Some(child) = (out.effects)(id)
        {
            let on_emitter = matches!(kind, EventKind::Emitter | EventKind::EmitterBound);
            let frame = if on_emitter {
                self.emitter
            } else {
                Frame {
                    origin: at,
                    ..self.emitter
                }
            };
            let bound = *kind == EventKind::EmitterBound;
            // Bound, it follows what this one follows: this one's own frame when that is all there is.
            let attached = bound.then(|| self.attached.unwrap_or(self.emitter));
            out.depth += 1;
            out.count += 1;
            let seed = self.random.get() ^ (out.count as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
            roll(&self.random);
            let mut storm = Self::create(child, frame, attached, seed, out);
            out.depth -= 1;
            storm.spawn(self.body, self.target);
            storm.tag = self.tag.clone().map(|tag| Tag {
                bound: bound && tag.bound,
                ..tag
            });
            storm.velocity = match kind {
                EventKind::Emitter | EventKind::EmitterBound => self.velocity,
                EventKind::Particle => [0.0; 3],
                EventKind::ParticleWithVelocity => particle.map_or([0.0; 3], |(_, moving)| moving),
            };
            (storm.alive, storm.visible) = (self.alive, self.visible);
            out.storms.push(storm);
        }
        if let Some(sound) = &event.sound {
            out.sounds.push(Sound {
                id: sound.clone(),
                at,
            });
        }
        run(&event.expression, &mut self.slots, &self.random);
    }

    /// `EventTriggerTimeline.check`: the events from `previous` to `now`, both ends counted.
    fn timeline(
        &mut self,
        timeline: &[(f64, Vec<String>)],
        previous: f64,
        now: f64,
        particle: Option<(Vec3, Vec3)>,
        out: &mut Out,
    ) {
        for (_, events) in timeline
            .iter()
            .filter(|(at, _)| (previous..=now).contains(at))
        {
            for event in events {
                // An event the effect does not have ends the whole check.
                if !self.effect.events.contains_key(event) {
                    return;
                }
                self.trigger(event, particle, out);
            }
        }
    }

    /// `remove`: the expiration events, once.
    fn despawn(&mut self, out: &mut Out) {
        if !self.despawned {
            let effect = self.effect.clone();
            for event in &effect.emitter.expiration_events {
                self.trigger(event, None, out);
            }
            self.despawned = true;
        }
    }

    /// `ParticleStorm.tick`.
    fn tick(&mut self, out: &mut Out) {
        let effect = self.effect.clone();
        // `Particle.tick`: an emitter that stopped lives no longer.
        if self.stopped {
            self.despawn(out);
        }
        self.age += 1;
        if !self.played_once {
            self.age = 0;
            self.played_once = true;
        }
        if !self.alive && !self.stopped {
            self.stopped = true;
            self.despawn(out);
        }
        if !self.visible {
            for particle in self.particles.iter_mut().chain(&mut self.fresh) {
                particle.invisible = true;
            }
            return;
        }
        if self.stopped {
            return;
        }

        let previous = self.position;
        if let Some(attached) = self.attached {
            self.position = attached.origin;
            let follows = effect.emitter.attachment;
            if follows.position {
                self.emitter.origin = attached.origin;
            }
            if follows.rotation {
                self.emitter.rotation = attached.rotation;
            }
            if follows.scale {
                self.emitter.scale = attached.scale;
            }
        } else {
            self.position = self.emitter.origin;
        }

        let before = f64::from(self.travelled);
        self.travelled += length(sub(self.position, previous)) as f32;
        let after = f64::from(self.travelled);
        self.timeline(
            &effect.emitter.travel_distance_events,
            before,
            after,
            None,
            out,
        );
        for (distance, events) in &effect.emitter.looping_travel_distance_events {
            if before < *distance && after >= *distance {
                for event in events {
                    self.trigger(event, None, out);
                }
            }
        }
        let age = f64::from(self.age) / 20.0;
        self.timeline(
            &effect.emitter.timeline,
            f64::from(self.age - 1) / 20.0,
            age,
            None,
            out,
        );

        for random in 0..4 {
            self.slots[slot::EMITTER_RANDOM + random] = roll(&self.random);
        }
        self.slots[slot::EMITTER_AGE] = age;
        if self.target.is_some() {
            let aim = self.aim();
            self.slots[slot::QUERY_TARGET..slot::QUERY_TARGET + 4].copy_from_slice(&aim);
        }
        run(&effect.emitter.update, &mut self.slots, &self.random);

        // `ParticleEmitterLifetime.getAction`.
        let (go, stop, reset) = match &effect.emitter.lifetime {
            Lifetime::Once { active } => {
                let active = eval(active, &self.slots, &self.random);
                self.slots[slot::EMITTER_LIFETIME] = active;
                (age <= active, age > active, false)
            }
            Lifetime::Looping { active, sleep } => {
                let active = eval(active, &self.slots, &self.random);
                let sleep = eval(sleep, &self.slots, &self.random);
                self.slots[slot::EMITTER_LIFETIME] = active;
                if age > active && sleep == 0.0 {
                    (false, true, false)
                } else {
                    let go = age % (active + sleep) < active;
                    (go, false, !go)
                }
            }
            Lifetime::Expression {
                activation,
                expiration,
            } => {
                if self.started {
                    let over = eval(expiration, &self.slots, &self.random) != 0.0;
                    (!over, over, false)
                } else {
                    (
                        eval(activation, &self.slots, &self.random) != 0.0,
                        false,
                        false,
                    )
                }
            }
        };
        if go {
            curves(&effect, &mut self.slots, &self.random);
            // `ParticleEmitterRate.getEmitCount`.
            let active = self.particles() as i64;
            let count = match &effect.emitter.rate {
                Rate::Instant { amount } => {
                    if self.started {
                        0
                    } else {
                        i64::from(eval(amount, &self.slots, &self.random) as i32)
                    }
                }
                Rate::Steady { rate, maximum } => {
                    let most = i64::from(eval(maximum, &self.slots, &self.random) as i32);
                    if active >= most {
                        0
                    } else {
                        let wanted = eval(rate, &self.slots, &self.random) / 20.0 + self.overflow;
                        let whole = wanted as i32;
                        self.overflow = wanted - f64::from(whole);
                        i64::from(whole).min(most - active)
                    }
                }
            };
            self.started = true;
            // No effect of Cobblemon's asks for more than a few hundred at once.
            for _ in 0..count.clamp(0, 4096) {
                self.emit(out);
            }
        } else if stop {
            self.stopped = true;
        } else if reset {
            self.started = false;
        }
    }

    /// `getNewParticlePosition` and `getCenter` of the emitter's shape, in its own space.
    fn shape(&self, center_only: bool) -> Vec3 {
        let (slots, random) = (&self.slots[..], &self.random);
        // Bedrock's x axis runs the other way.
        let mirrored = |offset: &[Expression; 3]| {
            let [x, y, z] = eval3(offset, slots, random);
            [-x, y, z]
        };
        // A place in a box of these sizes around its middle, on one of its faces when `surface`.
        let boxed = |sizes: Vec3, surface: bool| -> Vec3 {
            let face = if surface {
                Some((roll(random) * 6.0) as usize)
            } else {
                None
            };
            std::array::from_fn(|axis| match face {
                Some(face) if face / 2 == axis => {
                    if face % 2 == 0 {
                        -sizes[axis] / 2.0
                    } else {
                        sizes[axis] / 2.0
                    }
                }
                _ => roll(random) * sizes[axis] - sizes[axis] / 2.0,
            })
        };
        match &self.effect.emitter.shape {
            Shape::Point { offset } => {
                if center_only {
                    [0.0; 3]
                } else {
                    mirrored(offset)
                }
            }
            Shape::Sphere {
                offset,
                radius,
                surface_only,
            } => {
                if center_only {
                    return mirrored(offset);
                }
                let radius =
                    eval(radius, slots, random) * if *surface_only { 1.0 } else { roll(random) };
                let (theta, psi) = (TAU * roll(random), TAU * roll(random));
                add(
                    mirrored(offset),
                    [
                        radius * theta.cos() * psi.sin(),
                        radius * theta.sin() * psi.sin(),
                        radius * psi.cos(),
                    ],
                )
            }
            Shape::Disc {
                offset,
                radius,
                normal,
                surface_only,
            } => {
                let center = mirrored(offset);
                if center_only {
                    return center;
                }
                let normal = eval3(normal, slots, random);
                let normal = normalize(if normal == [0.0; 3] {
                    [0.0, 1.0, 0.0]
                } else {
                    normal
                });
                let radius = eval(radius, slots, random);
                let distance = if *surface_only {
                    radius
                } else {
                    roll(random) * radius
                };
                let theta = roll(random) * TAU;
                let flat = [distance * theta.cos(), 0.0, distance * theta.sin()];
                add(center, turn(&turn_to([0.0, 1.0, 0.0], normal), flat))
            }
            Shape::Box {
                offset,
                half,
                surface_only,
            } => {
                let center = mirrored(offset);
                if center_only {
                    return center;
                }
                let sizes = eval3(half, slots, random).map(|half| half * 2.0 + 0.0001);
                add(center, boxed(sizes, *surface_only))
            }
            Shape::EntityBox { surface_only } => {
                // Without an entity: a box a block wide and two high.
                let (center, sizes) = self.body.map_or(([0.0; 3], [1.0, 2.0, 1.0]), |body| {
                    (body.center, [body.width, body.height, body.width])
                });
                if center_only {
                    return center;
                }
                add(center, boxed(sizes, *surface_only))
            }
        }
    }

    /// `spawnParticle`, and `SnowstormParticle`'s constructor.
    fn emit(&mut self, out: &mut Out) {
        let effect = self.effect.clone();
        for random in 0..4 {
            self.slots[slot::PARTICLE_RANDOM + random] = roll(&self.random);
        }
        let scaled = effect.emitter.attachment.scale;
        let position = self.emitter.position(self.shape(false), scaled);
        let center = self.emitter.position(self.shape(true), scaled);
        let (slots, random) = (&self.slots[..], &self.random);
        // `ParticleMotion.getInitialVelocity`, in blocks a second.
        let initial = match &effect.particle.motion {
            Motion::Dynamic {
                direction, speed, ..
            } => {
                let apart = |from: Vec3, to: Vec3| {
                    if from == to {
                        [roll(random) - 0.5, roll(random) - 0.5, roll(random) - 0.5]
                    } else {
                        sub(to, from)
                    }
                };
                let direction = match direction {
                    Direction::Outwards => normalize(apart(center, position)),
                    Direction::Inwards => normalize(apart(position, center)),
                    Direction::Custom(direction) => {
                        self.emitter.direction(eval3(direction, slots, random))
                    }
                };
                scale(normalize(direction), eval(speed, slots, random))
            }
            Motion::Parametric { .. } | Motion::Static => [0.0; 3],
        };
        let mut velocity = scale(initial, 1.0 / 20.0);
        if effect.space.local_velocity {
            velocity = add(velocity, self.velocity);
        }

        // The particle's own variables: the emitter's as they are now.
        let mut slots = self.slots.clone();
        if self.target.is_some() {
            slots[slot::TARGET..slot::TARGET + 4].copy_from_slice(&self.aim());
        }
        let random = &self.random;
        let (start, angular) = match &effect.particle.rotation {
            Rotation::Parametric(expression) => (eval(expression, &slots, random), 0.0),
            Rotation::Dynamic { start, speed, .. } => (
                eval(start, &slots, random),
                eval(speed, &slots, random) / 20.0,
            ),
        };
        let lifetime = (eval(&effect.particle.max_age, &slots, random) * 20.0) as i32;
        let local = sub(position, self.position);
        self.fresh.push(Particle {
            slots,
            position,
            local,
            previous_local: local,
            origin: self.position,
            velocity,
            roll: -(start as f32),
            previous_roll: -(start as f32),
            angular,
            age: 0,
            lifetime,
            view: [0.0; 3],
            invisible: false,
            frame: self.emitter,
        });
        for event in &effect.particle.creation_events {
            self.trigger(event, Some((position, velocity)), out);
        }
    }

    /// What a particle's expressions read besides its own variables: new random numbers of the
    /// emitter's kind, its age, what the emitter's functions answer now, then the curves.
    fn prepare(&self, particle: &mut Particle) {
        for random in 0..4 {
            particle.slots[slot::EMITTER_RANDOM + random] = roll(&self.random);
        }
        particle.slots[slot::PARTICLE_AGE] = f64::from(particle.age) / 20.0;
        particle.slots[slot::PARTICLE_LIFETIME] = f64::from(particle.lifetime) / 20.0;
        let functions = slot::QUERY_ENTITY..slot::OWN;
        particle.slots[functions.clone()].copy_from_slice(&self.slots[functions]);
        curves(&self.effect, &mut particle.slots, &self.random);
    }

    /// `SnowstormParticle.tick`. False once the particle is gone.
    fn tick_particle(&mut self, particle: &mut Particle, out: &mut Out) -> bool {
        let effect = self.effect.clone();
        let local = effect.space.local_position;
        if local {
            particle.origin = self.emitter.origin;
        }
        self.prepare(particle);
        let random = &self.random;
        run(&effect.particle.update, &mut particle.slots, random);
        let slots = &particle.slots[..];
        // `ParticleRotation.getAngularVelocity`, divided by twenty every tick.
        particle.angular = match &effect.particle.rotation {
            Rotation::Parametric(expression) => {
                eval(expression, slots, random) + f64::from(particle.roll)
            }
            Rotation::Dynamic {
                acceleration, drag, ..
            } => {
                let next = particle.angular * 20.0 + eval(acceleration, slots, random);
                let drag = next * eval(drag, slots, random);
                particle.angular
                    + if drag.abs() > next.abs() {
                        0.0
                    } else {
                        next - drag - particle.angular * 20.0
                    }
            }
        } / 20.0;

        if particle.age >= particle.lifetime || eval(&effect.particle.kill, slots, random) != 0.0 {
            for event in &effect.particle.expiration_events {
                self.trigger(event, Some((particle.position, particle.velocity)), out);
            }
            return false;
        }
        let random = &self.random;
        let slots = &particle.slots[..];
        let frame = if local { self.emitter } else { particle.frame };
        // `ParticleMotion.getVelocity`.
        particle.velocity = match &effect.particle.motion {
            Motion::Dynamic {
                acceleration, drag, ..
            } => {
                let acceleration = eval3(acceleration, slots, random);
                let drag = eval(drag, slots, random);
                // Blocks a second a second, as blocks a tick a tick.
                add(
                    particle.velocity,
                    scale(
                        sub(acceleration, scale(particle.velocity, 20.0 * drag)),
                        1.0 / 400.0,
                    ),
                )
            }
            Motion::Parametric { offset, .. } => sub(
                frame.position(eval3(offset, slots, random), true),
                particle.position,
            ),
            Motion::Static => particle.velocity,
        };
        particle.previous_roll = particle.roll;
        // Bedrock turns the other way.
        particle.roll = particle.previous_roll - particle.angular as f32;
        particle.view = normalize(match &effect.particle.view_direction {
            ViewDirection::FromMotion { min_speed } => {
                if length(particle.velocity) * 20.0 >= *min_speed {
                    normalize(particle.velocity)
                } else {
                    particle.view
                }
            }
            ViewDirection::Custom(direction) => eval3(direction, slots, random),
        });

        particle.previous_local = particle.local;
        particle.age += 1;
        // `move`, without the blocks in the way.
        particle.local = add(particle.local, particle.velocity);
        particle.position = add(self.placed(particle.local), particle.origin);
        let now = f64::from(particle.age) / 20.0;
        self.timeline(
            &effect.particle.timeline,
            f64::from(particle.age - 1) / 20.0,
            now,
            Some((particle.position, particle.velocity)),
            out,
        );
        true
    }

    /// A particle's place from its origin, turned with what the emitter is attached to when the
    /// effect is in local rotation.
    fn placed(&self, local: Vec3) -> Vec3 {
        if self.effect.space.local_rotation {
            self.attached.unwrap_or(self.emitter).direction(local)
        } else {
            local
        }
    }

    fn tick_particles(&mut self, out: &mut Out) {
        let mut particles = std::mem::take(&mut self.particles);
        particles.retain_mut(|particle| self.tick_particle(particle, out));
        particles.append(&mut self.fresh);
        self.particles = particles;
    }

    /// `SnowstormParticle.render`: each particle as it is `partial` of a tick after the last one.
    fn quads(&mut self, partial: f64, draw: &mut dyn FnMut(&Effect, &Quad)) {
        let effect = self.effect.clone();
        let mut particles = std::mem::take(&mut self.particles);
        let mut fresh = std::mem::take(&mut self.fresh);
        for particle in particles.iter_mut().chain(&mut fresh) {
            if particle.invisible {
                continue;
            }
            self.prepare(particle);
            let random = &self.random;
            run(&effect.particle.render, &mut particle.slots, random);
            let slots = &particle.slots[..];
            let between: Vec3 = std::array::from_fn(|axis| {
                particle.previous_local[axis]
                    + (particle.local[axis] - particle.previous_local[axis]) * partial
            });
            let center = add(self.placed(between), particle.origin);
            // A particle an expression sent nowhere (a division by an age of 0) is never seen.
            if !center.iter().all(|n| n.is_finite()) {
                continue;
            }
            let half = [
                eval(&effect.particle.size[0], slots, random) as f32 / 1.5,
                eval(&effect.particle.size[1], slots, random) as f32 / 1.5,
            ];

            // `ParticleUVMode.get`, at the particle's age in whole ticks.
            let uv = &effect.particle.uv;
            let age = f64::from(particle.age) / 20.0;
            let (start, size) = match &uv.flipbook {
                Some(flipbook) => {
                    let last = (eval(&flipbook.max_frame, slots, random) as i32) - 1;
                    let step = [
                        eval(&flipbook.step[0], slots, random),
                        eval(&flipbook.step[1], slots, random),
                    ];
                    let size = [
                        eval(&uv.size[0], slots, random),
                        eval(&uv.size[1], slots, random),
                    ];
                    let frame = if flipbook.stretch_to_lifetime {
                        let life = f64::from(particle.lifetime) / 20.0;
                        ((age / life) * f64::from(last)) as i32
                    } else {
                        let played = age * eval(&flipbook.fps, slots, random);
                        if !flipbook.looping && played >= f64::from(last) {
                            last
                        } else {
                            (played % f64::from(last)) as i32
                        }
                    };
                    let start = [
                        eval(&uv.start[0], slots, random) + f64::from(frame) * step[0],
                        eval(&uv.start[1], slots, random) + f64::from(frame) * step[1],
                    ];
                    (start, size)
                }
                None => {
                    let start = [
                        eval(&uv.start[0], slots, random),
                        eval(&uv.start[1], slots, random),
                    ];
                    // Cobblemon evaluates the start a second time for the far corner.
                    let again = [
                        eval(&uv.start[0], slots, random),
                        eval(&uv.start[1], slots, random),
                    ];
                    let size = [
                        eval(&uv.size[0], slots, random),
                        eval(&uv.size[1], slots, random),
                    ];
                    (
                        start,
                        [again[0] - start[0] + size[0], again[1] - start[1] + size[1]],
                    )
                }
            };
            let [width, height] = uv.texture.map(f64::from);
            let uv = [
                (start[0] / width) as f32,
                (start[1] / height) as f32,
                ((start[0] + size[0]) / width) as f32,
                ((start[1] + size[1]) / height) as f32,
            ];

            // `ParticleTinting.getTint`.
            let colour = match &effect.particle.tint {
                Tint::Expression(channels) => {
                    std::array::from_fn(|channel| eval(&channels[channel], slots, random) as f32)
                }
                Tint::Gradient { interpolant, stops } => {
                    let at = eval(interpolant, slots, random);
                    let nearest = |keep: &dyn Fn(f64) -> bool| {
                        stops
                            .iter()
                            .filter(|(key, _)| keep(*key))
                            .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()))
                    };
                    match (nearest(&|key| key <= at), nearest(&|key| key >= at)) {
                        (None, None) => [1.0; 4],
                        (None, Some(only)) | (Some(only), None) => only.1,
                        (Some(below), Some(above)) => {
                            // On a stop this is 0 over 0, and no colour at all.
                            let along = ((at - below.0) / (above.0 - below.0)) as f32;
                            std::array::from_fn(|channel| {
                                below.1[channel] + along * (above.1[channel] - below.1[channel])
                            })
                        }
                    }
                }
            };
            let roll = if particle.roll == 0.0 {
                0.0
            } else {
                particle.previous_roll + (particle.roll - particle.previous_roll) * partial as f32
            };
            let frame = if effect.space.local_position {
                self.emitter
            } else {
                particle.frame
            };
            draw(
                &effect,
                &Quad {
                    center,
                    half,
                    roll,
                    uv,
                    colour,
                    view: particle.view,
                    rotation: frame.rotation,
                },
            );
        }
        self.particles = particles;
        self.fresh = fresh;
    }
}

/// Where an effect starts, and on what.
#[derive(Clone, Debug)]
pub struct Start {
    /// `emitterSpaceMatrix`: where the emitter is and how it is turned.
    pub emitter: Frame,
    /// `attachedMatrix` when it is another frame than the emitter's: the locator the emitter follows
    /// by what its shape's attachment says. Without it the emitter's frame is the one that is followed.
    pub attached: Option<Frame>,
    pub tag: Option<Tag>,
    pub body: Option<Body>,
    pub target: Option<Vec3>,
    /// A number to find the emitter again by ([`Storm::source`]), for whoever needs to know when it is
    /// gone or to end it; 0 for none.
    pub source: u64,
}

impl Start {
    /// An effect played at a place in the world, on nothing: `ParticleStorm.createAtPosition`.
    pub fn at(position: Vec3) -> Self {
        Self {
            emitter: Frame::at(position),
            attached: None,
            tag: None,
            body: None,
            target: None,
            source: 0,
        }
    }
}

/// The effects that are playing.
#[derive(Debug, Default)]
pub struct Storms {
    storms: Vec<Storm>,
    /// The sounds the events asked for since they were last taken.
    pub sounds: Vec<Sound>,
    seed: u64,
}

impl Storms {
    /// Starts `effect`: `ParticleStorm(...).spawn()`. `effects` finds the effects its events start, by
    /// their identifiers.
    pub fn start(
        &mut self,
        effect: Arc<Effect>,
        start: Start,
        effects: &dyn Fn(&str) -> Option<Arc<Effect>>,
    ) {
        if self.storms.len() >= MOST {
            return;
        }
        // Each emitter draws its own numbers.
        self.seed = self
            .seed
            .wrapping_add(0x9E37_79B9_7F4A_7C15)
            .wrapping_mul(0xBF58_476D_1CE4_E5B9)
            | 1;
        let mut out = Out {
            effects,
            storms: Vec::new(),
            sounds: &mut self.sounds,
            count: self.storms.len() + 1,
            depth: 0,
        };
        let mut storm = Storm::create(effect, start.emitter, start.attached, self.seed, &mut out);
        storm.spawn(start.body, start.target);
        storm.tag = start.tag;
        storm.source = start.source;
        // Its creation events' emitters were born on nothing: they are on what it is on.
        let mut born = out.storms;
        for child in &mut born {
            if child.body.is_none() {
                child.spawn(start.body, start.target);
            }
            if child.tag.is_none() {
                child.tag = storm.tag.clone().map(|tag| Tag {
                    bound: tag.bound && child.attached.is_some(),
                    ..tag
                });
            }
        }
        self.storms.push(storm);
        self.storms.append(&mut born);
    }

    /// Seeds the numbers the effects draw, for a test that needs the same ones twice.
    pub fn seed(&mut self, seed: u64) {
        self.seed = seed;
    }

    /// One tick, a twentieth of a second: every emitter, then its particles. The emitters and the
    /// particles this tick makes start ticking with the next one.
    pub fn tick(&mut self, effects: &dyn Fn(&str) -> Option<Arc<Effect>>) {
        let mut out = Out {
            effects,
            storms: Vec::new(),
            sounds: &mut self.sounds,
            count: self.storms.len(),
            depth: 0,
        };
        for storm in &mut self.storms {
            storm.tick(&mut out);
            storm.tick_particles(&mut out);
        }
        let mut born = out.storms;
        self.storms.retain(|storm| !storm.over());
        self.storms.append(&mut born);
    }

    /// What there is to draw `partial` of a tick after the last one.
    pub fn quads(&mut self, partial: f64, draw: &mut dyn FnMut(&Effect, &Quad)) {
        for storm in &mut self.storms {
            storm.quads(partial, draw);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Storm> {
        self.storms.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Storm> {
        self.storms.iter_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.storms.is_empty()
    }

    pub fn clear(&mut self) {
        self.storms.clear();
        self.sounds.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An effect of these components, on a 64 by 8 texture of eight frames.
    fn effect(id: &str, extra: &str, components: &str) -> Arc<Effect> {
        let json = format!(
            r#"{{"particle_effect":{{"description":{{"identifier":"{id}","basic_render_parameters":{{
                "material":"particles_alpha","texture":"textures/particles/test"}}}},{extra}
            "components":{{{components}}}}}}}"#
        );
        Arc::new(Effect::parse(&json).unwrap_or_else(|e| panic!("{}: {json}", e.0)))
    }

    const BILLBOARD: &str = r#""minecraft:particle_appearance_billboard":{"size":[0.3,0.15],
        "facing_camera_mode":"rotate_xyz","uv":{"texture_width":64,"texture_height":8,
        "flipbook":{"base_UV":[0,0],"size_UV":[8,8],"step_UV":[8,0],"frames_per_second":20,"max_frame":8}}}"#;

    fn none(_: &str) -> Option<Arc<Effect>> {
        None
    }

    fn drawn(storms: &mut Storms, partial: f64) -> Vec<Quad> {
        let mut quads = Vec::new();
        storms.quads(partial, &mut |_, quad| quads.push(*quad));
        quads
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1.0e-9
    }

    #[test]
    fn an_instant_emitter_makes_its_particles_once_and_they_live_their_ticks() {
        let flash = effect(
            "test:flash",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":3}},
                "minecraft:emitter_lifetime_once":{{"active_time":0.2}},
                "minecraft:emitter_shape_point":{{"offset":[1,0.5,2]}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":0.5}},
                "minecraft:particle_motion_dynamic":{{}},{BILLBOARD}"#
            ),
        );
        let mut storms = Storms::default();
        storms.start(flash, Start::at([10.0, 64.0, -3.0]), &none);
        // Nothing before the first tick.
        assert!(drawn(&mut storms, 0.0).is_empty());
        storms.tick(&none);
        let quads = drawn(&mut storms, 0.5);
        assert_eq!(quads.len(), 3);
        // The offset's x runs the other way; a size is a third wider than its half.
        assert_eq!(quads[0].center, [9.0, 64.5, -1.0]);
        assert_eq!(quads[0].half, [0.2, 0.1]);
        assert_eq!(quads[0].uv, [0.0, 0.0, 0.125, 1.0]);
        assert_eq!(quads[0].colour, [1.0; 4]);
        // No more of them on the ticks after, while the emitter is active and once it is not.
        for _ in 0..5 {
            storms.tick(&none);
            assert_eq!(drawn(&mut storms, 0.0).len(), 3);
        }
        // Ten ticks of life from the tick after the one that made them, then the tick that ends them.
        for _ in 0..5 {
            storms.tick(&none);
        }
        assert_eq!(drawn(&mut storms, 0.0).len(), 3);
        storms.tick(&none);
        assert!(drawn(&mut storms, 0.0).is_empty());
        assert!(storms.is_empty());
    }

    #[test]
    fn a_flipbook_shows_the_frame_of_the_particle_s_age_in_ticks_and_holds_its_last() {
        let sparks = effect(
            "test:frames",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":1}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_shape_point":{{}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":1}},{BILLBOARD}"#
            ),
        );
        let mut storms = Storms::default();
        storms.start(sparks, Start::at([0.0; 3]), &none);
        let mut frames = Vec::new();
        for _ in 0..12 {
            storms.tick(&none);
            // Whatever the part of the tick: the age counts whole ticks.
            frames.push((drawn(&mut storms, 0.9)[0].uv[0] * 8.0).round() as i32);
        }
        // Twenty frames a second is one a tick; eight frames are numbered up to 7, where it stays.
        assert_eq!(frames, [0, 1, 2, 3, 4, 5, 6, 7, 7, 7, 7, 7]);
    }

    #[test]
    fn a_dynamic_particle_leaves_at_its_speed_and_is_slowed_and_pulled() {
        let sparks = effect(
            "test:sparks",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":1}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_shape_sphere":{{"direction":[0,3,4]}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":2}},
                "minecraft:particle_initial_speed":10,
                "minecraft:particle_motion_dynamic":{{"linear_acceleration":[0,-8,0],"linear_drag_coefficient":2}},
                {BILLBOARD}"#
            ),
        );
        let mut storms = Storms::default();
        storms.start(sparks, Start::at([0.0; 3]), &none);
        storms.tick(&none);
        assert_eq!(drawn(&mut storms, 0.0)[0].center, [0.0; 3]);
        // A tick: the velocity (0, 6, 8) a second, a twentieth of it a tick, changed by
        // (acceleration - velocity * drag) over four hundred before the particle moves.
        storms.tick(&none);
        let (vy, vz) = (0.3 + (-8.0 - 6.0 * 2.0) / 400.0, 0.4 + (-8.0 * 2.0) / 400.0);
        let at = drawn(&mut storms, 1.0)[0].center;
        assert!(close(at[1], vy) && close(at[2], vz), "{at:?}");
        // Half way through the tick it is half way there.
        let half = drawn(&mut storms, 0.5)[0].center;
        assert!(close(half[1], vy / 2.0) && close(half[2], vz / 2.0));
        storms.tick(&none);
        let (vy2, vz2) = (
            vy + (-8.0 - vy * 20.0 * 2.0) / 400.0,
            vz + (-vz * 20.0 * 2.0) / 400.0,
        );
        let at = drawn(&mut storms, 1.0)[0].center;
        assert!(close(at[1], vy + vy2) && close(at[2], vz + vz2), "{at:?}");
    }

    #[test]
    fn a_steady_emitter_keeps_its_rate_and_its_most_and_stops_after_its_time() {
        let sparkle = effect(
            "test:sparkle",
            "",
            &format!(
                r#""minecraft:emitter_rate_steady":{{"spawn_rate":30,"max_particles":4}},
                "minecraft:emitter_lifetime_once":{{"active_time":0.25}},
                "minecraft:emitter_shape_sphere":{{"offset":[0,1.5,0],"radius":2,"surface_only":true,"direction":"outwards"}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":0.1}},
                "minecraft:particle_initial_speed":1,
                "minecraft:particle_motion_dynamic":{{}},{BILLBOARD}"#
            ),
        );
        let mut storms = Storms::default();
        storms.start(sparkle, Start::at([0.0; 3]), &none);
        let mut alive = Vec::new();
        for _ in 0..10 {
            storms.tick(&none);
            alive.push(drawn(&mut storms, 0.0).len());
        }
        // One and a half a tick: 1, 2, 1... while fewer than four are alive, counted before those of
        // the tick die: none on the fourth tick, one of the two owed on the fifth. Each is gone on the
        // third tick after its own, and none is made after the sixth tick (0.25 seconds).
        assert_eq!(alive, [1, 3, 4, 3, 2, 2, 2, 1, 0, 0]);
        assert!(storms.is_empty());
        // On the sphere's surface, two blocks from its middle, and leaving it.
        let mut storms = Storms::default();
        let sparkle = effect(
            "test:shell",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":20}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_shape_sphere":{{"offset":[0,1.5,0],"radius":2,"surface_only":true}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":1}},
                "minecraft:particle_initial_speed":2,
                "minecraft:particle_motion_dynamic":{{}},{BILLBOARD}"#
            ),
        );
        storms.start(sparkle, Start::at([5.0, 0.0, 5.0]), &none);
        storms.tick(&none);
        let first = drawn(&mut storms, 0.0);
        storms.tick(&none);
        let second = drawn(&mut storms, 1.0);
        for (a, b) in first.iter().zip(&second) {
            let middle = [5.0, 1.5, 5.0];
            assert!(close(length(sub(a.center, middle)), 2.0));
            assert!((length(sub(b.center, middle)) - 2.1).abs() < 1.0e-6);
        }
    }

    #[test]
    fn variables_curves_and_a_gradient_follow_the_particle_s_age() {
        let fading = effect(
            "test:fading",
            r##""curves":{"variable.grow":{"type":"linear","input":"v.particle_age","horizontal_range":"v.particle_lifetime","nodes":[0,1,0]}},"##,
            r##""minecraft:emitter_initialization":{"creation_expression":"v.base = 0.6;"},
                "minecraft:emitter_rate_instant":{"num_particles":1},
                "minecraft:emitter_lifetime_once":{"active_time":1},
                "minecraft:emitter_shape_point":{},
                "minecraft:particle_lifetime_expression":{"max_lifetime":0.4},
                "minecraft:particle_appearance_tinting":{"color":{"interpolant":"v.particle_age / v.particle_lifetime",
                    "gradient":{"0.0":"#FF000000","1.0":"#00FF0000"}}},
                "minecraft:particle_appearance_billboard":{"size":["v.base * v.grow","v.particle_random_1 * 0 + 0.3"],
                    "facing_camera_mode":"rotate_xyz","uv":{"texture_width":16,"texture_height":16,
                    "uv":["math.floor(v.particle_age * 10) * 4",4],"uv_size":[4,4]}}"##,
        );
        let mut storms = Storms::default();
        storms.start(fading, Start::at([0.0; 3]), &none);
        storms.tick(&none);
        // At its birth the gradient is on its first stop: no colour.
        let born = drawn(&mut storms, 0.0)[0];
        assert!(born.colour.iter().all(|channel| channel.is_nan()));
        assert_eq!(born.half, [0.0, 0.2]);
        for _ in 0..4 {
            storms.tick(&none);
        }
        // Four ticks old of eight: half way along the curve and the gradient, the third column.
        let half = drawn(&mut storms, 0.0)[0];
        assert!((half.half[0] - 0.4).abs() < 1.0e-6);
        assert!((half.colour[0] - 0.5).abs() < 1.0e-6 && (half.colour[3] - 0.5).abs() < 1.0e-6);
        assert_eq!(half.uv, [0.5, 0.25, 0.75, 0.5]);
    }

    #[test]
    fn events_start_other_effects_and_ask_for_sounds() {
        let child = effect(
            "test:child",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":2}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_shape_point":{{}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":1}},{BILLBOARD}"#
            ),
        );
        let parent = effect(
            "test:parent",
            r#""events":{"ring":{"particle_effect":{"effect":"test:child","type":"emitter"}},
                "chime":{"sound_effect":{"event_name":"test:chime"}},
                "burst":{"particle_effect":{"effect":"test:child","type":"particle"}}},"#,
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":1}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_lifetime_events":{{"creation_event":["ring","chime"],"timeline":{{"0.12":"chime"}}}},
                "minecraft:emitter_shape_point":{{"offset":[0,2,0]}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":0.1}},
                "minecraft:particle_lifetime_events":{{"expiration_event":"burst"}},{BILLBOARD}"#
            ),
        );
        let find = |id: &str| (id == "test:child").then(|| child.clone());
        let mut storms = Storms::default();
        storms.start(parent, Start::at([1.0, 0.0, 0.0]), &find);
        // Its creation events: another emitter where it is, and a sound.
        assert_eq!(storms.iter().count(), 2);
        assert_eq!(
            storms.sounds,
            [Sound {
                id: "test:chime".into(),
                at: [1.0, 0.0, 0.0]
            }]
        );
        storms.sounds.clear();
        storms.tick(&find);
        assert_eq!(drawn(&mut storms, 0.0).len(), 3);
        // 0.12 seconds is between the second tick's age and the third's.
        storms.tick(&find);
        storms.tick(&find);
        assert!(storms.sounds.is_empty());
        storms.tick(&find);
        assert_eq!(storms.sounds.len(), 1);
        // The parent's particle is gone after its two ticks: an emitter where it was.
        assert_eq!(storms.iter().count(), 3);
        assert_eq!(storms.iter().last().unwrap().position(), [1.0, 2.0, 0.0]);
        storms.tick(&find);
        assert_eq!(drawn(&mut storms, 0.0).len(), 4);
    }

    #[test]
    fn an_emitter_follows_what_it_is_on_and_its_local_particles_with_it() {
        let ring = effect(
            "test:ring",
            "",
            &format!(
                r#""minecraft:emitter_local_space":{{"position":true,"rotation":false}},
                "minecraft:emitter_rate_instant":{{"num_particles":1}},
                "minecraft:emitter_lifetime_once":{{"active_time":3}},
                "minecraft:emitter_shape_point":{{}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":3}},
                "minecraft:particle_motion_parametric":{{"relative_position":["q.entity_size","v.particle_age * 10",1]}},
                {BILLBOARD},"cobblemon:emitter_space":{{"scaling":"entity"}}"#
            ),
        );
        let body = Body {
            width: 0.5,
            height: 0.8,
            scale: 1.0,
            center: [0.0, 0.4, 0.0],
        };
        let locator = |x: f64| Frame::at([x, 70.0, 0.0]);
        // Turned a quarter about the vertical and twice the size.
        let turned = Frame::turned([0.0, 70.0, 0.0], 90.0, 2.0);
        let mut storms = Storms::default();
        storms.start(
            ring,
            Start {
                emitter: turned,
                attached: Some(locator(0.0)),
                tag: Some(Tag {
                    entity: 7,
                    locator: "middle".into(),
                    bound: true,
                }),
                body: Some(body),
                target: None,
                source: 9,
            },
            &none,
        );
        // Whoever started it finds its emitter again, which is not gone.
        assert!(
            storms
                .iter()
                .any(|storm| storm.source == 9 && !storm.despawned())
        );
        storms.tick(&none);
        storms.tick(&none);
        // Where its expressions put it, in the emitter's space: (0.8, 0, 1) scaled, +x turned to -z
        // and +z to +x.
        let at = drawn(&mut storms, 1.0)[0].center;
        assert!(
            close(at[0], 2.0) && close(at[1], 70.0) && close(at[2], -1.6),
            "{at:?}"
        );
        // The entity moves: the emitter and its particle with it, the particle a tick older. On the
        // tick of the move the particle is ahead by as much as the emitter moved, as in Cobblemon,
        // whose parametric motion measures from where the particle was before its origin moved.
        for storm in storms.iter_mut() {
            assert_eq!(storm.tag.as_ref().unwrap().entity, 7);
            storm.follow(Some(locator(4.0)), [0.0; 3], true, true);
        }
        storms.tick(&none);
        let at = drawn(&mut storms, 1.0)[0].center;
        assert!(
            close(at[0], 10.0) && close(at[1], 71.0) && close(at[2], -1.6),
            "{at:?}"
        );
        storms.tick(&none);
        let at = drawn(&mut storms, 1.0)[0].center;
        assert!(
            close(at[0], 6.0) && close(at[1], 72.0) && close(at[2], -1.6),
            "{at:?}"
        );
        // Unseen, its particles are too, for good; gone, it stops.
        for storm in storms.iter_mut() {
            storm.follow(None, [0.0; 3], true, false);
        }
        storms.tick(&none);
        assert!(drawn(&mut storms, 0.0).is_empty());
        for storm in storms.iter_mut() {
            storm.follow(None, [0.0; 3], false, true);
        }
        storms.tick(&none);
        assert!(drawn(&mut storms, 0.0).is_empty());
    }

    #[test]
    fn a_rotation_rate_is_all_but_lost_after_its_first_tick() {
        let spinning = effect(
            "test:spin",
            "",
            &format!(
                r#""minecraft:emitter_rate_instant":{{"num_particles":1}},
                "minecraft:emitter_lifetime_once":{{"active_time":1}},
                "minecraft:emitter_shape_point":{{}},
                "minecraft:particle_initial_spin":{{"rotation":90,"rotation_rate":400}},
                "minecraft:particle_lifetime_expression":{{"max_lifetime":1}},
                "minecraft:particle_motion_dynamic":{{}},{BILLBOARD}"#
            ),
        );
        let mut storms = Storms::default();
        storms.start(spinning, Start::at([0.0; 3]), &none);
        storms.tick(&none);
        assert_eq!(drawn(&mut storms, 0.0)[0].roll, -90.0);
        // 400 degrees a second is 20 a tick, which Cobblemon divides by twenty before each tick.
        storms.tick(&none);
        assert_eq!(drawn(&mut storms, 1.0)[0].roll, -91.0);
        storms.tick(&none);
        assert!((drawn(&mut storms, 1.0)[0].roll + 91.05).abs() < 1.0e-4);
    }

    #[test]
    fn rotations_take_one_direction_to_another() {
        let up = [0.0, 1.0, 0.0];
        for to in [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0], up] {
            let turned = turn(&turn_to(up, to), up);
            assert!(length(sub(turned, to)) < 1.0e-9, "{to:?}: {turned:?}");
        }
        let frame = Frame::turned([1.0, 2.0, 3.0], 90.0, 2.0);
        // `Axis.YP`: a quarter turn takes +z to +x.
        assert!(length(sub(frame.direction([0.0, 0.0, 1.0]), [1.0, 0.0, 0.0])) < 1.0e-9);
        assert!(length(sub(frame.position([0.0, 1.0, 1.0], true), [3.0, 4.0, 3.0])) < 1.0e-9);
        assert!(length(sub(frame.into_local([2.0, 0.0, 0.0]), [0.0, 0.0, 1.0])) < 1.0e-9);
    }

    #[test]
    fn the_curves_are_cobblemon_s() {
        let random = Cell::new(1);
        let value = |kind: CurveKind, input: f64| {
            let curve = Curve {
                slot: 0,
                input: Expression::number(0.0),
                kind,
            };
            curve_value(&curve, input, &[], &random)
        };
        let range = || Expression::number(2.0);
        let linear = |input| {
            value(
                CurveKind::Linear {
                    range: range(),
                    nodes: vec![0.0, 4.0, 2.0],
                },
                input,
            )
        };
        assert_eq!(
            (linear(-1.0), linear(0.5), linear(1.5), linear(9.0)),
            (0.0, 2.0, 3.0, 2.0)
        );
        // A Catmull-Rom curve goes through its inner nodes: its ends only shape it.
        let catmull = |input| {
            value(
                CurveKind::CatmullRom {
                    range: range(),
                    nodes: vec![9.0, 1.0, 3.0, 2.0, 9.0],
                },
                input,
            )
        };
        assert!(close(catmull(0.0), 1.0) && close(catmull(1.0), 3.0) && close(catmull(2.0), 2.0));
        let bezier = |input| {
            value(
                CurveKind::Bezier {
                    range: range(),
                    nodes: [0.0, 0.0, 1.0, 1.0],
                },
                input,
            )
        };
        assert!(close(bezier(0.0), 0.0) && close(bezier(1.0), 0.5) && close(bezier(2.0), 1.0));
        let chain = |input| {
            value(
                CurveKind::BezierChain(vec![
                    (0.0, 0.5, [0.0, 0.0, 1.0, 1.0]),
                    (0.5, 1.0, [1.0, 1.0, 0.0, 0.0]),
                ]),
                input,
            )
        };
        assert!(close(chain(0.25), 0.5) && close(chain(0.5), 1.0) && close(chain(7.0), 0.0));
    }
}
