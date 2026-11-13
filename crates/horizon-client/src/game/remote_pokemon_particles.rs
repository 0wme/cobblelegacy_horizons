//! Cobblemon's particle effects in the world (CobbleLegacy's fork).
//!
//! Cobblemon draws its own particles, which its sources call "Snowstorm": Bedrock's particle effects,
//! read from `assets/cobblemon/bedrock/particles` and played by its client. The flash and the sparks of
//! a Ball that opens are four of them, which `PokemonClientDelegate` starts by itself; the server asks
//! for others by name, at a place (`cobblemon:spawn_snowstorm_particle`) or on an entity
//! (`cobblemon:spawn_snowstorm_entity_particle`: the ring of a shiny Pokémon that comes out).
//!
//! The effects are read and run by `cobblelegacy_cobblemon` (`snowstorm`, `storm`). What is here is
//! what needs the game: the files, read on a thread of their own as the Pokémon's are; each effect's
//! texture, in one of the atlas's Pokémon slots while the effect plays; the clock (Cobblemon ticks its
//! particles twenty times a second and draws them between two ticks); the entity an effect is on; and
//! the quads, drawn with the level's translucent particles.
//!
//! A slot is 256 texels square and some sheets of frames are wider: such a sheet is cut into rows of
//! whole frames ([`pack`]), and halved first when even that does not fit.
//!
//! A third kind starts by itself: the effects the animations of a Pokémon's pose carry
//! (`particle_effects`: Gastly's gas, Slugma's bubbles of lava), which Cobblemon's `PosableState`
//! starts as the pose is taken and as the animations' time comes up to them ([`Emitter`]). Those of
//! the animations a server plays by name (cries, attacks) are not: such animations are not played.
//!
//! An effect on an entity is at one of its model's locators, where the model's poser has it now
//! ([`Game::pokemon_locator`]): the model's own, or the three Cobblemon gives every model.
//!
//! Not done: a keyframe's `pre_effect_script`; additive blending (an `add` effect is blended as the
//! others); and what `storm` leaves out.
use super::super::super::Game;
use super::{Assets, anim};
use cobblelegacy_cobblemon::{
    animation::{ParticleKeyframe, moments_passed},
    geo::LOCATOR_PREFIX,
    snowstorm::{
        Effect, Facing, Material, SPAWN_ENTITY_PARTICLE, SPAWN_PARTICLE, SpawnEntityParticle,
        SpawnParticle,
    },
    storm::{Body, Frame, Quad, Start, Storms, Tag},
};
use glam::{DVec3, Quat, Vec3};
use horizon_client::hud::POKEMON_SLOT_SIZE;
use horizon_client::mesher::Vertex;
use horizon_client::mobs::{Mob, MobType, Species, anim::State};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// A tick of Cobblemon's particles, in seconds.
const TICK: f64 = 0.05;
/// The ticks one frame may run: a frame that took longer leaves the rest behind.
const CATCH_UP: u32 = 4;
/// `PokemonClientDelegate`: the sparks start a sixtieth of a second after the flash.
const SPARKS_AFTER: Duration = Duration::from_micros(16_670);
/// And the sparkle 0.4 seconds after them, which is eight of the entity's ticks.
const SPARKLE_AFTER: Duration = Duration::from_millis(400);
/// How long an effect waits for the effects to be read before it is given up.
const PATIENCE: Duration = Duration::from_secs(5);

/// Whether each effect started is written to the log, for a test to read.
fn traced() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some())
}

/// What names a sheet in the atlas: the effect's texture, the size its effect says it has and the step
/// from one of its frames to the next, both in the effect's texels.
type SheetKey = (String, [u32; 2], [u32; 2]);

/// How a sheet lies in its slot.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The sheet's size once halved to fit.
    sheet: [u32; 2],
    /// What the slot holds of it: the sheet itself, or its strips one after the other.
    packed: [u32; 2],
    /// The sheet cut every so many texels, along x (`true`) or y, into strips laid side by side
    /// across the other axis.
    cut: Option<(bool, u32)>,
}

impl Layout {
    /// Where the part `uv` of the sheet (left, top, right, bottom, as fractions of it) is in what was
    /// packed, as fractions of that. `None` for a part that leaves the sheet, or that a cut goes
    /// through: Cobblemon would show its neighbours in the atlas there, which here are other things.
    fn place(&self, uv: [f32; 4]) -> Option<[f32; 4]> {
        const SLACK: f32 = 0.01;
        let [w, h] = self.sheet.map(|n| n as f32);
        let (mut x, mut y) = ([uv[0] * w, uv[2] * w], [uv[1] * h, uv[3] * h]);
        let within = |range: [f32; 2], size: f32| {
            range.iter().all(|at| (-SLACK..=size + SLACK).contains(at))
        };
        if !within(x, w) || !within(y, h) {
            return None;
        }
        if let Some((along_x, period)) = self.cut {
            let period = period as f32;
            let (cut, other, size) = if along_x {
                (&mut x, &mut y, h)
            } else {
                (&mut y, &mut x, w)
            };
            let strip = ((cut[0].min(cut[1]) + SLACK) / period).floor();
            *cut = cut.map(|at| at - strip * period);
            if cut.iter().any(|at| *at > period + SLACK) {
                return None;
            }
            *other = other.map(|at| at + strip * size);
        }
        let [pw, ph] = self.packed.map(|n| n as f32);
        Some([x[0] / pw, y[0] / ph, x[1] / pw, y[1] / ph])
    }
}

/// Every other texel of an image.
fn halve(width: u32, height: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let (w, h) = ((width / 2).max(1), (height / 2).max(1));
    let mut half = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let at = ((y * 2 * width + x * 2) * 4) as usize;
            half.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    (w, h, half)
}

/// An image made to fit a slot of the atlas, `slot` texels square. A sheet too long for it whose frames
/// are `step` apart is cut into strips of whole frames; what still does not fit is halved, and tried
/// again.
fn pack(
    mut width: u32,
    mut height: u32,
    mut rgba: Vec<u8>,
    mut step: [u32; 2],
    slot: u32,
) -> (Layout, Vec<u8>) {
    loop {
        if width <= slot && height <= slot {
            let whole = Layout {
                sheet: [width, height],
                packed: [width, height],
                cut: None,
            };
            return (whole, rgba);
        }
        // Along x for a sheet that is too wide, along y for one that is too high.
        for along_x in [true, false] {
            let (long, short, frame) = if along_x {
                (width, height, step[0])
            } else {
                (height, width, step[1])
            };
            if long <= slot || short > slot || frame == 0 || frame > slot {
                continue;
            }
            let period = slot / frame * frame;
            let strips = long.div_ceil(period);
            if strips * short > slot {
                continue;
            }
            let packed = if along_x {
                [period.min(long), strips * short]
            } else {
                [strips * short, period.min(long)]
            };
            let mut out = vec![0; (packed[0] * packed[1] * 4) as usize];
            for y in 0..height {
                for x in 0..width {
                    let (to_x, to_y) = if along_x {
                        (x % period, y + x / period * height)
                    } else {
                        (x + y / period * width, y % period)
                    };
                    let (from, to) = (
                        ((y * width + x) * 4) as usize,
                        ((to_y * packed[0] + to_x) * 4) as usize,
                    );
                    out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                }
            }
            let layout = Layout {
                sheet: [width, height],
                packed,
                cut: Some((along_x, period)),
            };
            return (layout, out);
        }
        (width, height, rgba) = halve(width, height, &rgba);
        // A frame that halving would cut through is no frame any more.
        step = step.map(|frame| if frame % 2 == 0 { frame / 2 } else { 0 });
    }
}

/// What the thread is asked for.
enum Job {
    /// Every effect of the resources.
    Effects,
    Sheet(SheetKey),
}

/// What it answers.
enum Done {
    Effects(HashMap<String, Arc<Effect>>),
    /// A sheet packed for its slot: its layout, and the image's width, height and texels.
    Sheet(SheetKey, Result<(Layout, Vec<u8>), String>),
}

/// The thread that reads the effects and their textures.
struct Loader {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
}

impl Loader {
    fn new(assets: Arc<Assets>) -> Option<Self> {
        let (jobs, asked) = mpsc::channel::<Job>();
        let (answers, done) = mpsc::channel();
        // It ends when the game leaves the server: the sender goes with what the window knows of it.
        let spawned = std::thread::Builder::new()
            .name("cobblemon particles".into())
            .spawn(move || {
                for job in asked {
                    let answer = match job {
                        Job::Effects => Done::Effects(read_effects(&assets)),
                        Job::Sheet(key) => {
                            let sheet = read_sheet(&assets, &key);
                            Done::Sheet(key, sheet)
                        }
                    };
                    if answers.send(answer).is_err() {
                        break;
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self { jobs, done }),
            Err(e) => {
                log::warn!(
                    "Horizon: no thread to read Cobblemon's particles on ({e}): none is drawn"
                );
                None
            }
        }
    }
}

/// Every `*.particle.json` under `dir`, whatever folder it is in: each Ball has its own
/// `sendflash.particle.json`.
fn effect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            effect_files(&path, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".particle.json"))
        {
            out.push(path);
        }
    }
}

/// The effect files of `folders`, the one that wins last: a file of a folder replaces the one an
/// earlier folder has at the same path under it. Sorted by that path, so that of two files that claim
/// one identifier the same one wins from run to run.
fn effect_paths(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths: HashMap<PathBuf, PathBuf> = HashMap::new();
    for folder in folders {
        let mut found = Vec::new();
        effect_files(folder, &mut found);
        for path in found {
            if let Ok(relative) = path.strip_prefix(folder) {
                paths.insert(relative.to_owned(), path);
            }
        }
    }
    let mut sorted: Vec<(PathBuf, PathBuf)> = paths.into_iter().collect();
    sorted.sort();
    sorted.into_iter().map(|(_, path)| path).collect()
}

/// `BedrockParticleOptionsRepository`: every `*.particle.json` under `bedrock/particles`, by the
/// identifier it gives itself.
fn read_effects(assets: &Assets) -> HashMap<String, Arc<Effect>> {
    // `assets.assets` has the root that wins first.
    let folders: Vec<PathBuf> = assets
        .assets
        .iter()
        .rev()
        .map(|root| root.join("bedrock/particles"))
        .collect();
    let mut effects = HashMap::new();
    for path in &effect_paths(&folders) {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|json| Effect::parse(&json).map_err(|e| e.0));
        match parsed {
            Ok(effect) => {
                effects.insert(effect.id.clone(), Arc::new(effect));
            }
            Err(e) => log::debug!("Horizon: Cobblemon particle {}: {e}", path.display()),
        }
    }
    let partial = effects
        .values()
        .filter(|effect: &&Arc<Effect>| !effect.unhandled.is_empty())
        .count();
    log::info!(
        "Horizon: {} Cobblemon particle effects, {partial} of them with something that is not done (said when one plays)",
        effects.len()
    );
    effects
}

/// The texture of `key`, packed for a slot.
fn read_sheet(assets: &Assets, key: &SheetKey) -> Result<(Layout, Vec<u8>), String> {
    let (texture, declared, step) = key;
    let (namespace, path) = texture.split_once(':').unwrap_or(("cobblemon", texture));
    let path = format!("textures/particle/{path}.png");
    let file = if namespace == "cobblemon" {
        assets.file(&path)
    } else {
        // Another namespace's assets are beside Cobblemon's in each root.
        assets
            .assets
            .iter()
            .filter_map(|root| Some(root.parent()?.join(namespace).join(&path)))
            .find(|file| file.is_file())
    }
    .ok_or_else(|| format!("no file {namespace}:{path}"))?;
    let png = std::fs::read(file).map_err(|e| e.to_string())?;
    let (width, height, rgba) =
        horizon_client::hud::decode_bytes(&png).map_err(|e| e.to_string())?;
    // The step in the image's own texels, which are the effect's unless a pack redrew the sheet.
    let own = |axis: usize, size: u32| {
        let scaled = u64::from(step[axis]) * u64::from(size);
        match u64::from(declared[axis]) {
            0 => 0,
            declared if scaled % declared == 0 => (scaled / declared) as u32,
            _ => 0,
        }
    };
    Ok(pack(
        width,
        height,
        rgba,
        [own(0, width), own(1, height)],
        POKEMON_SLOT_SIZE,
    ))
}

/// The sheet `effect`'s particles are frames of.
fn sheet_of(effect: &Effect) -> SheetKey {
    let uv = &effect.particle.uv;
    let whole = |value: Option<f32>| {
        value
            .filter(|step| *step >= 0.0 && step.fract() == 0.0)
            .map_or(0, |step| step as u32)
    };
    let step = uv.flipbook.as_ref().map_or([0; 2], |flipbook| {
        [
            whole(flipbook.step[0].constant()),
            whole(flipbook.step[1].constant()),
        ]
    });
    (
        effect.texture.clone(),
        uv.texture.map(|size| size.max(0) as u32),
        step,
    )
}

/// A sheet in the atlas, or on its way there.
enum Sheet {
    /// Its file is being read.
    Asked,
    /// In a slot, under this sprite's name.
    Placed { sprite: String, layout: Layout },
    /// It does not load, or there is no slot for it.
    None,
}

/// An effect to start later.
struct Later {
    at: Instant,
    what: What,
}

enum What {
    /// The Ball of Pokémon `pokemon` opens: its flash, then its sparks, where the Pokémon is by then.
    Opening { pokemon: i32, ball: String },
    /// An effect at a place, on nothing.
    At { effect: String, position: [f64; 3] },
    /// An effect the server asked for on an entity.
    On(SpawnEntityParticle),
}

/// Which keyframe of a model's animations: the animation among the model's
/// (`anim::Emitting::id`), and the keyframe's place in it. Cobblemon knows a keyframe by which object
/// it is.
type KeyframeId = (usize, usize, usize);

/// A particle effect an animation of a Pokémon's pose started and that plays still: one of
/// Cobblemon's `PosableState.poseParticles`.
#[derive(Clone, Debug, PartialEq)]
struct Emitted {
    id: KeyframeId,
    keyframe: ParticleKeyframe,
    /// Its emitter's `Storm::source`.
    source: u64,
}

/// What the animations of one Pokémon's poses have started: `PosableState`'s part in particle
/// effects, its `setPose` and its `tickEffects`.
#[derive(Debug, Default)]
struct Emitter {
    /// Whether the Pokémon was seen before: its first tick is its own.
    seen: bool,
    /// The pose it was last seen in, by its place in the poser's.
    pose: Option<usize>,
    /// Its age, in ticks, when its animations' moments were last looked at.
    age: u32,
    emitted: Vec<Emitted>,
}

impl Emitter {
    /// The Pokémon is now `age` ticks old, in the pose `variant` tells, as `state` has it: the effects
    /// to start, each with the number its emitter is to be found by (`sources` counts them).
    /// `plays` tells whether the emitter of that number is still there.
    ///
    /// As Cobblemon does it. An emitter that is gone is forgotten, so that its keyframe may start it
    /// again. On a change of pose, the effects the new pose's animations do not have end, and those
    /// it has at their very start begin, unless the same effect plays already (the same by what it
    /// says, `isSameAs`). Then, for each tick gone by, every keyframe the animations' time came up to
    /// starts, unless that very keyframe plays already. The two differ: of two keyframes that say the
    /// same, a change of pose starts one and the Pokémon's first tick the other, which is how Gastly
    /// has two clouds of gas.
    fn advance(
        &mut self,
        animator: &anim::Animator,
        variant: u8,
        state: &State,
        age: u32,
        plays: &dyn Fn(u64) -> bool,
        sources: &mut u64,
    ) -> Vec<(ParticleKeyframe, u64)> {
        let mut started = Vec::new();
        self.emitted.retain(|emitted| plays(emitted.source));
        let mut run = |emitted: &mut Vec<Emitted>, id: KeyframeId, keyframe: &ParticleKeyframe| {
            if emitted.iter().any(|playing| playing.id == id) {
                return;
            }
            *sources += 1;
            emitted.push(Emitted {
                id,
                keyframe: keyframe.clone(),
                source: *sources,
            });
            started.push((keyframe.clone(), *sources));
        };
        let program = animator.program(variant);
        let first = !std::mem::replace(&mut self.seen, true);
        if first || self.pose != program {
            // `setPose`, whatever the animations' conditions say.
            let of_pose: Vec<(KeyframeId, &ParticleKeyframe)> = program
                .into_iter()
                .flat_map(|program| animator.emitting(program))
                .flat_map(|emitting| {
                    let (file, animation) = emitting.id;
                    let keyframes = emitting.animation.particles.iter().enumerate();
                    keyframes.map(move |(place, keyframe)| ((file, animation, place), keyframe))
                })
                .collect();
            self.emitted.retain(|emitted| {
                of_pose
                    .iter()
                    .any(|(_, keyframe)| keyframe.is_same_as(&emitted.keyframe))
            });
            for (id, keyframe) in of_pose {
                let playing = |emitted: &Emitted| keyframe.is_same_as(&emitted.keyframe);
                if keyframe.seconds == 0.0 && !self.emitted.iter().any(playing) {
                    run(&mut self.emitted, id, keyframe);
                }
            }
            self.pose = program;
        }
        // `tickEffects`, a tick at a time: the first of the Pokémon's life, then those gone by since
        // (no more than a frame may run of the particles' own).
        let ticks: Vec<u32> = if first {
            vec![1]
        } else {
            let from = (self.age + 1).max(age.saturating_sub(CATCH_UP - 1));
            (from..=age).collect()
        };
        self.age = age;
        let Some(program) = program else {
            return started;
        };
        let context = anim::context(state);
        for tick in ticks {
            let (previous, now) = ((tick - 1) as f32 / 20.0, tick as f32 / 20.0);
            for emitting in animator
                .emitting(program)
                .filter(|emitting| emitting.runs(&context))
            {
                let keyframes = &emitting.animation.particles;
                let moments: Vec<f32> = keyframes.iter().map(|keyframe| keyframe.seconds).collect();
                let turn = emitting.animation.effects_turn();
                for place in moments_passed(&moments, turn, previous, now) {
                    let (file, animation) = emitting.id;
                    run(
                        &mut self.emitted,
                        (file, animation, place),
                        &keyframes[place],
                    );
                }
            }
        }
        started
    }
}

/// A Pokémon in the level, as its animations' effects know it.
struct Wearer {
    /// The type it had when its animator was looked for: its look is found again when that changes.
    kind: Arc<MobType>,
    /// What poses it, when a pose of that starts particle effects.
    animator: Option<Arc<anim::Animator>>,
    emitter: Emitter,
}

/// A particle to draw this frame.
struct Drawn {
    quad: Quad,
    /// Its part of the atlas: left, top, right, bottom.
    uv: [f32; 4],
    facing: Facing,
    /// Lit by the world where it is.
    lit: bool,
}

/// Cobblemon's particle effects: those that play, and what they are read and drawn from.
#[derive(Default)]
pub(super) struct Particles {
    loader: Option<Loader>,
    /// Whether the effects were asked for.
    asked: bool,
    /// The effects by identifier, once read.
    effects: Option<Arc<HashMap<String, Arc<Effect>>>>,
    storms: Storms,
    later: Vec<Later>,
    sheets: HashMap<SheetKey, Sheet>,
    /// The sprites of the sheets the effects that play draw from, which keep their slots.
    worn: HashSet<String>,
    /// When the clock was last read, and the part of a tick not run yet, in seconds.
    clock: Option<Instant>,
    owed: f64,
    drawn: Vec<Drawn>,
    /// The effects whose shortcomings were said.
    said: HashSet<String>,
    /// The Pokémon in the level, by entity id, and what their animations have started.
    wearers: HashMap<i32, Wearer>,
    /// How many effects the animations have started: each one's number (`Storm::source`).
    sources: u64,
}

impl Particles {
    /// Entity `id` is gone: its animations start nothing more, and what they started ends.
    pub(super) fn forget(&mut self, id: i32) {
        self.wearers.remove(&id);
    }

    /// The textures of the effects that play, which keep their slots in the atlas.
    pub(super) fn textures(&self) -> impl Iterator<Item = String> + '_ {
        self.worn.iter().cloned()
    }

    /// `texture` lost its slot in the atlas: its sheet is read again when next needed.
    pub(super) fn forget_texture(&mut self, texture: &str) {
        self.sheets
            .retain(|_, sheet| !matches!(sheet, Sheet::Placed { sprite, .. } if sprite == texture));
    }

    /// Nothing plays and nothing is about to.
    fn idle(&self) -> bool {
        self.storms.is_empty() && self.later.is_empty()
    }

    fn effect(&self, id: &str) -> Option<Arc<Effect>> {
        self.effects.as_ref()?.get(id).cloned()
    }

    /// Has the sheet of effect `id` read ahead of the effect: a Ball's are asked for as it is
    /// thrown, half a second before its flash.
    fn prepare(&mut self, id: &str) {
        let Some(effect) = self.effect(id) else {
            return;
        };
        let key = sheet_of(&effect);
        if let (Some(loader), std::collections::hash_map::Entry::Vacant(sheet)) =
            (&self.loader, self.sheets.entry(key.clone()))
        {
            let _ = loader.jobs.send(Job::Sheet(key));
            sheet.insert(Sheet::Asked);
        }
    }

    /// Starts `effect`, saying once what of it is not done.
    fn start(&mut self, effect: Arc<Effect>, start: Start) {
        let mut lacking = effect.unhandled.clone();
        if effect.material == Material::Add {
            lacking.push("its particles are blended, not added".to_owned());
        }
        if !lacking.is_empty() && self.said.insert(effect.id.clone()) {
            log::warn!(
                "Horizon: Cobblemon particle effect {} plays without: {}",
                effect.id,
                lacking.join("; ")
            );
        }
        if traced() {
            let [x, y, z] = start.emitter.origin;
            log::info!(
                "Horizon: particle effect {} starts at {x:.2} {y:.2} {z:.2}{}",
                effect.id,
                start.tag.as_ref().map_or(String::new(), |tag| format!(
                    " on entity {} ({}), at a scale of {:.2}",
                    tag.entity, tag.locator, start.emitter.scale
                ))
            );
        }
        let Some(effects) = self.effects.clone() else {
            return;
        };
        self.storms
            .start(effect, start, &|id| effects.get(id).cloned());
    }
}

/// `ParticleCameraMode.getRotation`: how a quad is turned. `camera` is the camera's rotation, `yaw`
/// and `pitch` its angles in degrees, `to_particle` the particle from the camera.
fn facing(
    facing: Facing,
    quad: &Quad,
    camera: Quat,
    yaw: f32,
    pitch: f32,
    to_particle: Vec3,
) -> Quat {
    let turn = |degrees: f32| degrees.to_radians();
    let roll = Quat::from_rotation_z(turn(quad.roll));
    let view = DVec3::from_array(quad.view).as_vec3();
    // The emitter's rotation, from the rows of its matrix.
    let emitter = || {
        let [x, y, z] = quad.rotation;
        Quat::from_mat3(&glam::Mat3::from_cols(
            Vec3::new(x[0] as f32, y[0] as f32, z[0] as f32),
            Vec3::new(x[1] as f32, y[1] as f32, z[1] as f32),
            Vec3::new(x[2] as f32, y[2] as f32, z[2] as f32),
        ))
    };
    // Cobblemon gives these angles, which are radians, to a function that takes degrees.
    let heading = view.x.atan2(view.z);
    let climb = view.y.atan2((view.x * view.x + view.z * view.z).sqrt());
    use std::f32::consts::{FRAC_PI_2, PI};
    match facing {
        Facing::RotateXyz => camera * roll,
        Facing::RotateY => Quat::from_rotation_y(turn(-yaw)) * roll * Quat::from_rotation_y(PI),
        Facing::LookAtXyz => {
            Quat::from_rotation_y(turn(-yaw - 180.0)) * Quat::from_rotation_x(turn(pitch)) * roll
        }
        Facing::LookAtY => Quat::from_rotation_y(turn(-yaw + 180.0)) * roll,
        Facing::DirectionZ => {
            Quat::from_rotation_x(turn(-climb)) * Quat::from_rotation_y(turn(heading))
        }
        Facing::DirectionY => {
            Quat::from_rotation_x(turn(climb - FRAC_PI_2))
                * Quat::from_rotation_y(turn(heading - PI))
        }
        Facing::DirectionX => {
            Quat::from_rotation_y(turn(heading - FRAC_PI_2)) * Quat::from_rotation_z(turn(climb))
        }
        Facing::EmitterYz => {
            emitter() * Quat::from_rotation_y(PI) * Quat::from_rotation_y(FRAC_PI_2)
        }
        Facing::EmitterXz => {
            emitter() * Quat::from_rotation_y(PI) * Quat::from_rotation_x(-FRAC_PI_2)
        }
        Facing::EmitterXy => emitter() * Quat::from_rotation_y(PI),
        Facing::LookAtDirection => {
            // Its x along the way it goes, its y square to that and to the line of sight.
            // The axis of the turn from the particle to where it goes, as the camera sees both.
            let (Some(along), Some(up)) = (
                view.try_normalize(),
                to_particle.cross(to_particle + view).try_normalize(),
            ) else {
                return camera * roll;
            };
            let lying = Quat::from_rotation_arc(Vec3::X, along);
            let stood = Quat::from_rotation_arc(lying * Vec3::Y, up) * lying;
            stood * roll * Quat::from_rotation_x(PI)
        }
    }
}

/// The four effects of a Pokémon that comes out of the Ball `ball` (`poke_ball`): its flash, its
/// sparks, its sparkles as it opens, and the sparkle that follows. `PokemonClientDelegate` names them
/// after the Ball without its underscores, the first three by whether the Pokémon is in a battle.
fn ball_effects(ball: &str, battling: bool) -> [String; 4] {
    let kind = ball.to_lowercase().replace('_', "");
    let mode = if battling { "battle" } else { "casual" };
    [
        format!("cobblemon:{kind}/{mode}/sendflash"),
        format!("cobblemon:{kind}/{mode}/ballsparks"),
        format!("cobblemon:{kind}/{mode}/ballsendsparkle"),
        format!("cobblemon:{kind}/ballsparkle"),
    ]
}

impl Game<'_> {
    /// The Ball of Pokémon `id` was thrown and opens at `opens`: its flash and its sparks play then
    /// (`PokemonClientDelegate`, `BEAM_MODE` 1).
    pub(super) fn pokemon_particles_thrown(&mut self, id: i32, ball: &str, opens: Instant) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        // Only a Pokémon that has an owner comes out with them.
        let Some(seen) = r.pokemon.seen.get(&id).filter(|seen| seen.owner.is_some()) else {
            return;
        };
        let effects = ball_effects(ball, seen.battle.is_some());
        let particles = &mut r.pokemon.particles;
        for effect in &effects {
            particles.prepare(effect);
        }
        particles.later.push(Later {
            at: opens,
            what: What::Opening {
                pokemon: id,
                ball: ball.to_owned(),
            },
        });
    }

    /// A message of the server that starts an effect.
    pub(super) fn pokemon_particle_payload(&mut self, channel: &str, data: &[u8]) {
        if traced() {
            let bytes: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            log::info!("Horizon: {channel} {bytes}");
        }
        let what = match channel {
            SPAWN_PARTICLE => SpawnParticle::read(data).map(|spawn| What::At {
                effect: spawn.effect,
                position: spawn.position,
            }),
            SPAWN_ENTITY_PARTICLE => SpawnEntityParticle::read(data).map(What::On),
            _ => return,
        };
        match what {
            Ok(what) => {
                if let Some(r) = self.remote.as_mut() {
                    r.pokemon.particles.later.push(Later {
                        at: Instant::now(),
                        what,
                    });
                }
            }
            Err(e) => log::warn!("Horizon: a particle effect of {channel} does not decode: {e}"),
        }
    }

    /// Where the model's own locator `locator` is for `mob` as its poser has it now: the part
    /// Cobblemon makes of a locator, under the poses of the parts it hangs from. `None` for a model
    /// without that locator.
    fn pokemon_model_locator(&self, mob: &Mob, locator: &str) -> Option<Frame> {
        let feet = DVec3::new(mob.position.x, mob.position.y, mob.position.z);
        let part = format!("{LOCATOR_PREFIX}{locator}");
        let data = self.res.mob_data();
        let matrix = horizon_client::mobs::render::part_transform(mob, 1.0, feet, data, &part)?;
        // `LocatorAccess.update`: the flip every model is drawn under is undone.
        let matrix = matrix * glam::Mat4::from_rotation_z(std::f32::consts::PI);
        let (scale, rotation, offset) = matrix.to_scale_rotation_translation();
        if !offset.is_finite() || !rotation.is_finite() || !scale.is_finite() {
            return None;
        }
        let turn = glam::Mat3::from_quat(rotation);
        let row = |axis: usize| {
            [
                f64::from(turn.x_axis[axis]),
                f64::from(turn.y_axis[axis]),
                f64::from(turn.z_axis[axis]),
            ]
        };
        Some(Frame {
            origin: (feet + offset.as_dvec3()).to_array(),
            rotation: [row(0), row(1), row(2)],
            scale: f64::from(scale.x.abs()).max(0.01),
        })
    }

    /// Where the locator `locator` of entity `id` is, and what an effect on it asks about it: the
    /// locator's frame, the entity's box and its motion in blocks a tick, and whether it is seen.
    /// `None` for an entity that is not there, or a locator that is neither the model's own nor one
    /// of those every model has.
    fn pokemon_locator(&self, id: i32, locator: &str) -> Option<(Frame, Body, [f64; 3], bool)> {
        let pokemon = &self.remote.as_ref()?.pokemon;
        let seen = pokemon.seen.get(&id)?;
        let mob = self.mobs.iter().find(|m| m.id == id as u32)?;
        let (width, height, _) = mob.dims();
        let feet = [mob.position.x, mob.position.y, mob.position.z];
        // The species' base scale, which the look was drawn at.
        let base = pokemon
            .looks
            .values()
            .flatten()
            .find(|look| Arc::ptr_eq(&look.kind, &mob.kind))
            .map_or(1.0, |look| f64::from(look.base_scale));
        let size = base * f64::from(mob.extra.model_scale);
        // `LocatorAccess.update`: the approximations Cobblemon gives every model, which the model's
        // own locator of the name replaces. The root is every model's.
        let up = match locator {
            "root" => Some(0.0),
            "middle" => Some(height / 2.0),
            "top" => Some(height),
            _ => None,
        };
        let own = (locator != "root")
            .then(|| self.pokemon_model_locator(mob, locator))
            .flatten();
        let frame = match (own, up) {
            (Some(own), _) => own,
            // `PosableModel.updateLocators`: half a turn from the way the body faces.
            (None, Some(up)) => Frame::turned(
                [feet[0], feet[1] + up, feet[2]],
                180.0 - f64::from(mob.body_yaw),
                size.max(0.01),
            ),
            (None, None) => return None,
        };
        let body = Body {
            width,
            height,
            scale: base * f64::from(seen.scale_modifier),
            center: [feet[0], feet[1] + height / 2.0, feet[2]],
        };
        let motion = [
            mob.position.x - mob.previous.x,
            mob.position.y - mob.previous.y,
            mob.position.z - mob.previous.z,
        ];
        let visible = !mob.invisible && !pokemon.phases.unseen(id);
        Some((frame, body, motion, visible))
    }

    /// `SpawnSnowstormEntityParticleHandler`: `spawn`'s effect on its entity, at the first of its
    /// locators that is one.
    fn pokemon_particle_on(&mut self, spawn: &SpawnEntityParticle, effect: Arc<Effect>) {
        let mut passed = Vec::new();
        let found = spawn.locators.iter().find_map(|locator| {
            let at = self.pokemon_locator(spawn.entity, locator);
            if at.is_none() {
                passed.push(locator.clone());
            }
            Some((locator.clone(), at?))
        });
        // A locator of the model itself, which only Cobblemon knows where to find: said once.
        if self.mobs.iter().any(|m| m.id == spawn.entity as u32)
            && let Some(r) = self.remote.as_mut()
        {
            for locator in passed {
                if r.pokemon
                    .particles
                    .said
                    .insert(format!("locator {locator}"))
                {
                    log::warn!(
                        "Horizon: the locator {locator} of a model is not known here: {} plays at {}",
                        spawn.effect,
                        found.as_ref().map_or("no place", |(next, _)| next.as_str())
                    );
                }
            }
        }
        let Some((locator, (frame, body, _, _))) = found else {
            return;
        };
        // What it is aimed at: the middle of that entity, where it is now.
        let target = spawn.target.and_then(|target| {
            let mob = self.mobs.iter().find(|m| m.id == target as u32)?;
            Some([
                mob.position.x,
                mob.position.y + f64::from(mob.height()) / 2.0,
                mob.position.z,
            ])
        });
        self.pokemon_particle_start(spawn.entity, locator, frame, body, effect, target, 0);
    }

    /// `effect` starts on entity `entity`, at its locator `locator`, which is at `frame`. `source` is
    /// the number its emitter is found again by, 0 for none.
    #[allow(clippy::too_many_arguments)]
    fn pokemon_particle_start(
        &mut self,
        entity: i32,
        locator: String,
        frame: Frame,
        body: Body,
        effect: Arc<Effect>,
        target: Option<[f64; 3]>,
        source: u64,
    ) {
        // `EmitterSpace.initializeEmitterMatrix`: the root's rotation, the locator's place, and its
        // scale for an effect that scales with its entity.
        let root = self.pokemon_locator(entity, "root");
        let emitter = Frame {
            origin: frame.origin,
            rotation: root.map_or(frame.rotation, |(root, ..)| root.rotation),
            scale: if effect.emitter.entity_scaling {
                frame.scale
            } else {
                1.0
            },
        };
        let start = Start {
            emitter,
            attached: Some(frame),
            tag: Some(Tag {
                entity,
                locator,
                bound: true,
            }),
            body: Some(body),
            target,
            source,
        };
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.particles.start(effect, start);
        }
    }

    /// `PosableState.incrementAge` for particle effects: each Pokémon's pose and age, and the effects
    /// its pose's animations start by now ([`Emitter::advance`]).
    fn pokemon_emitters_frame(&mut self) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let super::Pokemon {
            particles, looks, ..
        } = &mut r.pokemon;
        // Nothing starts before the effects are read: what a pose starts as it is taken would be
        // missed for good.
        if particles.effects.is_none() {
            return;
        }
        let Particles {
            wearers,
            storms,
            sources,
            ..
        } = particles;
        let plays = |source: u64| storms.iter().any(|s| s.source == source && !s.despawned());
        let mut started: Vec<(i32, ParticleKeyframe, u64)> = Vec::new();
        for mob in self.mobs.iter().filter(|m| m.species() == Species::Pokemon) {
            let id = mob.id as i32;
            let known = wearers
                .get(&id)
                .is_some_and(|wearer| Arc::ptr_eq(&wearer.kind, &mob.kind));
            if !known {
                // Its look, by the type it wears; none while its files are being read.
                let animator = looks
                    .values()
                    .flatten()
                    .find(|look| Arc::ptr_eq(&look.kind, &mob.kind))
                    .and_then(|look| look.animator.clone())
                    .filter(|animator| animator.emits());
                let wearer = Wearer {
                    kind: mob.kind.clone(),
                    animator,
                    emitter: Emitter::default(),
                };
                wearers.insert(id, wearer);
            }
            let Some(Wearer {
                animator: Some(animator),
                emitter,
                ..
            }) = wearers.get_mut(&id)
            else {
                continue;
            };
            let state = horizon_client::mobs::anim::state(mob, 1.0);
            let variant = mob.extra.variant;
            for (keyframe, source) in
                emitter.advance(animator, variant, &state, mob.age, &plays, sources)
            {
                started.push((id, keyframe, source));
            }
        }
        for (id, keyframe, source) in started {
            self.pokemon_emitted_start(id, &keyframe, source);
        }
    }

    /// `BedrockParticleKeyframe.run`: the effect of `keyframe` on Pokémon `id`, at the keyframe's
    /// locator or, for a model without it, at its root.
    fn pokemon_emitted_start(&mut self, id: i32, keyframe: &ParticleKeyframe, source: u64) {
        let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles) else {
            return;
        };
        let Some(effect) = particles.effect(&keyframe.effect) else {
            // Cobblemon drops the whole file of an animation that names an effect it does not know.
            if particles
                .said
                .insert(format!("unknown {}", keyframe.effect))
            {
                log::warn!(
                    "Horizon: an animation starts the particle effect {}, which is not known",
                    keyframe.effect
                );
            }
            return;
        };
        if keyframe.script.is_some() && particles.said.insert(format!("script {}", effect.id)) {
            log::warn!(
                "Horizon: an animation's particle effect {} plays without its `pre_effect_script`",
                effect.id
            );
        }
        let at = self.pokemon_locator(id, &keyframe.locator);
        let (locator, at) = match at {
            Some(at) => (keyframe.locator.as_str(), at),
            None => match self.pokemon_locator(id, "root") {
                Some(root) => ("root", root),
                None => return,
            },
        };
        let (frame, body, ..) = at;
        self.pokemon_particle_start(id, locator.to_owned(), frame, body, effect, None, source);
    }

    /// The effects whose time has come start.
    fn pokemon_particles_due(&mut self, now: Instant) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let particles = &mut r.pokemon.particles;
        if particles.later.iter().all(|later| later.at > now) {
            return;
        }
        if particles.effects.is_none() {
            // Not read yet: what has waited long enough is dropped.
            particles
                .later
                .retain(|later| now.saturating_duration_since(later.at) < PATIENCE);
            return;
        }
        let (due, later): (Vec<Later>, Vec<Later>) = std::mem::take(&mut particles.later)
            .into_iter()
            .partition(|later| later.at <= now);
        particles.later = later;
        for Later { at, what } in due {
            match what {
                What::At { effect, position } => {
                    let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles)
                    else {
                        return;
                    };
                    match particles.effect(&effect) {
                        Some(effect) => particles.start(effect, Start::at(position)),
                        None => log::debug!("Horizon: no Cobblemon particle effect {effect}"),
                    }
                }
                What::On(spawn) => {
                    let effect = self
                        .remote
                        .as_ref()
                        .and_then(|r| r.pokemon.particles.effect(&spawn.effect));
                    match effect {
                        Some(effect) => self.pokemon_particle_on(&spawn, effect),
                        None => {
                            log::debug!("Horizon: no Cobblemon particle effect {}", spawn.effect)
                        }
                    }
                }
                What::Opening { pokemon, ball } => {
                    let Some(mob) = self.mobs.iter().find(|m| m.id == pokemon as u32) else {
                        continue;
                    };
                    let position = [mob.position.x, mob.position.y, mob.position.z];
                    let Some(r) = self.remote.as_mut() else {
                        return;
                    };
                    let battling = r
                        .pokemon
                        .seen
                        .get(&pokemon)
                        .is_some_and(|s| s.battle.is_some());
                    let [flash, sparks, sparkles, sparkle] = ball_effects(&ball, battling);
                    let particles = &mut r.pokemon.particles;
                    // Without its flash a Ball has none of the four.
                    let Some(flash) = particles.effect(&flash) else {
                        log::debug!("Horizon: the Ball {ball} has no flash");
                        continue;
                    };
                    particles.start(flash, Start::at(position));
                    for (effect, after) in [
                        (sparks, SPARKS_AFTER),
                        (sparkles, SPARKS_AFTER),
                        (sparkle, SPARKS_AFTER + SPARKLE_AFTER),
                    ] {
                        // The ones a Ball lacks are not played, the others are.
                        if particles.effect(&effect).is_some() {
                            particles.later.push(Later {
                                at: at + after,
                                what: What::At { effect, position },
                            });
                        }
                    }
                }
            }
        }
    }

    /// Once a frame: what was read since the last one, the effects whose time has come, the ticks the
    /// frame owes, and what there is to draw.
    pub(super) fn pokemon_particles_frame(&mut self) {
        let now = Instant::now();
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let pokemon = &mut r.pokemon;
        // Nothing of this without Cobblemon's resources, which the first Pokémon has looked for.
        let Some(Some(assets)) = pokemon.assets.clone() else {
            return;
        };
        let particles = &mut pokemon.particles;
        if !particles.asked {
            particles.asked = true;
            particles.loader = Loader::new(assets);
            if let Some(loader) = &particles.loader {
                let _ = loader.jobs.send(Job::Effects);
            }
        }
        // What the thread has read.
        let mut sheets = Vec::new();
        if let Some(loader) = &particles.loader {
            while let Ok(done) = loader.done.try_recv() {
                match done {
                    Done::Effects(effects) => particles.effects = Some(Arc::new(effects)),
                    Done::Sheet(key, sheet) => sheets.push((key, sheet)),
                }
            }
        }
        for (key, sheet) in sheets {
            let placed = match sheet {
                Ok((layout, rgba)) => {
                    let sprite = format!("cobblemon/particle/{}#{}x{}", key.0, key.2[0], key.2[1]);
                    let [width, height] = layout.packed;
                    match self.pokemon_skin(&sprite, (width, height, rgba)) {
                        Some(()) => Sheet::Placed { sprite, layout },
                        None => {
                            log::debug!(
                                "Horizon: no slot of the atlas for the particles of {}",
                                key.0
                            );
                            Sheet::None
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Horizon: the particle texture {} does not load: {e}", key.0);
                    Sheet::None
                }
            };
            if let Some(r) = self.remote.as_mut() {
                r.pokemon.particles.sheets.insert(key, placed);
            }
        }
        self.pokemon_emitters_frame();
        self.pokemon_particles_due(now);
        let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles) else {
            return;
        };
        if particles.idle() {
            (particles.clock, particles.owed) = (None, 0.0);
            particles.drawn.clear();
            particles.worn.clear();
            return;
        }

        // The ticks this frame owes.
        let since = particles
            .clock
            .replace(now)
            .map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        let owed = particles.owed + since;
        let ticks = ((owed / TICK) as u32).min(CATCH_UP);
        let owed = if owed >= f64::from(CATCH_UP + 1) * TICK {
            0.0
        } else {
            owed - f64::from(ticks) * TICK
        };
        for _ in 0..ticks {
            // The emitters that are on an entity, where it is now.
            let tags: Vec<(usize, Tag)> = self
                .remote
                .as_ref()
                .into_iter()
                .flat_map(|r| r.pokemon.particles.storms.iter().enumerate())
                .filter_map(|(index, storm)| Some((index, storm.tag.clone()?)))
                .collect();
            let follows: Vec<(usize, Option<(Frame, [f64; 3], bool)>)> = tags
                .into_iter()
                .map(|(index, tag)| {
                    let at = self
                        .pokemon_locator(tag.entity, &tag.locator)
                        .map(|(frame, _, motion, visible)| (frame, motion, visible));
                    (index, at)
                })
                .collect();
            let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles) else {
                return;
            };
            // `BedrockParticleKeyframe.run`'s `sourceAlive`: an effect an animation started lives
            // while its Pokémon's pose still has it.
            let lasting: HashSet<u64> = particles
                .wearers
                .values()
                .flat_map(|wearer| &wearer.emitter.emitted)
                .map(|emitted| emitted.source)
                .collect();
            let mut follows = follows.into_iter().peekable();
            for (index, storm) in particles.storms.iter_mut().enumerate() {
                if follows.peek().is_none_or(|(of, _)| *of != index) {
                    continue;
                }
                let Some((_, at)) = follows.next() else {
                    break;
                };
                let bound = storm.tag.as_ref().is_some_and(|tag| tag.bound);
                let lasts = storm.source == 0 || lasting.contains(&storm.source);
                match at {
                    Some((frame, motion, visible)) => {
                        storm.follow(bound.then_some(frame), motion, lasts, visible);
                    }
                    // Its entity is gone.
                    None => storm.follow(None, [0.0; 3], false, true),
                }
            }
            if let Some(effects) = particles.effects.clone() {
                particles.storms.tick(&|id| effects.get(id).cloned());
            }
        }
        let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles) else {
            return;
        };
        particles.owed = owed;
        let sounds = std::mem::take(&mut particles.storms.sounds);
        for sound in sounds {
            // `ParticleEvent.run`: at its place, at full volume and its own pitch.
            self.audio.play(&sound.id, Some(sound.at), 1.0, 1.0);
        }

        // What there is to draw, and the sheets it is drawn from.
        let atlas = &self.gui.atlas;
        let Some(particles) = self.remote.as_mut().map(|r| &mut r.pokemon.particles) else {
            return;
        };
        let partial = (particles.owed / TICK).clamp(0.0, 1.0);
        let Particles {
            storms,
            sheets,
            drawn,
            worn,
            loader,
            ..
        } = particles;
        drawn.clear();
        worn.clear();
        // The sheet of the effect whose quads are coming, and where it is in the atlas.
        let mut current: Option<(*const Effect, Option<(Layout, [f32; 4])>)> = None;
        storms.quads(partial, &mut |effect, quad| {
            let known = matches!(current, Some((of, _)) if std::ptr::eq(of, effect));
            if !known {
                let key = sheet_of(effect);
                let sheet = sheets.entry(key.clone()).or_insert_with(|| {
                    if let Some(loader) = loader {
                        let _ = loader.jobs.send(Job::Sheet(key));
                    }
                    Sheet::Asked
                });
                let placed = match sheet {
                    Sheet::Placed { sprite, layout } => {
                        worn.insert(sprite.clone());
                        atlas
                            .sprites
                            .get(sprite.as_str())
                            .map(|rect| (*layout, [rect.u0, rect.v0, rect.u1, rect.v1]))
                    }
                    Sheet::Asked | Sheet::None => None,
                };
                current = Some((std::ptr::from_ref(effect), placed));
            }
            let Some((_, Some((layout, [u0, v0, u1, v1])))) = current else {
                return;
            };
            let Some([left, top, right, bottom]) = layout.place(quad.uv) else {
                return;
            };
            drawn.push(Drawn {
                quad: *quad,
                uv: [
                    u0 + (u1 - u0) * left,
                    v0 + (v1 - v0) * top,
                    u0 + (u1 - u0) * right,
                    v0 + (v1 - v0) * bottom,
                ],
                facing: effect.particle.facing,
                lit: effect.particle.environment_lighting,
            });
        });
    }

    /// `SnowstormParticle.render`: the particles of the effects that play, as quads among the level's
    /// translucent particles.
    pub(in super::super::super) fn pokemon_particle_geometry(
        &self,
        camera: DVec3,
        out: &mut Vec<Vertex>,
    ) {
        let Some(r) = self.remote.as_ref() else {
            return;
        };
        let drawn = &r.pokemon.particles.drawn;
        if drawn.is_empty() {
            return;
        }
        // `Camera.rotation()`, as the level's own particles are turned.
        let turned = Quat::from_rotation_y(std::f32::consts::PI - self.yaw.to_radians())
            * Quat::from_rotation_x(-self.pitch.to_radians());
        for Drawn {
            quad,
            uv,
            facing: mode,
            lit,
        } in drawn
        {
            let center = (DVec3::from_array(quad.center) - camera).as_vec3();
            let rotation = facing(*mode, quad, turned, self.yaw, self.pitch, center);
            let light = if *lit {
                let [x, y, z] = quad.center.map(|at| at.floor() as i32);
                let light = self.world.light(x, y, z);
                (u32::from(light >> 4) * 16) | ((u32::from(light & 15) * 16) << 8)
            } else {
                240 | 240 << 8
            };
            // The colour's channels as bytes: what is no number is 0, as it is in Java.
            let byte = |channel: f32| u32::from((channel.clamp(0.0, 1.0) * 255.0) as u8);
            let [red, green, blue, alpha] = quad.colour.map(byte);
            if alpha == 0 {
                continue;
            }
            let color = alpha << 24 | blue << 16 | green << 8 | red;
            let [left, top, right, bottom] = *uv;
            let [x, y] = quad.half;
            for (corner, uv) in [
                (Vec3::new(x, -y, 0.0), [right, bottom]),
                (Vec3::new(x, y, 0.0), [right, top]),
                (Vec3::new(-x, y, 0.0), [left, top]),
                (Vec3::new(-x, -y, 0.0), [left, bottom]),
            ] {
                out.push(Vertex {
                    pos: (center + rotation * corner).to_array(),
                    uv,
                    layer: horizon_client::gfx::TRANSLUCENT_PARTICLE_OVERLAY,
                    color,
                    light,
                    normal: [0; 4],
                    glint_uv: [0.0, 0.0],
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image of `width` by `height` whose texel (x, y) is (x, y, x / 256, 255).
    fn image(width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, (x / 256) as u8, 255]))
            .collect()
    }

    /// The texel of `rgba`, `width` wide, at (x, y), as the (x, y) it was at in `image`.
    fn texel(rgba: &[u8], width: u32, x: u32, y: u32) -> (u32, u32) {
        let at = ((y * width + x) * 4) as usize;
        (
            u32::from(rgba[at]) + 256 * u32::from(rgba[at + 2]),
            u32::from(rgba[at + 1]),
        )
    }

    #[test]
    fn a_sheet_that_fits_a_slot_is_packed_as_it_is() {
        let (layout, rgba) = pack(99, 9, image(99, 9), [9, 0], 256);
        assert_eq!(
            layout,
            Layout {
                sheet: [99, 9],
                packed: [99, 9],
                cut: None
            }
        );
        assert_eq!(rgba, image(99, 9));
        // The third frame of eleven, where it is in the sheet.
        let frame = layout.place([18.0 / 99.0, 0.0, 27.0 / 99.0, 1.0]).unwrap();
        assert!((frame[0] - 18.0 / 99.0).abs() < 1.0e-6 && frame[3] == 1.0);
        // A frame past the sheet's end is no part of it.
        assert_eq!(layout.place([1.0, 0.0, 1.1, 1.0]), None);
    }

    #[test]
    fn a_long_sheet_is_cut_into_rows_of_whole_frames() {
        // The flash of a Ball in battle: sixteen frames of 64 in a row.
        let (layout, rgba) = pack(1024, 64, image(1024, 64), [64, 0], 256);
        assert_eq!(
            layout,
            Layout {
                sheet: [1024, 64],
                packed: [256, 256],
                cut: Some((true, 256))
            }
        );
        // The sixth frame (320..384) is the second of the second row.
        let frame = layout
            .place([320.0 / 1024.0, 0.0, 384.0 / 1024.0, 1.0])
            .unwrap();
        assert_eq!(frame, [0.25, 0.25, 0.5, 0.5]);
        assert_eq!(texel(&rgba, 256, 64, 64), (320, 0));
        assert_eq!(texel(&rgba, 256, 127, 127), (383, 63));
        // The last frame ends the last row.
        let last = layout.place([960.0 / 1024.0, 0.0, 1.0, 1.0]).unwrap();
        assert_eq!(last, [0.75, 0.75, 1.0, 1.0]);
        // Twelve frames of 32: a row of eight and a row of four.
        let (layout, rgba) = pack(384, 32, image(384, 32), [32, 0], 256);
        assert_eq!((layout.packed, layout.cut), ([256, 64], Some((true, 256))));
        assert_eq!(texel(&rgba, 256, 0, 32), (256, 0));
        let ninth = layout
            .place([256.0 / 384.0, 0.0, 288.0 / 384.0, 1.0])
            .unwrap();
        assert_eq!(ninth, [0.0, 0.5, 0.125, 1.0]);
        // A sheet of frames one under the other is cut into columns.
        let (layout, rgba) = pack(8, 320, image(8, 320), [0, 8], 256);
        assert_eq!((layout.packed, layout.cut), ([16, 256], Some((false, 256))));
        assert_eq!(texel(&rgba, 16, 8, 0), (0, 256 % 256));
        let late = layout
            .place([0.0, 256.0 / 320.0, 1.0, 264.0 / 320.0])
            .unwrap();
        assert_eq!(late, [0.5, 0.0, 1.0, 8.0 / 256.0]);
    }

    #[test]
    fn what_does_not_fit_even_cut_is_halved_first() {
        // Twenty-one frames of 64: six rows would be 384 high. Halved, three rows of eight.
        let (layout, rgba) = pack(1344, 64, image(1344, 64), [64, 0], 256);
        assert_eq!(
            layout,
            Layout {
                sheet: [672, 32],
                packed: [256, 96],
                cut: Some((true, 256))
            }
        );
        assert_eq!(rgba.len(), 256 * 96 * 4);
        // The ninth frame starts the second row, and is every other texel of what it was.
        let ninth = layout
            .place([512.0 / 1344.0, 0.0, 576.0 / 1344.0, 1.0])
            .unwrap();
        assert_eq!(ninth, [0.0, 32.0 / 96.0, 0.125, 64.0 / 96.0]);
        assert_eq!(texel(&rgba, 256, 1, 33), (514, 2));
        // No frames to cut along: halved until it fits.
        let (layout, _) = pack(1024, 64, image(1024, 64), [0, 0], 256);
        assert_eq!(
            layout,
            Layout {
                sheet: [256, 16],
                packed: [256, 16],
                cut: None
            }
        );
        // A part of the sheet a cut goes through is in no one piece.
        let (layout, _) = pack(1024, 64, image(1024, 64), [64, 0], 256);
        assert_eq!(
            layout.place([224.0 / 1024.0, 0.0, 288.0 / 1024.0, 1.0]),
            None
        );
    }

    #[test]
    fn a_quad_faces_the_camera_and_turns_about_its_line_of_sight() {
        let quad = |roll: f32| Quad {
            center: [0.0; 3],
            half: [0.5, 0.5],
            roll,
            uv: [0.0; 4],
            colour: [1.0; 4],
            view: [0.0, 1.0, 0.0],
            rotation: cobblelegacy_cobblemon::storm::NO_TURN,
        };
        // A camera that looks south (yaw 0): `Camera.rotation()` is half a turn about the vertical.
        let camera = Quat::from_rotation_y(std::f32::consts::PI);
        let to = Vec3::new(0.0, 0.0, 4.0);
        let flat = facing(Facing::RotateXyz, &quad(0.0), camera, 0.0, 0.0, to);
        // The quad's own right is the west, which is the right of who looks south.
        assert!((flat * Vec3::X - Vec3::NEG_X).length() < 1.0e-5);
        assert!((flat * Vec3::Y - Vec3::Y).length() < 1.0e-5);
        // Turned a quarter: its right is up or down, and it still faces the camera.
        let turned = facing(Facing::RotateXyz, &quad(90.0), camera, 0.0, 0.0, to);
        assert!((turned * Vec3::X).y.abs() > 0.999);
        assert!((turned * Vec3::Z - flat * Vec3::Z).length() < 1.0e-5);
        // On the emitter's ground, it lies flat whatever the camera.
        let ground = facing(Facing::EmitterXz, &quad(0.0), camera, 0.0, 30.0, to);
        assert!((ground * Vec3::Z).y.abs() > 0.999);
        // Along its motion, its right is the way it goes.
        let along = facing(Facing::LookAtDirection, &quad(0.0), camera, 0.0, 0.0, to);
        assert!((along * Vec3::X - Vec3::Y).length() < 1.0e-4);
    }

    #[test]
    fn effect_files_are_told_apart_by_their_folders_and_a_later_root_replaces_them() {
        let root = std::env::temp_dir().join(format!(
            "horizon-particles-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let write = |path: &str| {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "{}").unwrap();
        };
        // Every Ball has a file of the same name; a pack redoes one of them and adds another.
        write("mod/balls/pokeball/casual/sendflash.particle.json");
        write("mod/balls/greatball/casual/sendflash.particle.json");
        write("mod/balls/greatball/casual/notes.txt");
        write("pack/balls/greatball/casual/sendflash.particle.json");
        write("pack/fire_beam.particle.json");
        let paths = effect_paths(&[root.join("mod"), root.join("pack"), root.join("absent")]);
        let under: Vec<String> = paths
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            under,
            [
                "pack/balls/greatball/casual/sendflash.particle.json",
                "mod/balls/pokeball/casual/sendflash.particle.json",
                "pack/fire_beam.particle.json",
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_sheet_is_named_by_its_texture_and_its_frames() {
        let effect = Effect::parse(
            r#"{"particle_effect":{"description":{"identifier":"cobblemon:pokeball/battle/sendflash",
                "basic_render_parameters":{"material":"particles_alpha","texture":"textures/particles/balls/pokeball/battle/sendbattle"}},
              "components":{"minecraft:emitter_rate_instant":{"num_particles":1},
                "minecraft:emitter_lifetime_once":{"active_time":1},"minecraft:emitter_shape_point":{},
                "minecraft:particle_appearance_billboard":{"size":[2.25,2.25],"uv":{"texture_width":1024,"texture_height":64,
                  "flipbook":{"base_UV":[0,0],"size_UV":[64,64],"step_UV":[64,0],"frames_per_second":24,"max_frame":16}}}}}}"#,
        )
        .unwrap();
        assert_eq!(
            sheet_of(&effect),
            (
                "cobblemon:balls/pokeball/battle/sendbattle".to_owned(),
                [1024, 64],
                [64, 0]
            )
        );
    }

    /// Gastly, as its files have it: two poses, each with its animation; both animations start the
    /// same gas, twice, at their very start, and have no length. A third pose bubbles every two
    /// seconds, and a fourth starts nothing.
    fn gastly() -> anim::Animator {
        use cobblelegacy_cobblemon::{animation::AnimationFile, poser::Poser};
        let poser = Poser::parse(
            r#"{
                "poses": {
                    "standing": {
                        "poseTypes": ["STAND"],
                        "animations": ["q.bedrock('gastly', 'ground_idle')"]
                    },
                    "walking": {
                        "poseTypes": ["WALK"],
                        "animations": ["q.bedrock('gastly', 'ground_walk')"]
                    },
                    "swimming": {
                        "poseTypes": ["SWIM"],
                        "animations": ["q.bedrock('gastly', 'water_swim')"]
                    },
                    "sleeping": {
                        "poseTypes": ["SLEEP"],
                        "animations": ["q.bedrock('gastly', 'sleep')"]
                    }
                }
            }"#,
        )
        .unwrap();
        let gas = r#"{"0.0": [
            {"effect": "ghostly_smoke", "locator": "gas"},
            {"effect": "ghostly_smoke", "locator": "gas"}
        ]}"#;
        let file = Arc::new(
            AnimationFile::parse(&format!(
                r#"{{"animations": {{
                    "animation.gastly.ground_idle": {{"loop": true, "particle_effects": {gas}}},
                    "animation.gastly.ground_walk": {{"loop": true, "particle_effects": {gas}}},
                    "animation.gastly.water_swim": {{
                        "loop": true,
                        "animation_length": 2.0,
                        "particle_effects": {{"0.5": {{"effect": "bubble", "locator": "mouth"}}}}
                    }},
                    "animation.gastly.sleep": {{"loop": true}}
                }}}}"#
            ))
            .unwrap(),
        );
        let mut load = |group: &str| (group == "gastly").then(|| file.clone());
        anim::Animator::new(&poser, &mut load)
    }

    fn still(age: u32) -> State {
        State {
            pos: 0.0,
            speed: 0.0,
            age: age as f32,
            y_rot: 0.0,
            x_rot: 0.0,
            swing: 0.0,
            baby: false,
            partial: 0.0,
        }
    }

    #[test]
    fn the_effects_of_a_pose_start_and_end_as_cobblemon_s_do() {
        use cobblelegacy_cobblemon::wire::PoseType;
        let animator = gastly();
        assert!(animator.emits());
        let (stand, walk) = (PoseType::Stand as u8, anim::with_pose(0, PoseType::Walk));
        let (swim, sleep) = (
            anim::with_pose(0, PoseType::Swim),
            anim::with_pose(0, PoseType::Sleep),
        );
        let mut emitter = Emitter::default();
        let mut sources = 0;
        // Every emitter that was started plays on, until told otherwise.
        let all = |_: u64| true;
        let mut advance =
            |emitter: &mut Emitter, variant: u8, age: u32, plays: &dyn Fn(u64) -> bool| {
                let started =
                    emitter.advance(&animator, variant, &still(age), age, plays, &mut sources);
                started
                    .into_iter()
                    .map(|(keyframe, source)| (keyframe.effect, keyframe.locator, source))
                    .collect::<Vec<_>>()
            };
        let gas = |source: u64| {
            (
                "cobblemon:ghostly_smoke".to_owned(),
                "gas".to_owned(),
                source,
            )
        };

        // First seen, already 40 ticks old: the pose is taken, which starts one of the two clouds
        // that say the same, and its first tick starts the other.
        assert_eq!(advance(&mut emitter, stand, 40, &all), [gas(1), gas(2)]);
        // Nothing more as time goes: the animation has no length, its start does not come again.
        assert_eq!(advance(&mut emitter, stand, 41, &all), []);
        assert_eq!(advance(&mut emitter, stand, 41, &all), []);
        assert_eq!(advance(&mut emitter, stand, 300, &all), []);
        // Walking: the same gas, by what it says, so the two clouds stay and none starts.
        assert_eq!(advance(&mut emitter, walk, 301, &all), []);
        assert_eq!(emitter.emitted.len(), 2);
        assert_eq!(advance(&mut emitter, stand, 302, &all), []);
        // Swimming has no gas: both clouds end. Its bubble comes half a second into each two.
        assert_eq!(advance(&mut emitter, swim, 303, &all), []);
        assert!(emitter.emitted.is_empty());
        assert_eq!(advance(&mut emitter, swim, 309, &all), []);
        let bubble = |source: u64| ("cobblemon:bubble".to_owned(), "mouth".to_owned(), source);
        // Tick 330 is half a second into the turn that began at tick 320.
        assert_eq!(advance(&mut emitter, swim, 330, &all), [bubble(3)]);
        // While it plays, the same keyframe does not start it again a turn later, on the tick that
        // comes up to it or on the one that leaves it.
        assert_eq!(advance(&mut emitter, swim, 370, &all), []);
        assert_eq!(advance(&mut emitter, swim, 371, &all), []);
        // Once its emitter is gone it may.
        let gone = |source: u64| source != 3;
        assert_eq!(advance(&mut emitter, swim, 372, &gone), []);
        assert!(emitter.emitted.is_empty());
        assert_eq!(advance(&mut emitter, swim, 410, &all), [bubble(4)]);
        // Back on the ground the gas starts again, one cloud: this is no first tick.
        assert_eq!(advance(&mut emitter, stand, 411, &all), [gas(5)]);
        assert_eq!(emitter.emitted.len(), 1);
        // Asleep, nothing: what played ends.
        assert_eq!(advance(&mut emitter, sleep, 412, &all), []);
        assert!(emitter.emitted.is_empty());

        // A frame that took long runs the last ticks only, and a clock that went back none.
        let mut late = Emitter::default();
        assert_eq!(advance(&mut late, swim, 0, &all), []);
        assert_eq!(advance(&mut late, swim, 8, &all), []);
        // Ticks 9 to 12 are skipped, 13 to 16 looked at: the bubble of tick 10 is missed.
        assert_eq!(advance(&mut late, swim, 16, &all), []);
        assert_eq!(advance(&mut late, swim, 5, &all), []);
        assert_eq!(late.age, 5);
        assert_eq!(advance(&mut late, swim, 10, &all), [bubble(6)]);

        // A model whose poses start nothing.
        assert!(!anim::Animator::still().emits());
    }
}
