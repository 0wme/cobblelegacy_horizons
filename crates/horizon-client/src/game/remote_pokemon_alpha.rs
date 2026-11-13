//! The eyes of an alpha Pokémon (CobbleLegacy's fork): Cobblemon's `AlphaEyeRenderer`, and the
//! particles of the `alpha_eyes` aspect.
//!
//! A Pokémon that bears the alpha's mark has the aspect `alpha_eyes`. Its look gives it red eyes (a
//! layer of the model, `remote_pokemon_looks.rs`), and Cobblemon's client adds three things at each
//! locator of its model whose name has `eye` in it:
//!
//! - sparks, one tick in four, for a Pokémon within eight blocks of the player: the particle effect
//!   `cobblemon:alpha_eyes` (`CobblemonClient`'s tick, `spawnAspectParticle`);
//! - a red glow that faces the viewer, and shrinks to nothing as the eye turns away
//!   (`renderAlphaEyeBloom`);
//! - a red ribbon along where the eye was over the last 0.4 seconds, which shows while it moves
//!   (`renderEyeTrail`).
//!
//! The glow and the ribbon have no texture and add their colour to what is behind them, as the
//! dragon's death rays do: they are drawn with those.
//!
//! Not here: the other aspects' particles (honey, crumbs and nectar, which are Minecraft's own).
use super::super::super::Game;
use cobblelegacy_cobblemon::geo::LOCATOR_PREFIX;
use glam::{DVec3, Mat4, Vec3};
use horizon_client::{
    mesher::Vertex,
    mobs::{Mob, Part, Species, render},
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// The aspect the alpha's mark gives (`mark_alpha.json`).
const ALPHA_EYES: &str = "alpha_eyes";
/// `aspectParticleMap`: the effect of that aspect, and its chance each tick.
const SPARKS: &str = "cobblemon:alpha_eyes";
const SPARKS_CHANCE: f32 = 0.25;
/// `CobblemonClient`: aspects' particles are for the Pokémon in a box of 16 blocks around the player.
const SPARKS_REACH: f64 = 8.0;

/// `AlphaEyeRenderer`'s numbers.
const TARGET_SIZE: f32 = 3.0;
const TRAIL_MAX_WIDTH: f32 = 0.1;
const BLOOM_RADIUS: f32 = 0.35;
const TRAIL_MAX_ALPHA: f32 = 0.6;
const TRAIL_DURATION: Duration = Duration::from_millis(400);
/// `TRAIL_COLOR`, (0.7, 0, 0), as a vertex's colour without its alpha: red in the low byte.
const RED: u32 = 178;
/// The glow is five rings of twelve segments.
const RINGS: u32 = 5;
const SEGMENTS: u32 = 12;

/// Where an eye was, and when.
type Trail = VecDeque<(DVec3, Instant)>;

/// What is kept of the alphas in the level.
#[derive(Default)]
pub(super) struct Alphas {
    /// Where each eye of each alpha was over the last moments, by entity id and locator.
    trails: HashMap<i32, HashMap<String, Trail>>,
    /// The age each was last given its sparks at, in ticks.
    ticked: HashMap<i32, u32>,
    /// What the sparks' chance is drawn from.
    random: u32,
}

impl Alphas {
    /// Entity `id` is gone.
    pub(super) fn forget(&mut self, id: i32) {
        self.trails.remove(&id);
        self.ticked.remove(&id);
    }

    /// A number from 0 to 1, 1 excluded (xorshift).
    fn random(&mut self) -> f32 {
        if self.random == 0 {
            self.random = 0x9E37_79B9;
        }
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        (self.random >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// The locators of a model whose name has `eye` in it (`"eye" in it.lowercase()`), in the model's
/// order.
fn eye_locators(model: &Part, out: &mut Vec<String>) {
    if let Some(locator) = model.name.strip_prefix(LOCATOR_PREFIX)
        && locator.to_lowercase().contains("eye")
    {
        out.push(locator.to_owned());
    }
    for child in &model.children {
        eye_locators(child, out);
    }
}

/// `getHitboxScaling`: how much larger or smaller the glow and the ribbon are for a Pokémon whose
/// hitbox is `largest` blocks on its larger side, 3 blocks being what they are made for.
fn hitbox_scaling(largest: f32) -> f32 {
    (largest / TARGET_SIZE).sqrt()
}

/// Minecraft's `Vec3.normalize`: nothing for what is shorter than a hundred-thousandth.
fn unit(vector: Vec3) -> Vec3 {
    let length = vector.length();
    if length < 1.0e-5 {
        Vec3::ZERO
    } else {
        vector / length
    }
}

/// `renderAlphaEyeBloom` for one eye: the glow's triangles, each corner with its alpha. `eye` is
/// where the eye is from the viewer, `forward` the way it looks (the locator's -Z, as long as the
/// locator's matrix makes it), `up` the viewer's up.
///
/// The glow is a disc that faces the viewer, whole when the eye looks at them and gone once it
/// looks more than 120 degrees away; it also comes a tenth of a block towards the viewer. Its alpha
/// falls from 1 at the centre as the square of what is left to the rim.
fn bloom(
    eye: Vec3,
    forward: Vec3,
    up: Vec3,
    scaling: f32,
    triangle: &mut dyn FnMut([(Vec3, f32); 3]),
) {
    let to_viewer = unit(-eye);
    let side = unit(up.cross(to_viewer));
    let facing = (forward.dot(to_viewer) + 0.5).clamp(0.0, 1.0);
    let radius = BLOOM_RADIUS * facing * facing * scaling;
    if radius <= 0.0 {
        return;
    }
    let center = eye + to_viewer * (0.1 * facing);
    let around = |segment: u32| {
        let angle = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        side * angle.cos() + up * angle.sin()
    };
    for ring in 0..RINGS {
        let (inner, outer) = (ring as f32 / RINGS as f32, (ring + 1) as f32 / RINGS as f32);
        let (a0, a1) = ((1.0 - inner).powi(2), (1.0 - outer).powi(2));
        let (r0, r1) = (radius * inner, radius * outer);
        for segment in 0..SEGMENTS {
            let (d0, d1) = (around(segment), around(segment + 1));
            let (p00, p01) = (center + d0 * r0, center + d1 * r0);
            let (p10, p11) = (center + d0 * r1, center + d1 * r1);
            triangle([(p00, a0), (p10, a1), (p11, a1)]);
            triangle([(p00, a0), (p11, a1), (p01, a0)]);
        }
    }
}

/// `renderEyeTrail` for one eye: the ribbon's quads, each corner with its alpha. `positions` is
/// where the eye was from the viewer, the oldest first: the ribbon grows from nothing there to its
/// full width and alpha where the eye is now, and has no width where the eye did not move.
fn ribbon(positions: &[Vec3], scaling: f32, quad: &mut dyn FnMut([(Vec3, f32); 4])) {
    let count = positions.len();
    if count < 2 {
        return;
    }
    let width = TRAIL_MAX_WIDTH * scaling;
    let along = |index: usize| index as f32 / (count - 1) as f32;
    for index in 0..count - 1 {
        let (current, next) = (positions[index], positions[index + 1]);
        let (t0, t1) = (along(index), along(index + 1));
        // Across the ribbon: at right angles to its run and to the line of sight.
        let across = unit(unit(next - current).cross(unit(-current)));
        let (w0, w1) = (across * (width * t0), across * (width * t1));
        let (a0, a1) = (TRAIL_MAX_ALPHA * t0, TRAIL_MAX_ALPHA * t1);
        quad([
            (current - w0, a0),
            (current + w0, a0),
            (next + w1, a1),
            (next - w1, a1),
        ]);
    }
}

/// One corner of the glow or the ribbon as a vertex of the additive, untextured draws (the dragon's
/// rays): `uv` is a white texel's.
fn vertex(position: Vec3, alpha: f32, uv: [f32; 2]) -> Vertex {
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
    Vertex {
        normal: [0; 4],
        pos: position.to_array(),
        uv,
        layer: horizon_client::gfx::DRAGON_RAYS_OVERLAY
            | u32::from(horizon_client::mesher::BRIGHT) << 16,
        color: alpha << 24 | RED,
        light: 0,
        glint_uv: [0.0, 0.0],
    }
}

/// A quad seen from both of its sides: those draws show a face from its front only, and which side
/// that is turns with the viewer.
fn two_sided(corners: [(Vec3, f32); 4], uv: [f32; 2], out: &mut Vec<Vertex>) {
    let [a, b, c, d] = corners;
    for (position, alpha) in [a, b, c, d, d, c, b, a] {
        out.push(vertex(position, alpha, uv));
    }
}

impl Game<'_> {
    /// The alphas among the level's Pokémon that are seen: each with its entity id.
    fn pokemon_alphas(&self) -> Vec<(i32, &Mob)> {
        let Some(pokemon) = self.remote.as_ref().map(|r| &r.pokemon) else {
            return Vec::new();
        };
        let alpha = |id: i32| {
            let seen = pokemon.seen.get(&id);
            seen.is_some_and(|seen| seen.aspects.iter().any(|aspect| aspect == ALPHA_EYES))
        };
        let mobs = self.mobs.iter();
        mobs.filter(|mob| mob.species() == Species::Pokemon)
            .map(|mob| (mob.id as i32, mob))
            .filter(|(id, mob)| alpha(*id) && !mob.invisible && !pokemon.phases.unseen(*id))
            .collect()
    }

    /// The eyes of `mob` that show, from `camera`: each locator with where it is and the way it
    /// looks (`isLocatorVisible`: one an animation has scaled to nothing does not show).
    fn pokemon_eyes(&self, mob: &Mob, partial: f32, camera: DVec3) -> Vec<(String, Vec3, Vec3)> {
        let mut locators = Vec::new();
        eye_locators(&mob.kind.model, &mut locators);
        let data = self.res.mob_data();
        let found = locators.into_iter().filter_map(|locator| {
            let part = format!("{LOCATOR_PREFIX}{locator}");
            let matrix = render::part_transform(mob, partial, camera, data, &part)?;
            // `LocatorAccess.update`: the flip every model is drawn under is undone at a locator.
            let matrix = matrix * Mat4::from_rotation_z(std::f32::consts::PI);
            let shows = matrix.determinant().abs() > 1.0e-6 && matrix.is_finite();
            shows.then(|| {
                let at = matrix.transform_point3(Vec3::ZERO);
                (locator, at, matrix.transform_vector3(Vec3::NEG_Z))
            })
        });
        found.collect()
    }

    /// Once a frame: the sparks of the alphas near the player, one tick in four
    /// (`PokemonClientDelegate.spawnAspectParticle`, each client tick).
    pub(super) fn pokemon_alpha_frame(&mut self) {
        let alphas = self.pokemon_alphas();
        if alphas.is_empty() {
            return;
        }
        let Some([x, y, z]) = self.eye_position() else {
            return;
        };
        let player = DVec3::new(x, y, z);
        // Which eyes of which alpha get sparks in this frame, by the ticks gone by.
        let mut due: Vec<(i32, u32, Vec<String>)> = Vec::new();
        for (id, mob) in alphas {
            let feet = DVec3::new(mob.position.x, mob.position.y, mob.position.z);
            let silent = self.remote.as_ref().and_then(|r| r.entities.get(&id));
            if (feet - player).abs().max_element() > SPARKS_REACH
                || silent.is_some_and(|tracked| tracked.silent)
            {
                continue;
            }
            let eyes = self.pokemon_eyes(mob, 1.0, feet);
            due.push((id, mob.age, eyes.into_iter().map(|eye| eye.0).collect()));
        }
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        for (id, age, eyes) in due {
            // The ticks since it was last looked at: one for a Pokémon just seen, a few at most.
            let before = pokemon.alphas.ticked.insert(id, age);
            let ticks = before.map_or(1, |before| age.saturating_sub(before).min(4));
            for _ in 0..ticks {
                if pokemon.alphas.random() >= SPARKS_CHANCE {
                    continue;
                }
                for eye in &eyes {
                    pokemon.particles.ask_on(id, SPARKS, eye);
                }
            }
        }
    }

    /// `doAlphaEyeRendering`: the glow and the ribbon of each alpha's eyes, among the frame's
    /// additive draws. Where each eye is joins its trail as it is drawn, as in Cobblemon.
    pub(in super::super::super) fn pokemon_alpha_eye_geometry(
        &mut self,
        camera: DVec3,
        partial: f64,
        out: &mut Vec<Vertex>,
    ) {
        let now = Instant::now();
        let fresh = |trail: &mut Trail| {
            while trail
                .front()
                .is_some_and(|(_, at)| now.duration_since(*at) > TRAIL_DURATION)
            {
                trail.pop_front();
            }
        };
        // Each alpha's eyes, and how its size scales what is drawn at them.
        let mut eyes = Vec::new();
        for (id, mob) in self.pokemon_alphas() {
            let scale = self
                .remote
                .as_ref()
                .and_then(|r| r.pokemon.seen.get(&id))
                .map_or(1.0, |seen| seen.scale_modifier);
            let largest = mob.kind.width.max(mob.kind.height) as f32 * scale;
            let found = self.pokemon_eyes(mob, partial as f32, camera);
            eyes.push((id, hitbox_scaling(largest), found));
        }
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        // A Pokémon that is an alpha no more, or is not seen, has no trail.
        let trails = &mut pokemon.alphas.trails;
        trails.retain(|id, _| eyes.iter().any(|(alpha, ..)| alpha == id));
        if eyes.is_empty() {
            return;
        }
        let white = self.gui.atlas.font.white();
        let uv = [(white.u0 + white.u1) / 2.0, (white.v0 + white.v1) / 2.0];
        // `Camera.upVector`.
        let up = glam::Quat::from_rotation_y(std::f32::consts::PI - self.yaw.to_radians())
            * glam::Quat::from_rotation_x(-self.pitch.to_radians())
            * Vec3::Y;
        for (id, scaling, found) in eyes {
            let trails = trails.entry(id).or_default();
            // Said once its model is there: a Pokémon whose look is still being read has no eye.
            if super::anim::trace() && trails.is_empty() && !found.is_empty() {
                let names: Vec<&str> = found.iter().map(|eye| eye.0.as_str()).collect();
                log::info!("Horizon: Pokémon {id} has an alpha's eyes at {names:?}");
            }
            for (locator, at, forward) in found {
                let trail = trails.entry(locator).or_default();
                trail.push_back((camera + at.as_dvec3(), now));
                bloom(at, forward, up, scaling, &mut |[a, b, c]| {
                    two_sided([a, b, c, c], uv, out);
                });
            }
            for trail in trails.values_mut() {
                fresh(trail);
                let positions: Vec<Vec3> = trail
                    .iter()
                    .map(|(position, _)| (*position - camera).as_vec3())
                    .collect();
                ribbon(&positions, scaling, &mut |corners| {
                    two_sided(corners, uv, out);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str, children: Vec<Part>) -> Part {
        Part {
            name: name.into(),
            pose: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            polygons: Vec::new(),
            children,
        }
    }

    #[test]
    fn the_eyes_are_the_locators_named_so() {
        let model = part(
            "root",
            vec![
                part(
                    "head",
                    vec![
                        part("eye_left", Vec::new()),
                        part("internal_locator__eye_left", Vec::new()),
                        part("internal_locator__Eye_Right", Vec::new()),
                        part("internal_locator__item_face", Vec::new()),
                    ],
                ),
                part("internal_locator__third_eye", Vec::new()),
            ],
        );
        let mut found = Vec::new();
        eye_locators(&model, &mut found);
        // A bone of that name is no locator; the case does not matter.
        assert_eq!(found, ["eye_left", "Eye_Right", "third_eye"]);
    }

    #[test]
    fn the_glow_faces_the_viewer_and_fades_as_the_eye_turns_away() {
        let glow = |forward: Vec3, scaling: f32| {
            let mut triangles = Vec::new();
            // An eye four blocks in front of the viewer, who looks down -Z.
            let eye = Vec3::new(0.0, 0.0, -4.0);
            bloom(eye, forward, Vec3::Y, scaling, &mut |t| triangles.push(t));
            triangles
        };
        // The eye looks at the viewer: whole, five rings of twelve segments of two triangles.
        let whole = glow(Vec3::Z, 1.0);
        assert_eq!(whole.len(), 120);
        // A tenth of a block towards the viewer, flat across the line of sight, 0.35 in radius.
        let far = |triangles: &[[(Vec3, f32); 3]]| {
            let corners = triangles.iter().flatten();
            corners.fold(0.0f32, |far, (p, _)| far.max(p.truncate().length()))
        };
        assert!(
            whole
                .iter()
                .flatten()
                .all(|(p, _)| (p.z + 3.9).abs() < 1e-5)
        );
        assert!((far(&whole) - 0.35).abs() < 1e-5);
        // Its alpha is 1 at the centre and nothing at the rim, by the square of what is left.
        let (center, alpha) = whole[0][0];
        assert_eq!((center, alpha), (Vec3::new(0.0, 0.0, -3.9), 1.0));
        let rim = whole
            .iter()
            .flatten()
            .filter(|(p, _)| (p.truncate().length() - 0.35).abs() < 1e-5);
        assert!(rim.clone().count() > 0 && rim.clone().all(|(_, alpha)| *alpha == 0.0));
        let second = whole[24][0];
        assert!((second.1 - 0.64).abs() < 1e-6);
        // Every triangle is wound the same way, seen from the viewer (who is at +Z of it).
        let turns = |[a, b, c]: &[(Vec3, f32); 3]| (b.0 - a.0).cross(c.0 - a.0).z;
        assert!(whole.iter().skip(24).all(|t| turns(t) > 0.0));
        // Seen from the side it is a quarter of that across and half as far forward; from behind
        // (more than 120 degrees away) there is none.
        let side = glow(Vec3::X, 1.0);
        assert!((far(&side) - 0.35 * 0.25).abs() < 1e-5);
        assert!(
            side.iter()
                .flatten()
                .all(|(p, _)| (p.z + 3.95).abs() < 1e-5)
        );
        assert!(glow(Vec3::NEG_Z, 1.0).is_empty());
        // A larger Pokémon has a larger glow: by the root of its size over three blocks.
        assert_eq!(hitbox_scaling(3.0), 1.0);
        assert!((hitbox_scaling(0.75) - 0.5).abs() < 1e-6);
        assert!((far(&glow(Vec3::Z, 0.5)) - 0.175).abs() < 1e-5);
    }

    #[test]
    fn the_ribbon_follows_where_the_eye_was() {
        let run = |positions: &[Vec3]| {
            let mut quads = Vec::new();
            ribbon(positions, 1.0, &mut |quad| quads.push(quad));
            quads
        };
        // An eye that moved to the viewer's right, four blocks ahead: three places, two quads.
        let at = |x: f32| Vec3::new(x, 0.0, -4.0);
        let quads = run(&[at(0.0), at(0.5), at(1.0)]);
        assert_eq!(quads.len(), 2);
        // Nothing at its oldest end, half its width and alpha in the middle, all of them now.
        let [a, b, c, d] = quads[0];
        assert_eq!((a.0, b.0, a.1), (at(0.0), at(0.0), 0.0));
        assert!((c.1 - 0.3).abs() < 1e-6 && (d.1 - 0.3).abs() < 1e-6);
        assert!(((d.0 - c.0).length() - 0.1).abs() < 1e-5);
        let [_, _, c, d] = quads[1];
        assert!((c.1 - 0.6).abs() < 1e-6);
        assert!(((d.0 - c.0).length() - 0.2).abs() < 1e-5);
        // Across the run and the line of sight: up and down here.
        assert!((d.0 - c.0).normalize().dot(Vec3::Y).abs() > 0.99);
        // An eye that stays where it is has a ribbon of no width, and one place is no ribbon.
        let still = run(&[at(0.0), at(0.0), at(0.0)]);
        assert!(still.iter().flatten().all(|(p, _)| *p == at(0.0)));
        assert!(run(&[at(0.0)]).is_empty());
    }

    #[test]
    fn a_corner_is_red_by_its_alpha_and_seen_from_both_sides() {
        let corner = vertex(Vec3::new(1.0, 2.0, 3.0), 0.5, [0.25, 0.75]);
        assert_eq!(corner.color, 127 << 24 | 178);
        assert_eq!(
            corner.layer & 0xffff,
            horizon_client::gfx::DRAGON_RAYS_OVERLAY
        );
        assert_eq!((corner.pos, corner.uv), ([1.0, 2.0, 3.0], [0.25, 0.75]));
        let quad = [
            (Vec3::ZERO, 0.0),
            (Vec3::X, 0.25),
            (Vec3::ONE, 0.5),
            (Vec3::Y, 1.0),
        ];
        let mut out = Vec::new();
        two_sided(quad, [0.0, 0.0], &mut out);
        let places: Vec<[f32; 3]> = out.iter().map(|v| v.pos).collect();
        let (front, back) = places.split_at(4);
        assert_eq!(front, quad.map(|(p, _)| p.to_array()));
        assert!(front.iter().eq(back.iter().rev()));
    }
}
