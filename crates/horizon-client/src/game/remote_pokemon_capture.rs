//! A Poké Ball thrown at a Pokémon and the capture it attempts (CobbleLegacy's fork).
//!
//! The Ball is an entity of Cobblemon's (`EmptyPokeBallEntity`), sent like a Pokémon in a message of its
//! own (`cobblemon:spawn_empty_pokeball_entity`) and followed by vanilla's packets for its id. It is a
//! thrown projectile, which a client flies itself between the server's words: its own gravity and drag,
//! a block in its way stopping it, the server's position replacing its own whenever one comes
//! (`ThrowableProjectile.tick`). Its data tells the capture (`CAPTURE_STATE`, `SHAKE`), and
//! `cobblelegacy_cobblemon::capture::Display` what the Ball's model plays for it.
//!
//! The Pokémon it takes is drawn by `remote_pokemon_phase.rs`, as one called back whose beam ends at the
//! Ball. A battle's tile shows the same capture as a Ball in place of the portrait
//! (`remote_pokemon_battle.rs`).
//!
//! The files a Ball's look is made of are read ahead, on a thread of their own, as soon as Cobblemon's
//! resources are known ([`Captures::read_ahead`]): read as a Ball was first thrown, they held the frame
//! for as long as the disk took, seconds when it was cold.
//!
//! Not here yet: the Ball's particles (the stars of a capture: `capturestar`, `capturesparks`,
//! `afterspark` on the model's `center_particles` locator, the flash of a Pokémon breaking out) and its
//! own sounds (`poke_ball.open`, `.shut`, `.bounce`, `.shake`, `.capture_succeeded`), which Cobblemon
//! plays from the keyframes of these animations: [`Ball::effects`] is where they start. The old Balls of
//! Hisui shake by animations the server names in a packet of their own (`play_posable_animation`), which
//! is not read: they sit still through their shakes.
use super::super::super::Game;
use super::super::Tracked;
use super::{Assets, path_of};
use cobblelegacy_cobblemon::{
    animation::AnimationFile,
    capture::{
        BallData, CaptureState, Clip, Display, Shown, SpawnPokeBall, TILE_BALL_AT, TileShows, tile,
    },
    molang::Context,
};
use glam::{DVec3, Mat4};
use horizon_client::hud::HudBatch;
use horizon_client::mesher::Vertex;
use horizon_client::mobs::{
    Mob,
    anim::{Poses, State, part_key},
    own::{self, OwnAnimation},
};
use horizon_core::geometry::Vec3;
use horizon_protocol::entity_data::Value;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Instant,
};

/// `ThrowableProjectile.getDefaultGravity`, blocks a tick each tick.
const GRAVITY: f64 = 0.03;
/// What a thrown projectile keeps of its speed each tick, in the air.
const DRAG: f64 = 0.99;
/// The Ball's middle above its base: `PokeBallRenderer` draws the model at 0.7 of its 8 pixels.
const MIDDLE: f64 = 0.175;
/// Ticks run at most in one frame, after a long one.
const CATCH_UP: u64 = 10;
/// `PokeBallRenderer`, and `PokemonRenderer.drawPokeBall`: a Ball in the world is drawn at this part
/// of its model's size, which its look is made at.
const WORLD_SCALE: f32 = 0.7;
/// `MoveTileOffscreenAnimation`: how long a caught Pokémon's tile takes to slide off the screen.
pub(super) const TILE_LEAVES: f32 = 0.75;

/// One thrown Ball.
struct Ball {
    /// As its textures name it: `poke_ball`.
    name: String,
    position: DVec3,
    previous: DVec3,
    /// Blocks a tick.
    velocity: DVec3,
    /// Degrees, the way the model faces.
    yaw: f32,
    previous_yaw: f32,
    no_gravity: bool,
    /// Ticks since it appeared: `PosableState.age`.
    age: u32,
    /// The position the server last gave, as the level's tracker holds it.
    told: [f64; 3],
    /// `HIT_TARGET_POSITION`: where it hit the Pokémon, which it faces from then on.
    hit_target: Option<DVec3>,
    display: Display,
    /// How many times it shook: which wobble the next shake is.
    shakes: u32,
}

/// The yaw of a Ball that goes along `x`, `z`: `Mth.atan2(x, z)` in degrees.
fn heading(x: f64, z: f64) -> f32 {
    x.atan2(z).to_degrees() as f32
}

impl Ball {
    fn new(spawn: &SpawnPokeBall) -> Self {
        let position = DVec3::from_array(spawn.entity.position);
        let name = path_of(&spawn.ball).to_owned();
        Self {
            display: Display::thrown(name.contains("beast")),
            name,
            position,
            previous: position,
            velocity: DVec3::from_array(spawn.entity.velocity),
            yaw: spawn.entity.yaw,
            previous_yaw: spawn.entity.yaw,
            no_gravity: false,
            age: 0,
            told: spawn.entity.position,
            hit_target: None,
            shakes: 0,
        }
    }

    /// The Ball's own seconds, `partial` of a tick into this one.
    fn seconds(&self, partial: f32) -> f32 {
        (self.age as f32 + partial) / 20.0
    }

    /// `ThrowableProjectile.tick` and `EmptyPokeBallEntity.tick` on a client. `told` is where the
    /// server last said the Ball is; `blocked` whether a block is in the way between two points.
    /// Answers whether the server moved the Ball, and whether a block stopped it.
    fn tick(
        &mut self,
        told: Option<[f64; 3]>,
        blocked: impl Fn(DVec3, DVec3) -> bool,
    ) -> (bool, bool) {
        let (mut moved_by_server, mut stopped) = (false, false);
        self.previous = self.position;
        self.previous_yaw = self.yaw;
        self.age += 1;
        // `Entity`'s interpolation is none: the server's position replaces the Ball's own at once.
        if let Some(told) = told
            && told != self.told
        {
            self.told = told;
            self.position = DVec3::from_array(told);
            moved_by_server = true;
        }
        // `onHitBlock`: a Ball in a capture stops there and falls again; one that hit nothing is the
        // server's to remove, and waits where it is.
        if self.velocity != DVec3::ZERO && blocked(self.position, self.position + self.velocity) {
            self.velocity = DVec3::ZERO;
            if self.display.state() != CaptureState::Not {
                self.no_gravity = false;
            }
            stopped = true;
        }
        let moved = self.velocity;
        self.position += moved;
        self.velocity *= DRAG;
        if !self.no_gravity {
            self.velocity.y -= GRAVITY;
        }
        match self.hit_target {
            // It looks at what it hit.
            Some(target) => {
                let to = target - self.position;
                self.yaw = heading(to.x, to.z);
            }
            // `Projectile.updateRotation`: a fifth of the way to where it goes, each tick.
            None if moved.x != 0.0 || moved.z != 0.0 => {
                let wanted = heading(moved.x, moved.z);
                self.yaw += (wanted - self.yaw + 180.0).rem_euclid(360.0) - 180.0;
                self.yaw = self.previous_yaw + (self.yaw - self.previous_yaw) * 0.2;
            }
            None => {}
        }
        (moved_by_server, stopped)
    }

    /// Where the Ball is, for a test's log.
    fn place(&self) -> String {
        let DVec3 { x, y, z } = self.position;
        format!("{x:.3} {y:.3} {z:.3}")
    }

    /// `CAPTURE_STATE` as the server says it.
    fn state(&mut self, state: CaptureState) {
        if let Some(falls) = state.gravity() {
            self.no_gravity = !falls;
        }
        let before = self.display.state();
        self.display.set(state, self.seconds(0.0));
        if before != state {
            self.effects(state);
        }
    }

    /// `SHAKE` flipped: one of the six wobbles, another each time.
    fn shake(&mut self) {
        self.shakes += 1;
        self.display.shake(self.seconds(0.0), wobble(self.shakes));
    }

    /// Where the particles and the sounds of a capture start, once they are drawn and played: the
    /// keyframes of the animation the Ball begins as it enters `state` (`sound_effects` and
    /// `particle_effects` in `poke_ball.animation.json`). Nothing yet.
    #[allow(clippy::unused_self)]
    fn effects(&self, _state: CaptureState) {}

    /// Where the Ball is `partial` of a tick into this one, and how it is turned (degrees).
    fn at(&self, partial: f64) -> (DVec3, f32) {
        let turn = (self.yaw - self.previous_yaw + 180.0).rem_euclid(360.0) - 180.0;
        (
            self.previous.lerp(self.position, partial),
            self.previous_yaw + turn * partial as f32,
        )
    }
}

/// Which of the six wobbles the `shake`th shake of a Ball is. Cobblemon draws one at random each time;
/// here they follow one another, never the same twice in a row.
fn wobble(shake: u32) -> u8 {
    (shake.wrapping_mul(5) % 6) as u8 + 1
}

/// A Ball's model playing its animations: `PokeBallModel` and `AncientPokeBallModel`. One serves every
/// Ball of a shape; each draw says what to play in the mob it draws ([`play`]).
pub(super) struct BallAnimator {
    file: AnimationFile,
    /// By [`Clip::index`]: the animation's place in the file, and for each of its bones the key of the
    /// part it poses. None for a clip the file lacks.
    clips: Vec<Option<(usize, Vec<u64>)>>,
}

impl BallAnimator {
    /// The animations of the Balls shaped `shape` (`poke_ball`, `ancient_poke_ball`), from their
    /// file's text.
    fn load(json: &str, shape: &str) -> Option<Self> {
        match AnimationFile::parse(json) {
            Ok(file) => Some(Self::new(file, shape)),
            Err(e) => {
                log::warn!("Horizon: Cobblemon animations of {shape}: {}", e.0);
                None
            }
        }
    }

    fn new(file: AnimationFile, shape: &str) -> Self {
        let clips = Clip::ALL
            .iter()
            .map(|clip| {
                let wanted = format!("animation.{shape}.{}", clip.name());
                let index = file.animations.iter().position(|a| a.name == wanted)?;
                let parts = file.animations[index]
                    .bones
                    .iter()
                    .map(|b| part_key(&b.bone))
                    .collect();
                Some((index, parts))
            })
            .collect();
        Self { file, clips }
    }

    /// `clip` (by [`Clip::index`]) `seconds` after it started, on top of what poses the model so far.
    /// `state_seconds` is the Ball's own time, which `q.anim_time` reads.
    fn run(&self, clip: usize, seconds: f32, state_seconds: f32, poses: &mut Poses) {
        let Some(Some((index, parts))) = self.clips.get(clip) else {
            return;
        };
        let context = Context {
            anim_time: state_seconds,
            age_in_ticks: state_seconds * 20.0,
            limb_swing: 0.0,
            limb_swing_amount: 0.0,
        };
        self.file.animations[*index].sample(seconds, &context, &mut |bone, delta| {
            if let Some(pose) = poses.by_key(parts[bone]) {
                delta.apply(pose);
            }
        });
    }
}

/// What [`BallAnimator`] reads in the mob it poses: the pose's animation and the one over it, each a
/// [`Clip::index`] from 1 (0: none), and the seconds the second has run. A Pokémon's `variant2` is its
/// tint, which a Ball has none of.
fn play(mob: &mut Mob, shown: Shown) {
    mob.extra.variant = shown.idle.map_or(0, |(clip, _)| clip.index() as u8 + 1);
    mob.extra.variant3 = shown.over.map_or(0, |(clip, _)| clip.index() as u8 + 1);
    mob.extra.vector[0] = shown.over.map_or(0.0, |(_, seconds)| f64::from(seconds));
}

impl OwnAnimation for BallAnimator {
    fn setup(&self, mob: &Mob, state: &State, poses: &mut Poses) {
        // `PosableState.animationSeconds`: the Ball's age, with the partial tick.
        let seconds = state.age / 20.0;
        if let Some(idle) = mob.extra.variant.checked_sub(1) {
            self.run(usize::from(idle), seconds, seconds, poses);
        }
        if let Some(over) = mob.extra.variant3.checked_sub(1) {
            self.run(
                usize::from(over),
                mob.extra.vector[0] as f32,
                seconds,
                poses,
            );
        }
    }
}

/// `ClientBallDisplay`: the Ball a battle's tile shows in place of the portrait of the Pokémon it was
/// thrown at. It keeps the wall's time, as the tiles do.
pub(super) struct TileBall {
    pokemon: u128,
    /// As its textures name it: `poke_ball`.
    name: String,
    since: Instant,
    display: Display,
    shakes: u32,
}

/// What a tile draws of its [`TileBall`] in one frame.
pub(super) struct TileFrame {
    /// The Pokémon whose tile it is.
    pub(super) pokemon: u128,
    name: String,
    /// The portrait still, or the Ball by now, and at what part of its size.
    pub(super) shows: TileShows,
    pub(super) size: f32,
    shown: Shown,
    seconds: f32,
}

impl TileBall {
    /// The Ball `ball` (`cobblemon:poke_ball`), which just hit `pokemon`.
    pub(super) fn new(pokemon: u128, ball: &str, now: Instant) -> Self {
        Self {
            pokemon,
            name: path_of(ball).to_owned(),
            since: now,
            display: Display::in_battle(),
            shakes: 0,
        }
    }

    pub(super) fn pokemon(&self) -> u128 {
        self.pokemon
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// The Ball takes the portrait's place a moment after the hit, and bounces as it does.
    fn land(&mut self, seconds: f32) {
        if seconds >= TILE_BALL_AT {
            self.display.set(CaptureState::Shake, TILE_BALL_AT);
        }
    }

    /// `BattleCaptureShakeHandler`: the server says the Ball shook.
    pub(super) fn shake(&mut self, now: Instant) {
        let seconds = now.saturating_duration_since(self.since).as_secs_f32();
        self.land(seconds);
        self.shakes += 1;
        self.display.shake(seconds, wobble(self.shakes));
    }

    pub(super) fn frame(&mut self, now: Instant) -> TileFrame {
        let seconds = now.saturating_duration_since(self.since).as_secs_f32();
        self.land(seconds);
        let (shows, size) = tile(seconds);
        TileFrame {
            pokemon: self.pokemon,
            name: self.name.clone(),
            shows,
            size,
            shown: self.display.shown(seconds),
            seconds,
        }
    }
}

/// The Balls thrown in the level.
#[derive(Default)]
pub(super) struct Captures {
    balls: HashMap<i32, Ball>,
    /// The client's tick the Balls were last flown at.
    ticked: u64,
    /// The animations of each shape of Ball, registered once (`own::register` keeps them for good).
    /// `own::NONE` for a shape whose file does not load.
    animators: HashMap<String, u32>,
    /// The files of the Balls' looks, by their path under `assets/cobblemon`, once the thread that
    /// reads them ahead is done. None until it is started.
    files: Option<Arc<OnceLock<HashMap<String, Vec<u8>>>>>,
}

/// Where the files of the Balls' looks are, under `assets/cobblemon`: their two geometries, their two
/// sets of animations, a texture a Ball.
const BALL_FOLDERS: [&str; 3] = [
    "bedrock/poke_balls/models",
    "bedrock/poke_balls/animations",
    "textures/poke_balls",
];

/// Every file of the Balls' looks, from the root that wins among those that have it.
fn read_ball_files(assets: &Assets) -> HashMap<String, Vec<u8>> {
    let mut read = HashMap::new();
    for folder in BALL_FOLDERS {
        for root in &assets.assets {
            let Ok(entries) = std::fs::read_dir(root.join(folder)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let path = format!("{folder}/{name}");
                if !read.contains_key(&path)
                    && let Ok(bytes) = std::fs::read(entry.path())
                {
                    read.insert(path, bytes);
                }
            }
        }
    }
    read
}

impl Captures {
    pub(super) fn forget(&mut self, id: i32) {
        self.balls.remove(&id);
    }

    /// Whether entity `id` is a thrown Ball.
    pub(super) fn has(&self, id: i32) -> bool {
        self.balls.contains_key(&id)
    }

    /// The Balls in the level, as their textures name them: they keep their slots in the atlas.
    pub(super) fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.balls.values().map(|ball| ball.name.as_str())
    }

    /// Where a beam that takes a Pokémon into Ball `id` ends: Cobblemon's `beam` locator, in the Ball's
    /// lid; its middle here.
    pub(super) fn beam_end(&self, id: i32) -> Option<DVec3> {
        self.balls
            .get(&id)
            .map(|ball| ball.position + DVec3::Y * MIDDLE)
    }

    /// Starts reading the files of the Balls' looks, once: by the time a Ball is thrown they are in
    /// memory, and making its look reads nothing.
    pub(super) fn read_ahead(&mut self, assets: &Arc<Assets>) {
        if self.files.is_some() {
            return;
        }
        let files = Arc::new(OnceLock::new());
        self.files = Some(files.clone());
        let assets = assets.clone();
        let spawned = std::thread::Builder::new()
            .name("cobblemon balls".into())
            .spawn(move || {
                // Set once here and nowhere else: it cannot be there already.
                let _ = files.set(read_ball_files(&assets));
            });
        if let Err(e) = spawned {
            log::warn!(
                "Horizon: no thread to read the Balls' models on ({e}): they are read as thrown"
            );
        }
    }

    /// The file at `path` under `assets/cobblemon`, one of a Ball's look: as it was read ahead, or
    /// from the disk while that is not done.
    pub(super) fn file(&self, assets: &Assets, path: &str) -> Option<Vec<u8>> {
        let ahead = self.files.as_ref().and_then(|files| files.get());
        match ahead.and_then(|read| read.get(path)) {
            Some(bytes) => Some(bytes.clone()),
            None => std::fs::read(assets.file(path)?).ok(),
        }
    }

    /// The number of the animations of the Balls shaped `shape`, for a type's `Anim::Own`.
    pub(super) fn animator(&mut self, assets: &Assets, shape: &str) -> u32 {
        if let Some(known) = self.animators.get(shape) {
            return *known;
        }
        let path = format!("bedrock/poke_balls/animations/{shape}.animation.json");
        let animator = self
            .file(assets, &path)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|json| BallAnimator::load(&json, shape))
            .map_or(own::NONE, |animator| own::register(Arc::new(animator)));
        self.animators.insert(shape.to_owned(), animator);
        animator
    }

    /// The server's motion for entity `id` (`Entity.lerpMotion`). False when it is no Ball.
    pub(super) fn motion(&mut self, id: i32, velocity: [f64; 3]) -> bool {
        let Some(ball) = self.balls.get_mut(&id) else {
            return false;
        };
        ball.velocity = DVec3::from_array(velocity);
        let [x, y, z] = velocity;
        trace(id, &format!("is given the motion {x:.3} {y:.3} {z:.3}"));
        true
    }

    /// What of a Ball's data vanilla's readers know: whether it falls, the capture's state, a shake.
    pub(super) fn vanilla_data(&mut self, id: i32, values: &[(u8, i32, Value)]) {
        let Some(ball) = self.balls.get_mut(&id) else {
            return;
        };
        for (index, _, value) in values {
            match (*index, value) {
                (BallData::NO_GRAVITY, Value::Bool(none)) => {
                    ball.no_gravity = *none;
                    trace(id, if *none { "is held up" } else { "falls" });
                }
                (BallData::CAPTURE_STATE, Value::Byte(state)) => {
                    let Some(state) = CaptureState::of(*state) else {
                        continue;
                    };
                    ball.state(state);
                    trace(id, &format!("is now {state:?} at {}", ball.place()));
                }
                (BallData::SHAKE, Value::Bool(_)) => {
                    ball.shake();
                    trace(id, "shakes");
                }
                _ => {}
            }
        }
    }

    /// The rest of a Ball's data, from Cobblemon's own types on.
    fn own_data(&mut self, id: i32, data: &BallData) {
        let Some(ball) = self.balls.get_mut(&id) else {
            return;
        };
        // `Vec3.ZERO` is the field's default: nothing hit.
        if let Some(target) = data.hit_target.map(DVec3::from_array)
            && target != DVec3::ZERO
        {
            ball.hit_target = Some(target);
        }
        if data.shake.is_some() {
            ball.shake();
            trace(id, "shakes");
        }
    }
}

/// What a test reads in the log, a Ball's state being nothing `horizon-ctl` can ask for. Each line
/// ends with the wall's time in milliseconds, which a screenshot's own time is compared with.
fn trace(id: i32, what: &str) {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *TRACE.get_or_init(|| std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        log::info!("Horizon: Ball {id} {what} [{now}]");
    }
}

/// Under `HORIZON_COBBLEMON_DUMP=<folder>`, what the server sent of a capture is written there as it
/// came, numbered in the order it came: the tests' fixtures are made from these files.
pub(super) fn record(what: &str, data: &[u8]) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static FOLDER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let folder = FOLDER.get_or_init(|| {
        let folder = std::path::PathBuf::from(std::env::var_os("HORIZON_COBBLEMON_DUMP")?);
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder)
    });
    if let Some(folder) = folder {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        // As the fixtures of the Cobblemon crate are named: `flow-007-battle_capture_start.bin`.
        let name = format!("flow-{number:03}-{}.bin", path_of(what).replace('/', "_"));
        if let Err(e) = std::fs::write(folder.join(name), data) {
            log::warn!("Horizon: {what} is not written to the dump folder: {e}");
        }
    }
}

impl Game<'_> {
    /// `SpawnPokeballPacket.spawnAndApply`: a thrown Ball enters the level.
    pub(super) fn remote_pokemon_ball_spawn(&mut self, data: &[u8]) {
        record("spawn_empty_pokeball_entity", data);
        let spawn = match SpawnPokeBall::read(data) {
            Ok(spawn) => spawn,
            Err(e) => {
                log::warn!("Horizon: a Ball spawn that does not decode: {e}");
                return;
            }
        };
        let id = spawn.entity.id;
        // The AddEntity arm of `handle_packet`: an id the level holds already starts afresh.
        self.audio.remove_sound_entity(id as u32);
        self.persist.uuids.insert(
            id as u32,
            horizon_client::lan::playerdata::uuid_ints(spawn.entity.uuid),
        );
        self.detach_remote_tracking_entity(id);
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        r.ticked_before_player.remove(&(id as u32));
        r.level_player_textures.remove(&id);
        r.level_players.remove(&id);
        r.level_player_names.remove(&id);
        r.stepped.remove(&id);
        r.cart_lerps.remove(&id);
        r.lerps.remove(&id);
        // Known to the level, so that the moves vanilla's packets tell reach it.
        r.entities.insert(
            id,
            Tracked {
                kind: "cobblemon:empty_pokeball".to_owned(),
                position: spawn.entity.position,
                codec_base: spawn.entity.position,
                silent: false,
                vehicle: None,
            },
        );
        r.pokemon.forget(id);
        let ball = Ball::new(&spawn);
        trace(id, &format!("appears, a {}", ball.name));
        let name = ball.name.clone();
        r.pokemon.captures.balls.insert(id, ball);
        self.pokemon_ball(&name);
    }

    /// `SetEntityMotion` for a thrown Ball. False when `id` is none.
    pub(in super::super) fn pokemon_ball_motion(&mut self, id: i32, velocity: [f64; 3]) -> bool {
        self.remote
            .as_mut()
            .is_some_and(|r| r.pokemon.captures.motion(id, velocity))
    }

    /// A Ball's data from Cobblemon's own types on (`remote_pokemon_data`). False when `id` is no Ball.
    pub(super) fn remote_pokemon_ball_data(&mut self, id: i32, rest: &[u8]) -> bool {
        let Some(captures) = self.remote.as_mut().map(|r| &mut r.pokemon.captures) else {
            return false;
        };
        if !captures.has(id) {
            return false;
        }
        record("pokeball_data", rest);
        match BallData::read_rest(rest) {
            Ok(data) => captures.own_data(id, &data),
            Err(e) => log::warn!("Horizon: the data of Ball {id} does not decode: {e}"),
        }
        true
    }

    /// Once a frame: the Balls fly on by the ticks the client ran since the last, and each one's look
    /// is there to draw it.
    pub(super) fn pokemon_capture_frame(&mut self) {
        let now = self.ticks;
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        let captures = &mut pokemon.captures;
        // As soon as a Pokémon or the party had Cobblemon's resources looked for.
        if let Some(Some(assets)) = &pokemon.assets {
            captures.read_ahead(assets);
        }
        let ticks = now.saturating_sub(captures.ticked).min(CATCH_UP);
        captures.ticked = now;
        if captures.balls.is_empty() {
            return;
        }
        for _ in 0..ticks {
            self.pokemon_capture_tick();
        }
        // A look made when the Ball appeared may have lost its place in the atlas since.
        let names: Vec<String> = self
            .remote
            .as_ref()
            .map(|r| r.pokemon.captures.names().map(str::to_owned).collect())
            .unwrap_or_default();
        for name in names {
            self.pokemon_ball(&name);
        }
    }

    /// One tick of every Ball, among the level's blocks.
    fn pokemon_capture_tick(&mut self) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let mut balls = std::mem::take(&mut r.pokemon.captures.balls);
        let told: HashMap<i32, [f64; 3]> = balls
            .keys()
            .filter_map(|id| Some((*id, r.entities.get(id)?.position)))
            .collect();
        {
            let world = self.mob_world_base(&[]);
            let blocked = |from: DVec3, to: DVec3| {
                world
                    .clip_block(
                        Vec3::new(from.x, from.y, from.z),
                        Vec3::new(to.x, to.y, to.z),
                    )
                    .is_some()
            };
            for (id, ball) in &mut balls {
                let (moved_by_server, stopped) = ball.tick(told.get(id).copied(), blocked);
                if moved_by_server {
                    trace(*id, &format!("is told to be at {}", ball.place()));
                }
                if stopped {
                    trace(*id, &format!("is stopped by a block at {}", ball.place()));
                }
            }
        }
        if let Some(r) = self.remote.as_mut() {
            // A Ball that appeared during the tick is kept.
            balls.extend(std::mem::take(&mut r.pokemon.captures.balls));
            r.pokemon.captures.balls = balls;
        }
    }

    /// `BattleOverlay.drawPokeBall`: the Ball of a battle's tile, where the portrait was. `origin` is
    /// where a portrait's drawing starts from, the middle of the frame's top edge five pixels up.
    pub(super) fn draw_pokemon_tile_ball(
        &self,
        batch: &mut HudBatch,
        frame: &TileFrame,
        origin: (f32, f32),
        reversed: bool,
        clip: [f32; 4],
    ) {
        use glam::Vec3 as V;
        let Some(kind) = self
            .remote
            .as_ref()
            .and_then(|r| r.pokemon.phases.ball(&frame.name))
        else {
            return;
        };
        // Stood at the origin and facing the viewer, as a portrait's model is.
        let mut mob = Mob::new(0, kind.clone(), Vec3::new(0.0, 0.0, 0.0), 180.0);
        mob.previous = mob.position;
        (mob.body_yaw, mob.previous_body_yaw) = (180.0, 180.0);
        (mob.head_yaw, mob.previous_head_yaw) = (180.0, 180.0);
        mob.extra.model_scale = 1.0;
        let ticks = frame.seconds * 20.0;
        mob.age = ticks as u32;
        play(&mut mob, frame.shown);
        let side: f32 = if reversed { -1.0 } else { 1.0 };
        // The look is the world's, smaller than the model; the tile draws the model itself.
        let whole = 1.0 / WORLD_SCALE;
        let by = 5.0 * frame.size;
        let gui = Mat4::from_translation(V::new(origin.0, origin.1, 0.0))
            * Mat4::from_scale(V::new(5.0, 5.0, -5.0))
            * Mat4::from_translation(V::new(0.0, 5.5, -4.0))
            * Mat4::from_scale(V::new(by, by, 0.1))
            * Mat4::from_rotation_y((-32.0 * side).to_radians())
            * Mat4::from_rotation_x(5f32.to_radians())
            * Mat4::from_scale(V::new(-whole, -whole, whole));
        self.draw_pokemon_model(batch, &mob, ticks.fract(), gui, clip);
    }

    /// `PokeBallRenderer.render`: the thrown Balls, among the frame's mobs.
    pub(super) fn pokemon_capture_geometry(
        &self,
        camera: DVec3,
        alpha: f64,
        out: &mut Vec<Vertex>,
    ) {
        let Some(r) = self.remote.as_ref() else {
            return;
        };
        let captures = &r.pokemon.captures;
        if captures.balls.is_empty() {
            return;
        }
        let atlas = &self.gui.atlas;
        let lookup = |name: &str| atlas.sprites.get(name).map(|r| [r.u0, r.v0, r.u1, r.v1]);
        for ball in captures.balls.values() {
            let Some(kind) = r.pokemon.phases.ball(&ball.name) else {
                continue;
            };
            let seconds = ball.seconds(alpha as f32);
            // Burst open and shrunk to nothing: the server removes it a moment later.
            if ball.display.gone(seconds) {
                continue;
            }
            let (at, yaw) = ball.at(alpha);
            let mut mob = Mob::new(u32::MAX, kind.clone(), Vec3::new(at.x, at.y, at.z), 0.0);
            mob.previous = mob.position;
            (mob.yaw, mob.previous_yaw) = (0.0, 0.0);
            (mob.body_yaw, mob.previous_body_yaw) = (0.0, 0.0);
            mob.extra.model_scale = 1.0;
            mob.age = ball.age;
            play(&mut mob, ball.display.shown(seconds));
            let origin = Mat4::from_translation((at - camera).as_vec3())
                * Mat4::from_rotation_y(yaw.to_radians());
            let light = self.world.light(
                at.x.floor() as i32,
                (at.y + MIDDLE).floor() as i32,
                at.z.floor() as i32,
            );
            let packed = (u32::from(light >> 4) * 16) | ((u32::from(light & 15) * 16) << 8);
            horizon_client::mobs::render::render_posed(
                &mob,
                alpha as f32,
                origin,
                &lookup,
                packed,
                self.res.mob_data(),
                out,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobblelegacy_cobblemon::wire::AddEntity;

    fn thrown(velocity: [f64; 3]) -> Ball {
        Ball::new(&SpawnPokeBall {
            ball: "cobblemon:poke_ball".into(),
            aspects: Vec::new(),
            entity: AddEntity {
                id: 7,
                uuid: 0,
                kind: 131,
                position: [0.0, 10.0, 0.0],
                pitch: 0.0,
                yaw: 0.0,
                head_yaw: 0.0,
                data: 0,
                velocity,
            },
        })
    }

    #[test]
    fn a_ball_flies_on_by_itself_and_the_server_s_word_replaces_its_own() {
        let mut ball = thrown([0.0, 0.0, 1.0]);
        assert_eq!(ball.name, "poke_ball");
        ball.tick(Some([0.0, 10.0, 0.0]), |_, _| false);
        // Moved by its speed, which then loses a hundredth and takes the fall.
        assert_eq!(ball.position, DVec3::new(0.0, 10.0, 1.0));
        assert!((ball.velocity - DVec3::new(0.0, -0.03, 0.99)).length() < 1.0e-9);
        ball.tick(Some([0.0, 10.0, 0.0]), |_, _| false);
        assert!((ball.position - DVec3::new(0.0, 9.97, 1.99)).length() < 1.0e-9);
        // The server says where it is: from there, at once.
        ball.tick(Some([0.0, 9.0, 3.0]), |_, _| false);
        assert!((ball.position.z - (3.0 + 0.99 * 0.99)).abs() < 1.0e-9);
        assert_eq!(ball.previous.z, 1.99);
        assert_eq!(ball.age, 3);
        // Half way through that tick.
        let (at, _) = ball.at(0.5);
        assert!((at.z - (1.99 + ball.position.z) / 2.0).abs() < 1.0e-9);
        // The server's motion replaces its own.
        let mut captures = Captures::default();
        captures.balls.insert(7, ball);
        assert!(captures.motion(7, [0.1, 0.0, 0.0]) && !captures.motion(8, [0.0; 3]));
        assert_eq!(captures.balls[&7].velocity, DVec3::new(0.1, 0.0, 0.0));
    }

    #[test]
    fn a_ball_turns_the_way_it_goes_then_faces_what_it_hit() {
        // Thrown south (yaw 0) but going east: a fifth of the quarter turn each tick.
        let mut ball = thrown([1.0, 0.0, 0.0]);
        ball.tick(None, |_, _| false);
        assert!((ball.yaw - 18.0).abs() < 1.0e-4, "{}", ball.yaw);
        ball.tick(None, |_, _| false);
        assert!((ball.yaw - 32.4).abs() < 1.0e-3, "{}", ball.yaw);
        // Half way between the two, half a tick in.
        assert!((ball.at(0.5).1 - 25.2).abs() < 1.0e-3);
        // Once it hit, it looks at the place: north of it here.
        let mut captures = Captures::default();
        let at = ball.position;
        captures.balls.insert(7, ball);
        captures.own_data(
            7,
            &BallData {
                hit_target: Some([at.x, at.y, at.z - 2.0]),
                ..Default::default()
            },
        );
        let ball = captures.balls.get_mut(&7).unwrap();
        ball.velocity = DVec3::ZERO;
        ball.no_gravity = true;
        ball.tick(None, |_, _| false);
        assert!((ball.yaw.abs() - 180.0).abs() < 1.0e-3, "{}", ball.yaw);
        // The default, no place at all, is none.
        let mut other = Captures::default();
        other.balls.insert(7, thrown([0.0; 3]));
        other.own_data(
            7,
            &BallData {
                hit_target: Some([0.0; 3]),
                ..Default::default()
            },
        );
        assert_eq!(other.balls[&7].hit_target, None);
    }

    #[test]
    fn the_capture_s_states_stop_the_ball_and_let_it_fall() {
        let mut captures = Captures::default();
        captures.balls.insert(7, thrown([0.0, 0.0, 0.5]));
        let byte = |state: i8| [(BallData::CAPTURE_STATE, 0, Value::Byte(state))];
        captures.vanilla_data(7, &byte(1));
        let ball = captures.balls.get_mut(&7).unwrap();
        assert_eq!(ball.display.state(), CaptureState::Hit);
        // Held in the air while the Pokémon goes in: the server says so.
        captures.vanilla_data(7, &[(BallData::NO_GRAVITY, 0, Value::Bool(true))]);
        assert!(captures.motion(7, [0.0; 3]));
        let ball = captures.balls.get_mut(&7).unwrap();
        ball.tick(None, |_, _| false);
        assert_eq!(ball.position, DVec3::new(0.0, 10.0, 0.0));
        // FALL lets it go, by the state alone; the ground stops it short of the block.
        captures.vanilla_data(7, &byte(2));
        let ball = captures.balls.get_mut(&7).unwrap();
        assert!(!ball.no_gravity);
        let ground = |_: DVec3, to: DVec3| to.y < 9.5;
        for _ in 0..40 {
            ball.tick(None, ground);
        }
        assert!(
            ball.position.y >= 9.5 && ball.position.y < 9.8,
            "{}",
            ball.position.y
        );
        // SHAKE holds it there; a shake picks a wobble, and only then.
        captures.vanilla_data(7, &byte(4));
        captures.vanilla_data(7, &[(BallData::SHAKE, 0, Value::Bool(true))]);
        let ball = &captures.balls[&7];
        assert_eq!(ball.shakes, 1);
        let shown = ball.display.shown(ball.seconds(0.0));
        assert_eq!(shown.over.map(|o| o.0), Some(Clip::Bob(wobble(1))));
        // A state outside the seven, another entity's data: nothing.
        captures.vanilla_data(7, &byte(9));
        captures.vanilla_data(8, &byte(5));
        assert_eq!(captures.balls[&7].display.state(), CaptureState::Shake);
        assert_eq!(captures.beam_end(7).map(|p| p.x), Some(0.0));
        captures.forget(7);
        assert!(!captures.has(7) && captures.beam_end(7).is_none());
    }

    #[test]
    fn a_tile_s_ball_takes_the_portrait_s_place_then_shakes() {
        let start = Instant::now();
        let at = |seconds: f32| start + std::time::Duration::from_secs_f32(seconds);
        let mut ball = TileBall::new(9, "cobblemon:master_ball", start);
        assert_eq!((ball.pokemon(), ball.name()), (9, "master_ball"));
        // The portrait first, whole then shrinking; a shake that early is none.
        let frame = ball.frame(at(0.5));
        assert_eq!(
            (frame.shows, frame.size, frame.pokemon),
            (TileShows::Pokemon, 1.0, 9)
        );
        ball.shake(at(0.6));
        assert_eq!(ball.frame(at(0.7)).shown.over, None);
        let frame = ball.frame(at(1.15));
        assert!(frame.shows == TileShows::Pokemon && (frame.size - 0.5).abs() < 1.0e-3);
        // Then the Ball, growing as it bounces.
        let frame = ball.frame(at(1.45));
        assert!(frame.shows == TileShows::Ball && (frame.size - 0.5).abs() < 1.0e-3);
        assert_eq!(frame.shown.over.map(|o| o.0), Some(Clip::Bounce));
        assert_eq!(frame.shown.idle.map(|i| i.0), Some(Clip::ShutIdle));
        // A shake the server tells is one of the wobbles, from its start.
        ball.shake(at(3.0));
        let frame = ball.frame(at(3.25));
        let (clip, seconds) = frame.shown.over.unwrap();
        assert!(clip == Clip::Bob(wobble(2)) && (seconds - 0.25).abs() < 1.0e-3);
        assert_eq!((frame.shows, frame.size), (TileShows::Ball, 1.0));
        // A shake that comes before any frame drew the Ball still finds it landed.
        let mut unseen = TileBall::new(9, "poke_ball", start);
        unseen.shake(at(2.0));
        assert_eq!(
            unseen.frame(at(2.0)).shown.over.map(|o| o.0),
            Some(Clip::Bob(wobble(1)))
        );
    }

    #[test]
    fn a_ball_s_files_are_read_ahead_from_the_root_that_wins() {
        let folder = std::env::temp_dir().join(format!("horizon-balls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let write = |root: &str, path: &str, bytes: &[u8]| {
            let file = folder.join(root).join("assets/cobblemon").join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        };
        // Cobblemon's own resources, and a pack over them that repaints one Ball.
        write("mod", "textures/poke_balls/poke_ball.png", b"red");
        write("mod", "textures/poke_balls/great_ball.png", b"blue");
        write("mod", "bedrock/poke_balls/models/poke_ball.geo.json", b"{}");
        write("pack", "textures/poke_balls/poke_ball.png", b"gold");
        let assets = Assets {
            assets: ["pack", "mod"]
                .map(|root| folder.join(root).join("assets/cobblemon"))
                .to_vec(),
            models: HashMap::new(),
            posers: HashMap::new(),
            resolvers: HashMap::new(),
            species: HashMap::new(),
            animations: HashMap::new(),
            animation_files: Default::default(),
        };
        let read = read_ball_files(&assets);
        assert_eq!(read.len(), 3);
        assert_eq!(read["textures/poke_balls/poke_ball.png"], b"gold");
        assert_eq!(read["textures/poke_balls/great_ball.png"], b"blue");
        assert_eq!(read["bedrock/poke_balls/models/poke_ball.geo.json"], b"{}");
        // Before the reading ahead is done, and for a file it did not find, the disk answers.
        let mut captures = Captures::default();
        let poke_ball = "textures/poke_balls/poke_ball.png";
        assert_eq!(captures.file(&assets, poke_ball), Some(b"gold".to_vec()));
        let ahead = Arc::new(OnceLock::new());
        ahead
            .set(HashMap::from([(poke_ball.to_owned(), b"kept".to_vec())]))
            .unwrap();
        captures.files = Some(ahead);
        assert_eq!(captures.file(&assets, poke_ball), Some(b"kept".to_vec()));
        assert_eq!(
            captures.file(&assets, "textures/poke_balls/great_ball.png"),
            Some(b"blue".to_vec())
        );
        assert_eq!(captures.file(&assets, "textures/poke_balls/none.png"), None);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_wobbles_follow_one_another_and_never_repeat() {
        let mut seen = std::collections::HashSet::new();
        for shake in 1..=6 {
            let now = wobble(shake);
            assert!((1..=6).contains(&now));
            assert_ne!(now, wobble(shake + 1));
            seen.insert(now);
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn a_draw_tells_the_animator_what_to_play() {
        let file = AnimationFile::parse(
            r#"{"animations": {
                "animation.poke_ball.shut_idle": {"loop": true, "bones": {"lid": {"rotation": [10, 0, 0]}}},
                "animation.poke_ball.bob2": {"animation_length": 1, "bones": {"bottom": {"position": [0, 2, 0]}}}
            }}"#,
        )
        .unwrap();
        let animator = BallAnimator::new(file, "poke_ball");
        assert!(animator.clips[Clip::ShutIdle.index()].is_some());
        assert!(animator.clips[Clip::Bob(2).index()].is_some());
        assert!(animator.clips[Clip::Throw.index()].is_none());
        let model = horizon_client::mobs::Part {
            name: "ball".into(),
            pose: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            polygons: Vec::new(),
            children: ["bottom", "lid"]
                .into_iter()
                .map(|name| horizon_client::mobs::Part {
                    name: name.into(),
                    pose: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                    polygons: Vec::new(),
                    children: Vec::new(),
                })
                .collect(),
        };
        let mut poses = Poses::new(&model);
        animator.run(Clip::ShutIdle.index(), 3.0, 3.0, &mut poses);
        animator.run(Clip::Bob(2).index(), 0.5, 3.0, &mut poses);
        // A clip the file lacks, or one that is over, poses nothing.
        animator.run(Clip::Throw.index(), 0.5, 3.0, &mut poses);
        animator.run(Clip::Bob(2).index(), 1.5, 3.0, &mut poses);
        let lid = *poses.by_key(part_key("lid")).unwrap();
        assert!((lid[3] - 10.0_f32.to_radians()).abs() < 1.0e-4);
        // A Bedrock position goes up, a part's down.
        assert_eq!(poses.by_key(part_key("bottom")).unwrap()[1], -2.0);
    }
}
