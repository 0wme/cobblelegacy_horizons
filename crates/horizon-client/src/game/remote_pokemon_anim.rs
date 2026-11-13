//! A Pokémon's animation (CobbleLegacy's fork): its poser's pose for what it is doing, run each draw.
//!
//! Cobblemon's `PosableModel` picks one pose of the species' poser file from the entity's `PoseType`,
//! applies the pose's `transformedParts`, then runs the pose's animations on the model at rest: Bedrock
//! animations (`q.bedrock('pikachu', 'ground_idle')`, sampled at the entity's age) and a few written in
//! code (`q.look`, `q.quadruped_walk`...). `cobblelegacy_cobblemon` reads the files and does the math;
//! this binds them to the parts the mob renderer poses.
//!
//! The same object says what the model is drawn with as time goes: the frame of a texture that has
//! several, and the layers Cobblemon draws over the model (`PosableModel.render`: the model once more
//! for each `ModelLayer` of its look, under that layer's texture).
//!
//! Not here yet: quirks (blinks), the blend from one pose into the next, named animations (cries,
//! attacks), battle poses, and conditions on the world around the Pokémon (dusk, rain); a layer's
//! `tint` and `scrolling`, and frames blended into one another (`interpolation`), which no Pokémon
//! file of Cobblemon 1.8.1 has.
use cobblelegacy_cobblemon::{
    animation::AnimationFile,
    molang::{Context, Expression},
    pose::Delta,
    poser::{self, Poser},
    resolver::Texture,
    wire::PoseType,
};
use horizon_client::mobs::{
    Mob,
    anim::{Poses, State, part_key},
    own::{self, OwnAnimation},
    render::{self, Layer},
};
use std::sync::Arc;

/// What `Mob::extra.variant` holds for a Pokémon: its pose (the one it appeared in, then the one the
/// server last said), and above it whether the server says it moves (`PokemonEntity.MOVING`).
pub(super) const MOVING: u8 = 0x80;
/// Set once the server said the pose after the Pokémon appeared (`PokemonEntity.POSE_TYPE`).
const SAID: u8 = 0x40;

/// `variant` once the server says the Pokémon is in `pose`: asleep, taking off, in the water.
pub(super) fn with_pose(variant: u8, pose: PoseType) -> u8 {
    (variant & MOVING) | SAID | pose as u8
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
        .get(usize::from(variant & !(MOVING | SAID)))
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

/// A pose of the poser, ready to run.
struct Program {
    /// `transformedParts`: the part, what is added to its pose, and whether it is hidden.
    parts: Vec<(u64, Delta, bool)>,
    /// The animations in the pose's order, each with the condition it runs under.
    steps: Vec<(Option<Expression>, Step)>,
}

pub(super) struct Animator {
    /// A program per pose of the poser, in the file's order.
    programs: Vec<Program>,
    /// The program of each `PoseType` (by ordinal), for a wild Pokémon out of battle.
    by_pose: [Option<usize>; 14],
    /// The model's own texture when it has several frames, by their sprites in the atlas.
    skin: Option<Texture>,
    /// What is drawn over the model, in the look's order.
    overlays: Vec<Overlay>,
}

/// One layer of a look (`resolver::Layer`), ready to draw.
pub(super) struct Overlay {
    /// The layer's texture, by its frames' sprites in the atlas.
    pub(super) texture: Texture,
    /// Drawn at full brightness and without shading, whatever the light on the Pokémon.
    pub(super) emissive: bool,
    /// Its texels blend with what is behind them; else each is there whole or not at all.
    pub(super) translucent: bool,
}

/// `PosableState.animationSeconds` for an age in ticks, which is what a texture's frames go by
/// (`AnimatedModelTextureSupplier`).
fn seconds(age: f32) -> f32 {
    age / 20.0
}

/// A MoLang source's value with nothing known of the entity: what a pose's conditions and a part's
/// visibility are asked once, as the model is built. 0 for one that does not parse.
fn value(source: &str) -> f64 {
    Expression::parse(source).map_or(0.0, |e| f64::from(e.eval(&Context::default())))
}

impl Animator {
    /// `load` reads a Bedrock animation group (`pikachu` for `pikachu.animation.json`).
    pub(super) fn new(
        poser: &Poser,
        load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>,
    ) -> Self {
        let programs = poser
            .poses
            .iter()
            .map(|pose| Program {
                // The poser's own `transformedParts` first, twice: `PosableModel.applyAnimations` runs
                // them once itself and once more through `setDefault`, and their deltas add up.
                parts: poser
                    .transformed_parts
                    .iter()
                    .chain(&poser.transformed_parts)
                    .chain(&pose.transformed_parts)
                    .map(|t| {
                        let hidden = t.visible.as_ref().is_some_and(|v| !v.get(&mut value));
                        (part_key(&t.part), t.delta, hidden)
                    })
                    .collect(),
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
        let mut by_pose = [None; 14];
        for (ordinal, &pose) in PoseType::ALL.iter().enumerate() {
            let water = matches!(pose, PoseType::Swim | PoseType::Float);
            let entity = poser::Entity {
                pose,
                battle: false,
                touching_water: water,
                under_water: water,
                in_water_or_rain: water,
                dusk: false,
                ride_style: None,
            };
            by_pose[ordinal] = poser
                .pose_for(&entity, &mut value)
                .and_then(|chosen| poser.poses.iter().position(|p| std::ptr::eq(p, chosen)));
        }
        Self {
            programs,
            by_pose,
            skin: None,
            overlays: Vec::new(),
        }
    }

    /// A model no poser poses: it stays at rest.
    pub(super) fn still() -> Self {
        Self {
            programs: Vec::new(),
            by_pose: [None; 14],
            skin: None,
            overlays: Vec::new(),
        }
    }

    /// The same, drawn with `skin` when the model's own texture has several frames, and with
    /// `overlays` over it.
    pub(super) fn dressed(mut self, skin: Option<Texture>, overlays: Vec<Overlay>) -> Self {
        (self.skin, self.overlays) = (skin, overlays);
        self
    }

    /// The place in the poser's poses of the pose of each `PoseType`, by ordinal: what else is told
    /// by pose (a cry's sound, `remote_pokemon_sounds.rs`) is told by the pose the model is drawn in.
    pub(super) fn by_pose(&self) -> [Option<usize>; 14] {
        self.by_pose
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
        self.pose(mob, state, poses);
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
            // it is translucent or not; of the others, only a translucent one blends.
            let model = if overlay.translucent && !overlay.emissive {
                own::TRANSLUCENT_LAYER
            } else {
                own::LAYER
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

    /// The model's parts in the pose the Pokémon is in, its animations run to the mob's age.
    fn pose(&self, mob: &Mob, state: &State, poses: &mut Poses) {
        let pose = pose_of(mob.extra.variant);
        let Some(program) = self.by_pose[pose as usize].and_then(|i| self.programs.get(i)) else {
            return;
        };
        for &(part, delta, hidden) in &program.parts {
            if let Some(pose) = poses.by_key(part) {
                delta.apply(pose);
            }
            if hidden {
                poses.hide_key(part);
            }
        }
        // `PosableState.animationSeconds`: the entity's age, with the partial tick.
        let seconds = state.age / 20.0;
        let context = Context {
            anim_time: seconds,
            age_in_ticks: state.age,
            limb_swing: state.pos,
            limb_swing_amount: state.speed,
        };
        let motion = poser::Motion {
            limb_swing: state.pos,
            limb_swing_amount: state.speed,
            age_in_ticks: state.age,
            animation_seconds: seconds,
            head_yaw: state.y_rot,
            head_pitch: state.x_rot,
            velocity: [mob.velocity.x, mob.velocity.y, mob.velocity.z],
            ridden: false,
            intensity: 1.0,
        };
        // Cobblemon keeps one per entity from frame to frame, to ease a tilt back; one per draw here.
        let mut tilt = poser::Tilt::default();
        for (condition, step) in &program.steps {
            if condition.as_ref().is_some_and(|c| c.eval(&context) == 0.0) {
                continue;
            }
            match step {
                Step::Bedrock { file, index, parts } => {
                    file.animations[*index].sample(seconds, &context, &mut |bone, delta| {
                        if let Some(pose) = poses.by_key(parts[bone]) {
                            delta.apply(pose);
                        }
                    });
                }
                Step::Coded(animation) => {
                    animation.apply(&motion, &mut tilt, &mut |part, delta| {
                        if let Some(pose) = poses.by_key(part_key(part)) {
                            delta.apply(pose);
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PoseType::*;

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
    }

    /// Ponyta's mane: four frames at ten a second, and eyes that glow whatever the hour.
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
                },
                Overlay {
                    texture: Texture::Static("ponyta|5".into()),
                    emissive: false,
                    translucent: true,
                },
                Overlay {
                    texture: Texture::Static("ponyta|6".into()),
                    emissive: false,
                    translucent: false,
                },
                Overlay {
                    texture: Texture::Static("ponyta|7".into()),
                    emissive: true,
                    translucent: true,
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
        // translucent one alone is on the key that blends.
        assert_eq!(
            drawn,
            [
                (own::LAYER, "ponyta|2", true),
                (own::TRANSLUCENT_LAYER, "ponyta|5", false),
                (own::LAYER, "ponyta|6", false),
                (own::LAYER, "ponyta|7", true),
            ]
        );
        // Each in the colour it is given (the model's own), none scrolling.
        assert!(
            layers
                .iter()
                .all(|l| l.color == 0xFF11_2233 && l.scroll == 0.0)
        );
        // Nothing is drawn over a model without layers.
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
