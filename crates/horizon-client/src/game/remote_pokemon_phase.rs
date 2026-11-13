//! A Pokémon coming out of its Ball and going back into it (CobbleLegacy's fork).
//!
//! Cobblemon's server sends nothing of its own for either. It sets two of the Pokémon's synced fields,
//! `BEAM_MODE` (1 thrown out in a Ball, 2 appearing where it stands, 3 called back) and
//! `PHASING_TARGET_ID` (who threw the Ball, whom the beam goes back to), and its client draws the rest from
//! them (`PokemonClientDelegate`, `PokemonRenderer.renderTransition`): the Ball flying from the trainer's
//! hand, the Pokémon growing where it lands, then the red beam and the Pokémon shrinking into it.
//!
//! Cobblemon runs these on three clocks (the frame's, the entity's ticks, the wall's); one is kept here,
//! the wall's, and the Ball's arc does not depend on the frame rate as it does there. Its particles (the
//! flash as the Ball opens, the sparks) are drawn by `remote_pokemon_particles.rs` and its own sounds
//! played by `remote_pokemon_sounds.rs`, both asked for from here at Cobblemon's moments.
use super::super::super::Game;
use super::{Assets, GROUND, STAND_IN, fit, part, path_of, pokemon_type};
use cobblelegacy_cobblemon::geo;
use glam::{DVec3, Mat4};
use horizon_client::mesher::Vertex;
use horizon_client::mobs::{Anim, Mob, MobType, Part};
use horizon_core::geometry::Vec3;
use horizon_protocol::entity_data::Value;
use std::{collections::HashMap, sync::Arc, time::Instant};

/// `PokemonClientDelegate.POKEBALL_AIR_TIME`: the Ball's flight, which the Pokémon waits out unseen.
const AIR: f32 = 0.5;
/// `BEAM_SHRINK_TIME`: growing where the Ball landed, and shrinking into the beam.
const RESIZE: f32 = 0.4;
/// `BEAM_EXTEND_TIME`: the beam reaching the Pokémon, and drawing back once it is in.
const EXTEND: f32 = 0.2;
/// What stands for no size at all: `Extra::model_scale` reads 0 as "none was given".
const UNSEEN: f32 = 1.0e-4;
/// `PokemonRenderer.drawPokeBall`: the Ball's model at 0.7 of its size.
const BALL_SCALE: f32 = 0.7;
/// The Ball turns about its middle: 0.2 up its model, at that size.
const BALL_MIDDLE: f32 = 0.14;
/// The Ball's arc: the point it left climbs 0.04 blocks a frame in Cobblemon, what 60 frames a second
/// make of it.
const RISE: f64 = 2.4;
/// `PokemonRenderer.renderBeam`: no beam to a trainer farther than this.
const BEAM_REACH: f32 = 20.0;
/// The Ball of a Pokémon that names none.
const POKE_BALL: &str = "poke_ball";

/// `BEAM_MODE`, but for 0: nothing under way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Thrown out in its Ball.
    Thrown,
    /// Appearing where it stands: put down from a pasture, breaking out of a Ball that failed.
    Appearing,
    /// Called back, caught, or taken away once it fainted.
    Recalled,
}

impl Mode {
    fn of(beam_mode: i8) -> Option<Self> {
        match beam_mode {
            1 => Some(Self::Thrown),
            2 => Some(Self::Appearing),
            3 => Some(Self::Recalled),
            _ => None,
        }
    }

    /// When a Pokémon coming out is whole. One called back stays in until the server removes it.
    fn whole_at(self) -> Option<f32> {
        match self {
            Self::Thrown => Some(AIR + RESIZE),
            Self::Appearing => Some(RESIZE),
            Self::Recalled => None,
        }
    }
}

/// `activeSendoutScale`: how much of its size a Pokémon has, `seconds` into `mode`. Every change is
/// linear, as Cobblemon's are.
fn size(mode: Mode, seconds: f32) -> f32 {
    match mode {
        Mode::Thrown => ((seconds - AIR) / RESIZE).clamp(0.0, 1.0),
        Mode::Appearing => (seconds / RESIZE).clamp(0.0, 1.0),
        Mode::Recalled => 1.0 - ((seconds - EXTEND) / RESIZE).clamp(0.0, 1.0),
    }
}

/// The green and blue a Pokémon called back keeps, its red whole: down to 0.4 as the beam takes it.
fn tint(seconds: f32) -> f32 {
    1.0 - ((seconds - EXTEND) / RESIZE).clamp(0.0, 0.6)
}

/// How much of the way from the trainer to the Pokémon the beam covers: out, held while the Pokémon
/// shrinks, then back.
fn reach(seconds: f32) -> f32 {
    if seconds < EXTEND {
        seconds / EXTEND
    } else if seconds < EXTEND + RESIZE {
        1.0
    } else {
        1.0 - ((seconds - EXTEND - RESIZE) / EXTEND).min(1.0)
    }
}

/// The hand a Ball leaves and a beam goes back to, from the trainer's eyes and the way they face: below
/// them and to the right, nearer the eyes for the player whose view this is.
fn hand(eye: DVec3, yaw: f32, own: bool) -> DVec3 {
    let (down, aside) = if own { (0.4, 0.3) } else { (0.7, 0.4) };
    let yaw = f64::from(yaw).to_radians();
    // The look turned a quarter about the vertical: the unit vector to the left.
    eye - DVec3::new(aside * yaw.cos(), down, aside * yaw.sin())
}

/// The Ball `seconds` after it left `source` for the Pokémon's `feet`: where its base is, and how far it
/// has tumbled (radians), a turn for every four blocks of the throw.
fn flight(source: DVec3, feet: DVec3, seconds: f32) -> (DVec3, f32) {
    let done = f64::from((seconds / AIR).clamp(0.0, 1.0));
    let from = source + DVec3::Y * RISE * f64::from(seconds);
    let turns = (from.distance(feet) / 4.0).ceil();
    (
        feet + (from - feet) * (1.0 - done),
        (done * turns * std::f64::consts::TAU) as f32,
    )
}

/// One Pokémon coming out or going in.
struct Phase {
    mode: Mode,
    since: Instant,
    /// Thrown: where the Ball left its thrower's hand, from the first frame the thrower is known.
    source: Option<DVec3>,
    /// The Ball it is thrown in, as its textures name it (`poke_ball`).
    ball: String,
    /// Whole again: nothing left to draw, and the entry only remembers the mode until the server takes
    /// it back (it sends a Pokémon's whole data after its spawn, the mode with it:
    /// `PokemonClientDelegate.ballDone`).
    over: bool,
    /// The server took the mode back while the Pokémon was still growing: the entry goes when it is
    /// whole.
    released: bool,
}

/// The Pokémon coming out and going in, and the Balls that draw them.
#[derive(Default)]
pub(super) struct Phases {
    of: HashMap<i32, Phase>,
    /// `PHASING_TARGET_ID`, for the Pokémon that have one.
    targets: HashMap<i32, i32>,
    /// A Ball's look, made the first time one is thrown. `None`: it does not load.
    balls: HashMap<String, Option<Arc<MobType>>>,
}

impl Phases {
    pub(super) fn forget(&mut self, id: i32) {
        self.of.remove(&id);
        self.targets.remove(&id);
    }

    /// `PHASING_TARGET_ID`, -1 for none.
    pub(super) fn target(&mut self, id: i32, target: i32) {
        if target == -1 {
            self.targets.remove(&id);
        } else {
            self.targets.insert(id, target);
        }
    }

    /// What of the data vanilla's readers know is read here, at a 1.21.1 server's index.
    pub(super) fn vanilla_data(&mut self, id: i32, values: &[(u8, i32, Value)]) {
        for (index, _, value) in values {
            if let (26, Value::Int(target)) = (*index, value) {
                self.target(id, *target);
            }
        }
    }

    /// The textures of the Balls in flight, which keep their slots in the atlas.
    pub(super) fn textures(&self) -> impl Iterator<Item = String> + '_ {
        self.of
            .values()
            .filter(|p| p.mode == Mode::Thrown && !p.over)
            .filter_map(|p| self.balls.get(&p.ball)?.as_ref())
            .map(|kind| kind.texture.clone())
    }

    /// Whether Pokémon `id` is in its Ball still, or back in it: nothing of it shows, its label neither.
    pub(super) fn unseen(&self, id: i32) -> bool {
        self.of
            .get(&id)
            .is_some_and(|p| !p.over && size(p.mode, p.since.elapsed().as_secs_f32()) <= 0.0)
    }

    /// The look of Ball `ball` (`poke_ball`), once [`Game::pokemon_ball`] made it.
    pub(super) fn ball(&self, ball: &str) -> Option<&Arc<MobType>> {
        self.balls.get(ball)?.as_ref()
    }

    /// The textures of the Balls `names`, thrown at Pokémon to catch them: they keep their slots too.
    pub(super) fn ball_textures<'a>(
        &'a self,
        names: impl Iterator<Item = &'a str> + 'a,
    ) -> impl Iterator<Item = String> + 'a {
        names
            .filter_map(|name| self.ball(name))
            .map(|kind| kind.texture.clone())
    }

    /// `texture` lost its slot in the atlas: the Ball that wore it is made again when next thrown.
    pub(super) fn forget_texture(&mut self, texture: &str) {
        self.balls
            .retain(|_, kind| kind.as_ref().is_none_or(|k| k.texture != texture));
    }

    /// The atlas is a new one: every Ball is made again when next thrown.
    pub(super) fn forget_textures(&mut self) {
        self.balls.clear();
    }
}

/// `RenderHelper.renderBeaconBeam` as `PokemonRenderer.renderBeam` calls it: a thin core that turns on
/// itself and a wider glow around it, both the recall's red, from `from` towards `to` (world minus
/// camera) over `part` of the way.
fn beam(
    from: glam::Vec3,
    to: glam::Vec3,
    part: f32,
    seconds: f32,
    uv: [f32; 2],
    out: &mut Vec<Vertex>,
) {
    let along = to - from;
    let length = along.length();
    if part <= 0.0 || length > BEAM_REACH || length < 1.0e-3 {
        return;
    }
    let axis = along / length;
    // Any two axes square to the beam: the core turns and the glow is square.
    let side = axis
        .cross(glam::Vec3::Y)
        .try_normalize()
        .unwrap_or(glam::Vec3::X);
    let over = side.cross(axis);
    let end = from + axis * length * part;
    let mut prism = |radius: f32, turned: f32, colour: u32| {
        let corner = |i: usize| {
            let angle = turned + i as f32 * std::f32::consts::FRAC_PI_2;
            (side * angle.cos() + over * angle.sin()) * radius
        };
        for i in 0..4 {
            let (a, b) = (corner(i), corner(i + 1));
            for pos in [from + a, end + a, end + b, from + b] {
                out.push(Vertex {
                    pos: pos.to_array(),
                    uv,
                    layer: 0,
                    color: colour,
                    light: 240 | 240 << 8,
                    normal: [0; 4],
                    glint_uv: [0.0, 0.0],
                });
            }
        }
    };
    // 2.25 degrees a tick, over 40 ticks, from 45 degrees back.
    let turned = ((seconds * 20.0).rem_euclid(40.0) * 2.25 - 45.0).to_radians();
    prism(0.03, turned, 0xFF1A_1AFF);
    // The glow's square has its corners at (±0.07, ±0.07).
    prism(
        0.07 * std::f32::consts::SQRT_2,
        std::f32::consts::FRAC_PI_4,
        0x661A_1AFF,
    );
}

impl Game<'_> {
    /// `PokemonClientDelegate.onSyncedDataUpdated` for `BEAM_MODE`: Pokémon `id` starts coming out or
    /// going in, or is done with either.
    pub(super) fn pokemon_phase(&mut self, id: i32, beam_mode: i8) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let pokemon = &mut r.pokemon;
        let own = pokemon.seen.get(&id).map_or(1.0, |s| s.scale_modifier);
        let Some(mode) = Mode::of(beam_mode) else {
            // `PokemonClientDelegate` does nothing at 0, so a Pokémon coming out goes on growing: the
            // server says 0 a tenth of a second after one breaks out of a Ball. Its entry goes once
            // it is whole.
            if let Some(phase) = pokemon.phases.of.get_mut(&id)
                && phase.mode != Mode::Recalled
                && !phase.over
            {
                phase.released = true;
                return;
            }
            // Nothing under way any more: one called back is whole again.
            if pokemon.phases.of.remove(&id).is_some_and(|p| !p.over)
                && let Some(mob) = self.mob_index.get_mut(&mut self.mobs, id as u32)
            {
                (mob.extra.model_scale, mob.extra.variant2) = (own, 0);
            }
            return;
        };
        if pokemon.phases.of.get(&id).is_some_and(|p| p.mode == mode) {
            return;
        }
        let ball = pokemon
            .seen
            .get(&id)
            .map(|s| path_of(&s.caught_ball))
            .filter(|ball| !ball.is_empty())
            .unwrap_or(POKE_BALL)
            .to_owned();
        pokemon.phases.of.insert(
            id,
            Phase {
                mode,
                since: Instant::now(),
                source: None,
                ball,
                over: false,
                released: false,
            },
        );
        // What Cobblemon's client plays of its own as a Pokémon comes out (`remote_pokemon_sounds.rs`).
        match mode {
            Mode::Thrown => self.pokemon_thrown_sounds(id, AIR),
            Mode::Appearing => self.pokemon_appearing_sound(id),
            Mode::Recalled => {}
        }
        // At once, so that no frame shows a thrown Pokémon whole before its Ball has landed.
        self.pokemon_phase_frame();
    }

    /// The hand of entity `target`, the player's own or another trainer's.
    fn pokemon_phase_hand(&self, target: i32) -> Option<DVec3> {
        // A capture: the beam goes into the Ball that hit the Pokémon.
        if let Some(ball) = self.remote.as_ref()?.pokemon.captures.beam_end(target) {
            return Some(ball);
        }
        if target == self.remote.as_ref()?.entity_id {
            let [x, y, z] = self.eye_position()?;
            return Some(hand(DVec3::new(x, y, z), self.yaw, true));
        }
        let trainer = self.mobs.iter().find(|m| m.id == target as u32)?;
        let eye = DVec3::new(trainer.position.x, trainer.eye_y(), trainer.position.z);
        Some(hand(eye, trainer.head_yaw, false))
    }

    /// Once a frame: every Pokémon coming out or going in at the size and the red it has by now, its
    /// Ball's flight started and that Ball's look made.
    pub(in super::super::super) fn pokemon_phase_frame(&mut self) {
        let Some(r) = self.remote.as_ref() else {
            return;
        };
        let phases = &r.pokemon.phases;
        if phases.of.values().all(|p| p.over) {
            return;
        }
        let now = Instant::now();
        // The hands the Balls thrown since the last frame leave from.
        let hands: Vec<(i32, DVec3)> = phases
            .of
            .iter()
            .filter(|(_, p)| p.mode == Mode::Thrown && !p.over && p.source.is_none())
            .filter_map(|(id, _)| Some((*id, self.pokemon_phase_hand(*phases.targets.get(id)?)?)))
            .collect();
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let pokemon = &mut r.pokemon;
        let mut thrown = Vec::new();
        // The Balls that open, for their particles (`remote_pokemon_particles.rs`).
        let mut opening = Vec::new();
        for (&id, phase) in &mut pokemon.phases.of {
            if phase.over {
                continue;
            }
            if let Some((_, hand)) = hands.iter().find(|(of, _)| *of == id) {
                phase.source = Some(*hand);
                thrown.push(phase.ball.clone());
                opening.push((id, phase.ball.clone(), phase.since));
            }
            let seconds = now.duration_since(phase.since).as_secs_f32();
            let own = pokemon.seen.get(&id).map_or(1.0, |s| s.scale_modifier);
            phase.over = phase.mode.whole_at().is_some_and(|at| seconds >= at);
            let Some(mob) = self.mob_index.get_mut(&mut self.mobs, id as u32) else {
                continue;
            };
            // The Ball's trail is heard from where the Ball is.
            if let (Mode::Thrown, Some(source)) = (phase.mode, phase.source) {
                let feet = DVec3::new(mob.position.x, mob.position.y, mob.position.z);
                pokemon
                    .sounds
                    .ball_at(id, flight(source, feet, seconds).0.to_array());
            }
            mob.extra.model_scale = (own * size(phase.mode, seconds)).max(UNSEEN);
            // How far from white, which `mobs::render::body_color` reads for a Pokémon.
            mob.extra.variant2 = match phase.mode {
                Mode::Recalled => ((1.0 - tint(seconds)) * 255.0).round() as u8,
                _ => 0,
            };
        }
        pokemon
            .phases
            .of
            .retain(|_, phase| !(phase.over && phase.released));
        for ball in thrown {
            self.pokemon_ball(&ball);
        }
        for (id, ball, since) in opening {
            let opens = since + std::time::Duration::from_secs_f32(AIR);
            self.pokemon_particles_thrown(id, &ball, opens);
        }
    }

    /// The look of Ball `ball` (`poke_ball`), made the first time one is thrown.
    pub(super) fn pokemon_ball(&mut self, ball: &str) {
        let known = |game: &Self| {
            game.remote
                .as_ref()
                .is_none_or(|r| r.pokemon.phases.balls.contains_key(ball))
        };
        if known(self) {
            return;
        }
        let kind = self.pokemon_ball_type(ball);
        if kind.is_none() {
            log::debug!("Horizon: no model for the Ball {ball}: it is thrown unseen");
        }
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.phases.balls.insert(ball.to_owned(), kind);
        }
    }

    /// A Ball as a mob type, like a Pokémon's look: Cobblemon's Ball geometry (the old Balls of Hisui
    /// have their own) under the Ball's texture. It is shut, which is how it is drawn in flight, until
    /// a draw names animations for it (`remote_pokemon_capture.rs`).
    fn pokemon_ball_type(&mut self, ball: &str) -> Option<Arc<MobType>> {
        let template = self.res.mob_types().get(STAND_IN).cloned()?;
        let pokemon = &mut self.remote.as_mut()?.pokemon;
        let assets = pokemon.assets.get_or_insert_with(Assets::load).as_deref()?;
        let shape = if ball.starts_with("ancient_") {
            "ancient_poke_ball"
        } else {
            POKE_BALL
        };
        // The Ball's files are in memory by now, read ahead (`Captures::read_ahead`).
        let captures = &mut pokemon.captures;
        let model = captures.file(
            assets,
            &format!("bedrock/poke_balls/models/{shape}.geo.json"),
        )?;
        let geometry = geo::Geometry::parse(std::str::from_utf8(&model).ok()?).ok()?;
        let root = geometry.drawn_root(None, shape)?;
        let animations = captures.animator(assets, shape);
        let texture = format!("textures/poke_balls/{ball}.png");
        let png = captures.file(assets, &texture)?;
        let (width, height, rgba) = horizon_client::hud::decode_bytes(&png).ok()?;
        let body = Part {
            name: "#stand".into(),
            pose: [
                0.0, GROUND, 0.0, 0.0, 0.0, 0.0, BALL_SCALE, BALL_SCALE, BALL_SCALE,
            ],
            polygons: Vec::new(),
            children: vec![part(root, geometry.texture_size)],
        };
        let sprite = format!("cobblemon/{texture}");
        self.pokemon_skin(&sprite, fit(width, height, rgba))?;
        let mut kind = pokemon_type(&template, "cobblemon:poke_ball");
        kind.model = Arc::new(body);
        kind.anim = Anim::Own(animations);
        kind.texture = sprite;
        Some(Arc::new(kind))
    }

    /// `PokemonRenderer.renderTransition`: the Balls in flight and the beams of the Pokémon called back,
    /// among the frame's mobs.
    pub(in super::super::super) fn pokemon_phase_geometry(
        &self,
        camera: DVec3,
        alpha: f64,
        out: &mut Vec<Vertex>,
    ) {
        // The Balls thrown at Pokémon (`remote_pokemon_capture.rs`).
        self.pokemon_capture_geometry(camera, alpha, out);
        let Some(r) = self.remote.as_ref() else {
            return;
        };
        let phases = &r.pokemon.phases;
        if phases.of.values().all(|p| p.over) {
            return;
        }
        let now = Instant::now();
        let atlas = &self.gui.atlas;
        let lookup = |name: &str| atlas.sprites.get(name).map(|r| [r.u0, r.v0, r.u1, r.v1]);
        // The white texel the HUD's flat fills sample: the beam is one colour.
        let white = lookup("horizon/white").map_or([0.0; 2], |[u0, v0, u1, v1]| {
            [(u0 + u1) / 2.0, (v0 + v1) / 2.0]
        });
        for (&id, phase) in &phases.of {
            if phase.over {
                continue;
            }
            let seconds = now.duration_since(phase.since).as_secs_f32();
            let Some(mob) = self.mobs.iter().find(|m| m.id == id as u32) else {
                continue;
            };
            let feet = DVec3::new(mob.position.x, mob.position.y, mob.position.z);
            match phase.mode {
                Mode::Thrown if seconds < AIR => {
                    let (Some(source), Some(Some(kind))) =
                        (phase.source, phases.balls.get(&phase.ball))
                    else {
                        continue;
                    };
                    let (at, tumbled) = flight(source, feet, seconds);
                    // Turned along the throw, as seen from above.
                    let back = source - feet;
                    let facing = (std::f64::consts::PI * 1.5 - back.z.atan2(back.x)) as f32;
                    let mut ball =
                        Mob::new(u32::MAX, kind.clone(), Vec3::new(at.x, at.y, at.z), 0.0);
                    ball.previous = ball.position;
                    (ball.yaw, ball.previous_yaw) = (0.0, 0.0);
                    (ball.body_yaw, ball.previous_body_yaw) = (0.0, 0.0);
                    ball.extra.model_scale = 1.0;
                    let origin = Mat4::from_translation((at - camera).as_vec3())
                        * Mat4::from_rotation_y(facing)
                        * Mat4::from_translation(glam::Vec3::Y * BALL_MIDDLE)
                        * Mat4::from_rotation_x(-tumbled)
                        * Mat4::from_translation(glam::Vec3::Y * -BALL_MIDDLE);
                    // Lit as the Pokémon it lands on.
                    let light = self.world.light(
                        feet.x.floor() as i32,
                        feet.y.floor() as i32,
                        feet.z.floor() as i32,
                    );
                    let packed = (u32::from(light >> 4) * 16) | ((u32::from(light & 15) * 16) << 8);
                    horizon_client::mobs::render::render_posed(
                        &ball,
                        alpha as f32,
                        origin,
                        &lookup,
                        packed,
                        self.res.mob_data(),
                        out,
                    );
                }
                Mode::Recalled if seconds < EXTEND + RESIZE + EXTEND => {
                    let Some(hand) = phases
                        .targets
                        .get(&id)
                        .and_then(|&target| self.pokemon_phase_hand(target))
                    else {
                        continue;
                    };
                    // The Pokémon's middle, which sinks to its feet as it shrinks (its height follows
                    // its size).
                    let middle = feet + DVec3::Y * f64::from(mob.height()) / 2.0;
                    beam(
                        (hand - camera).as_vec3(),
                        (middle - camera).as_vec3(),
                        reach(seconds),
                        seconds,
                        white,
                        out,
                    );
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thrown_pokemon_waits_for_its_ball_then_grows() {
        assert_eq!(size(Mode::Thrown, 0.0), 0.0);
        assert_eq!(size(Mode::Thrown, 0.49), 0.0);
        assert!((size(Mode::Thrown, 0.7) - 0.5).abs() < 1.0e-5);
        assert_eq!(size(Mode::Thrown, 1.0), 1.0);
        assert_eq!(Mode::Thrown.whole_at(), Some(AIR + RESIZE));
        // One that appears where it stands starts growing at once.
        assert!((size(Mode::Appearing, 0.1) - 0.25).abs() < 1.0e-5);
        assert_eq!(size(Mode::Appearing, 2.0), 1.0);
    }

    #[test]
    fn a_recall_reaches_out_shrinks_the_pokemon_red_and_draws_back() {
        // The beam on its way: the Pokémon whole and its own colour.
        assert_eq!((size(Mode::Recalled, 0.1), tint(0.1)), (1.0, 1.0));
        assert!((reach(0.1) - 0.5).abs() < 1.0e-5);
        // Held while it shrinks; the red stops at 0.4 of the green and blue.
        assert!((size(Mode::Recalled, 0.4) - 0.5).abs() < 1.0e-5);
        assert!((tint(0.4) - 0.5).abs() < 1.0e-5);
        assert_eq!(reach(0.4), 1.0);
        assert!((tint(0.6) - 0.4).abs() < 1.0e-5);
        // Gone, the beam drawing back, and it never comes back out by itself.
        assert_eq!(size(Mode::Recalled, 0.7), 0.0);
        assert!((reach(0.7) - 0.5).abs() < 1.0e-5);
        assert_eq!((reach(0.9), size(Mode::Recalled, 30.0)), (0.0, 0.0));
        assert_eq!(Mode::Recalled.whole_at(), None);
        assert_eq!(Mode::of(0), None);
    }

    #[test]
    fn the_ball_flies_from_the_hand_to_the_feet() {
        // Facing south (yaw 0), east is to the left: the hand is to the west of the eyes, and lower.
        let own = hand(DVec3::new(0.0, 1.62, 0.0), 0.0, true);
        assert!((own - DVec3::new(-0.3, 1.22, 0.0)).length() < 1.0e-6);
        let other = hand(DVec3::new(0.0, 1.62, 0.0), 90.0, false);
        assert!((other - DVec3::new(0.0, 0.92, -0.4)).length() < 1.0e-6);
        let feet = DVec3::new(0.0, 0.0, 6.0);
        let (start, turned) = flight(own, feet, 0.0);
        assert!((start - own).length() < 1.0e-6);
        assert_eq!(turned, 0.0);
        // Half way, lifted by the arc; two turns for a throw of six blocks and some.
        let (middle, turned) = flight(own, feet, 0.25);
        assert!((middle.z - 3.0).abs() < 1.0e-6);
        assert!((middle.y - (1.22 + 0.6) / 2.0).abs() < 1.0e-6);
        assert!((turned - std::f32::consts::TAU).abs() < 1.0e-4);
        let (end, _) = flight(own, feet, 0.5);
        assert!((end - feet).length() < 1.0e-6);
    }

    #[test]
    fn a_beam_is_two_prisms_and_none_past_its_reach() {
        let mut out = Vec::new();
        beam(
            glam::Vec3::ZERO,
            glam::Vec3::new(0.0, 0.0, 4.0),
            0.5,
            0.0,
            [0.0; 2],
            &mut out,
        );
        // Four faces each, four corners a face.
        assert_eq!(out.len(), 32);
        // Half the way, no farther.
        let farthest = out.iter().map(|v| v.pos[2]).fold(0.0, f32::max);
        assert!((farthest - 2.0).abs() < 1.0e-5);
        out.clear();
        beam(
            glam::Vec3::ZERO,
            glam::Vec3::new(0.0, 0.0, 21.0),
            1.0,
            0.0,
            [0.0; 2],
            &mut out,
        );
        beam(
            glam::Vec3::ZERO,
            glam::Vec3::new(0.0, 0.0, 4.0),
            0.0,
            0.0,
            [0.0; 2],
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_target_is_kept_until_the_server_takes_it_back() {
        let mut phases = Phases::default();
        phases.vanilla_data(7, &[(26, 0, Value::Int(3))]);
        assert_eq!(phases.targets.get(&7), Some(&3));
        phases.vanilla_data(7, &[(26, 0, Value::Int(-1))]);
        assert!(phases.targets.is_empty());
        phases.target(7, 3);
        phases.forget(7);
        assert!(phases.targets.is_empty());
    }
}
