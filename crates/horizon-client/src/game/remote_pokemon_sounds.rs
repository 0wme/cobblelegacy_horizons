//! What is heard on a Cobblemon server (CobbleLegacy's fork).
//!
//! Cobblemon's sound events are no part of the game's resources. They are read where its Pokémon are
//! (`HORIZON_COBBLEMON_ROOT`): every `assets/<namespace>/sounds.json` of those roots and the files
//! under `sounds/`, on a thread of their own, once a run, then handed to the sound engine
//! (`Audio::add_sounds`). From then on the sounds the server plays by name find their event: a Ball
//! thrown, a Pokémon called back, a block of Cobblemon's (the protocol adapter names a mod's sound,
//! `horizon_protocol::adapter::fabric::Modded::sounds`).
//!
//! The rest the server does not send as sounds, and Cobblemon's client plays it itself:
//!
//! - a Pokémon coming out of its Ball (`PokemonClientDelegate`): the Ball's trail, which follows it in
//!   flight, and what it opens with (`remote_pokemon_phase.rs` says when);
//! - a cry, which is an animation the server names (`cobblemon:play_posable_animation`): the sound
//!   is a keyframe of that animation, read with the model (`Voice`). The animation itself is not
//!   played yet;
//! - a battle's music (`cobblemon:battle_music`), which holds the game's own music, ambience and
//!   records while it plays (`BattleMusicController`);
//! - the click of Cobblemon's screens.
//!
//! Not here: Cobblemon also keeps a sound of those three categories from starting while a battle's
//! music plays (`SoundManagerMixin`); only the music manager is held.
use super::super::super::Game;
use super::{Looked, anim};
use cobblelegacy_cobblemon::{
    animation::{AnimationFile, SoundKeyframe},
    poser::Poser,
    sound::{self, BattleMusic, MusicStep, NamedSounds, PlayAnimation},
};
use horizon_client::{
    audio::ExtraSounds,
    random_source,
    sound_engine::{
        AMBIENT, Attenuation, InstanceId, MUSIC, NEUTRAL, PLAYERS, RECORDS, SoundEntity,
        SoundInstance, Sounds as _, Ticking,
    },
    sound_events::SoundRegistry,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Once, OnceLock},
    time::{Duration, Instant},
};

/// `BattleMusicController.filteredCategories`: what waits while a battle's music plays.
const HELD_BY_MUSIC: [usize; 3] = [AMBIENT, MUSIC, RECORDS];

/// Cobblemon's sounds, once they are read.
static READ: OnceLock<Arc<ExtraSounds>> = OnceLock::new();

/// Starts reading Cobblemon's sounds, the first time it is called in a run. Nothing is read without
/// `HORIZON_COBBLEMON_ROOT`.
fn start_reading() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let Some(roots) = std::env::var_os("HORIZON_COBBLEMON_ROOT") else {
            return;
        };
        let roots: Vec<PathBuf> = std::env::split_paths(&roots).collect();
        let spawned = std::thread::Builder::new()
            .name("cobblemon sounds".into())
            .spawn(move || {
                let sounds = read_sounds(&roots);
                log::info!(
                    "Horizon: Cobblemon sounds from {} roots: {} events, {} files",
                    roots.len(),
                    sounds.registry.len(),
                    sounds.files.len()
                );
                let _ = READ.set(Arc::new(sounds));
            });
        if let Err(e) = spawned {
            log::warn!("Horizon: no thread to read Cobblemon's sounds on ({e}): they stay unknown");
        }
    });
}

/// Every file under `dir`, by its path from `base` with `/` (`sounds/poke_ball/throw.ogg`).
fn files_under(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(base, &path, out);
        } else if let Ok(relative) = path.strip_prefix(base) {
            out.push((relative.to_string_lossy().replace('\\', "/"), path));
        }
    }
}

/// The sound events of `roots` and where their files are. The roots stack as resource packs do, from
/// the one a file is taken from last to the one that wins: a file is the highest root's that has it,
/// and each root's `sounds.json` adds to what the roots under it gave an event, or replaces it when
/// it says so (`SoundManager.Preparations`). A sound whose file no root has is left out, as there.
fn read_sounds(roots: &[PathBuf]) -> ExtraSounds {
    let mut sounds = ExtraSounds::default();
    let mut events = Vec::new();
    for root in roots {
        let Ok(namespaces) = std::fs::read_dir(root.join("assets")) else {
            continue;
        };
        for namespace in namespaces.flatten() {
            let (folder, name) = (namespace.path(), namespace.file_name());
            let Some(name) = name.to_str() else {
                continue;
            };
            let mut found = Vec::new();
            files_under(&folder, &folder.join("sounds"), &mut found);
            for (path, file) in found {
                sounds.files.insert(format!("{name}:{path}"), file);
            }
            if let Ok(json) = std::fs::read(folder.join("sounds.json")) {
                events.push((name.to_owned(), json));
            }
        }
    }
    let files = &sounds.files;
    let exists = |namespace: &str, path: &str| files.contains_key(&format!("{namespace}:{path}"));
    let mut registry = SoundRegistry::default();
    for (namespace, json) in &events {
        registry.add_file(namespace, json, &exists);
    }
    sounds.registry = registry;
    sounds
}

/// The sounds of a look's named animations, with the pose each of the entity's pose types puts the
/// model in: what a cry plays, and when.
#[derive(Default)]
pub(super) struct Voice {
    named: NamedSounds,
    /// The place in the poser's poses of the pose of each `PoseType`, by ordinal.
    by_pose: [Option<usize>; 14],
}

impl Voice {
    /// `load` reads a Bedrock animation group, as `Animator::new`'s.
    pub(super) fn new(
        poser: &Poser,
        animator: &anim::Animator,
        load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>,
    ) -> Self {
        Self {
            named: NamedSounds::new(poser, load),
            by_pose: animator.by_pose(),
        }
    }

    /// The sounds of the first of `names` the model has an animation for, a Pokémon whose
    /// `Mob::extra.variant` is `variant` being in the pose it is.
    fn sounds(&self, variant: u8, names: &[String]) -> &[SoundKeyframe] {
        self.named
            .sounds(self.by_pose[anim::pose_of(variant) as usize], names)
    }
}

/// A sound that waits for its moment.
struct Later {
    due: Instant,
    /// The Pokémon it is about: it is not played once that Pokémon is gone.
    entity: i32,
    sound: String,
    volume: f32,
    pitch: f32,
    /// Where it plays (`ClientLevel.playSound` at a place, on the players' source); None for one
    /// that follows the Pokémon (`playLocalSound(entity, ..)`, on the entity's).
    at: Option<[f64; 3]>,
}

/// A Ball's trail (`MovingSoundInstance`).
struct Trail {
    instance: InstanceId,
    since: Instant,
    /// Where the Ball was last drawn (`sendOutPosition + sendOutOffset`), from the first frame its
    /// thrower is known. Until then the sound is where Cobblemon leaves it, at the world's origin.
    at: Option<[f64; 3]>,
}

/// A battle's music (`BattleMusicInstance`).
struct Music {
    instance: InstanceId,
    name: String,
    /// The volume it started at, which a fade starts from.
    volume: f32,
    /// Since when it fades out (`setFade`).
    fading: Option<Instant>,
    /// Whether the game's music, ambience and records were held for it.
    held: bool,
}

/// What this module keeps of the sounds of the Pokémon in view.
#[derive(Default)]
pub(super) struct Sounds {
    /// Whether the sound engine has Cobblemon's sounds.
    known: bool,
    later: Vec<Later>,
    trails: HashMap<i32, Trail>,
    /// `playedThrowingSound`: the Pokémon whose Ball has had its trail. It is never played twice for
    /// one entity.
    trailed: HashSet<i32>,
    /// The trails of the Pokémon that left the level, to stop.
    stopped: Vec<InstanceId>,
    music: Option<Music>,
    /// How many of the entries of the engine's `heard` have been looked at for a sound it lacks.
    checked: usize,
    /// The sounds asked for that the engine has no event or no file for, each told once in the log.
    unknown: BTreeSet<String>,
    /// How many times each sound was started from here (`horizon-ctl status`).
    played: BTreeMap<String, u32>,
    /// Where the last trail was last put, and for which Pokémon (`horizon-ctl status`).
    trail_at: Option<(i32, [f64; 3])>,
}

impl Sounds {
    pub(super) fn forget(&mut self, id: i32) {
        self.trailed.remove(&id);
        self.later.retain(|later| later.entity != id);
        if let Some(trail) = self.trails.remove(&id) {
            self.stopped.push(trail.instance);
        }
    }

    /// The Ball of Pokémon `id` is at `at` this frame.
    pub(super) fn ball_at(&mut self, id: i32, at: [f64; 3]) {
        if let Some(trail) = self.trails.get_mut(&id) {
            trail.at = Some(at);
            self.trail_at = Some((id, at));
        }
    }

    fn count(&mut self, sound: &str) {
        *self.played.entry(sound.to_owned()).or_default() += 1;
    }
}

/// How long a trail lasts: `MovingSoundInstance` stops itself on the tick its count is past its
/// duration.
fn trail_life() -> Duration {
    Duration::from_millis(u64::from(sound::TRAIL_TICKS + 2) * 50)
}

impl Game<'_> {
    /// Once a frame: Cobblemon's sounds given to the engine once they are read, the trails moved with
    /// their Balls, the sounds whose moment has come, a battle's music fading.
    pub(super) fn pokemon_sounds_frame(&mut self) {
        let now = Instant::now();
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let sounds = &mut r.pokemon.sounds;
        if !sounds.known {
            start_reading();
            if let Some(read) = READ.get() {
                self.audio.add_sounds(read.clone());
                sounds.known = true;
            }
        }
        for instance in sounds.stopped.drain(..) {
            self.audio.stop_instance(instance);
        }
        sounds.trails.retain(|_, trail| {
            if now.duration_since(trail.since) > trail_life() {
                self.audio.stop_instance(trail.instance);
                return false;
            }
            if let (Some(at), Some(instance)) =
                (trail.at, self.audio.engine.instance_mut(trail.instance))
            {
                instance.position = at;
            }
            true
        });
        // `BattleMusicInstance.tick`, by the clock: a tick is a twentieth of a second.
        if let Some(music) = &mut sounds.music
            && let Some(since) = music.fading
        {
            let ticks = (now.duration_since(since).as_secs_f32() * 20.0) as u32;
            match sound::faded(music.volume, ticks) {
                Some(volume) => {
                    if let Some(instance) = self.audio.engine.instance_mut(music.instance) {
                        instance.volume = volume;
                    }
                }
                // `isStopped`: done, and what it held plays on.
                None => {
                    self.audio.stop_instance(music.instance);
                    if music.held {
                        self.audio.resume_sources(&HELD_BY_MUSIC);
                    }
                    sounds.music = None;
                }
            }
        }
        // A sound a mod's server asked for that nothing plays, said once: the engine counts what it
        // is asked whether or not it has the event.
        if sounds.known && sounds.checked != self.audio.heard.len() {
            sounds.checked = self.audio.heard.len();
            let registry = &self.audio.engine.registry;
            for name in self.audio.heard.keys() {
                if !name.starts_with("minecraft:")
                    && name.contains(':')
                    && registry.weight(name) <= 0
                    && sounds.unknown.insert(name.clone())
                {
                    log::warn!("Horizon: unknown sound event {name}: nothing plays for it");
                }
            }
        }
        let due: Vec<Later> = if sounds.later.iter().any(|later| later.due <= now) {
            let (due, later) = std::mem::take(&mut sounds.later)
                .into_iter()
                .partition(|later| later.due <= now);
            sounds.later = later;
            due
        } else {
            return;
        };
        for later in due {
            self.pokemon_sound_now(later);
        }
    }

    /// Plays a sound whose moment has come, if its Pokémon is still there.
    fn pokemon_sound_now(&mut self, later: Later) {
        let Later {
            entity,
            sound,
            volume,
            pitch,
            at,
            ..
        } = later;
        if self.mob_index.get(&self.mobs, entity as u32).is_none() {
            return;
        }
        match at {
            Some(at) => self.audio.play_on(PLAYERS, &sound, Some(at), volume, pitch),
            None => {
                // `BedrockSoundKeyframe.run`: nothing for a silent entity.
                let Some(state) = self.remote_sound_entity_state(entity).filter(|s| !s.silent)
                else {
                    return;
                };
                let bound = SoundEntity::new(entity as u32, state.position, state.silent);
                bound.update(state);
                self.audio
                    .play_bound(&sound, NEUTRAL, volume, pitch, bound, None);
            }
        }
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.sounds.count(&sound);
        }
    }

    /// Where Pokémon `id` stands, whether it is shiny and whether it has an owner.
    fn pokemon_sound_facts(&self, id: i32) -> Option<([f64; 3], bool, bool)> {
        let spawn = self.remote.as_ref()?.pokemon.seen.get(&id)?;
        let mob = self.mobs.iter().find(|mob| mob.id == id as u32)?;
        Some((
            [mob.position.x, mob.position.y, mob.position.z],
            spawn.shiny,
            spawn.owner.is_some(),
        ))
    }

    /// `PokemonClientDelegate`, `BEAM_MODE` 1: Pokémon `id` is thrown out in its Ball, which lands
    /// `air` seconds from now. The trail starts, once for an entity, and the sounds of the Ball
    /// opening wait for it to land, where the Pokémon stands now.
    pub(super) fn pokemon_thrown_sounds(&mut self, id: i32, air: f32) {
        let Some((at, shiny, owned)) = self.pokemon_sound_facts(id) else {
            return;
        };
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let sounds = &mut r.pokemon.sounds;
        let now = Instant::now();
        if sounds.trailed.insert(id) {
            // `MovingSoundInstance`: not looping, heard from where the Ball is, which is nowhere
            // (the origin) until the Ball is drawn.
            let mut trail = SoundInstance::new(
                sound::BALL_TRAIL,
                PLAYERS,
                sound::TRAIL_VOLUME,
                1.0,
                random_source::create(),
                false,
                0,
                Attenuation::Linear,
                [0.0; 3],
                false,
            );
            trail.ticking = Ticking::Native;
            let (instance, _) = self.audio.play_instance(trail, false);
            sounds.trails.insert(
                id,
                Trail {
                    instance,
                    since: now,
                    at: None,
                },
            );
            sounds.count(sound::BALL_TRAIL);
        }
        let due = now + Duration::from_secs_f32(air);
        for name in sound::thrown_sounds(shiny, owned) {
            sounds.later.push(Later {
                due,
                entity: id,
                sound: name.to_owned(),
                volume: sound::SEND_OUT_VOLUME,
                pitch: 1.0,
                at: Some(at),
            });
        }
    }

    /// `PokemonClientDelegate`, `BEAM_MODE` 2: Pokémon `id` appears where it stands, with a Ball's
    /// sound if it has an owner.
    pub(super) fn pokemon_appearing_sound(&mut self, id: i32) {
        let Some((at, shiny, owned)) = self.pokemon_sound_facts(id) else {
            return;
        };
        let Some(name) = sound::appearing_sound(shiny, owned) else {
            return;
        };
        self.audio
            .play_on(PLAYERS, name, Some(at), sound::SEND_OUT_VOLUME, 1.0);
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.sounds.count(name);
        }
    }

    /// `PlayPosableAnimationHandler`, for what of it is heard: the sounds of the animation the server
    /// names for an entity (a cry), each at its moment in that animation.
    pub(super) fn pokemon_animation_sounds(&mut self, data: &[u8]) {
        let packet = match PlayAnimation::read(data) {
            Ok(packet) => packet,
            Err(e) => {
                log::warn!("Horizon: a Cobblemon animation that does not decode: {e}");
                return;
            }
        };
        let Some(spawn) = self
            .remote
            .as_ref()
            .and_then(|r| r.pokemon.seen.get(&packet.entity))
        else {
            return;
        };
        let (species, aspects, alpha) = (spawn.species.clone(), spawn.aspects.clone(), spawn.alpha);
        let Some(variant) = self
            .mob_index
            .get(&self.mobs, packet.entity as u32)
            .map(|mob| mob.extra.variant)
        else {
            return;
        };
        // A look whose files are still being read has no voice yet.
        let Looked::Made(look) = self.pokemon_look_for(&species, &aspects, None) else {
            return;
        };
        // `BedrockSoundKeyframe.run`: an alpha's is louder and lower.
        let (volume, pitch) = if alpha { sound::ALPHA_CRY } else { (1.0, 1.0) };
        let now = Instant::now();
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        for keyframe in look.voice.sounds(variant, &packet.animations) {
            r.pokemon.sounds.later.push(Later {
                due: now + Duration::from_secs_f32(keyframe.seconds.max(0.0)),
                entity: packet.entity,
                sound: keyframe.sound.clone(),
                volume,
                pitch,
                at: None,
            });
        }
    }

    /// `BattleMusicHandler`: the music a battle asks for starts, takes the place of the one that
    /// plays, or what plays fades out.
    pub(super) fn pokemon_battle_music(&mut self, data: &[u8]) {
        let asked = match BattleMusic::read(data) {
            Ok(asked) => asked,
            Err(e) => {
                log::warn!("Horizon: a battle's music that does not decode: {e}");
                return;
            }
        };
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let sounds = &mut r.pokemon.sounds;
        let playing = sounds
            .music
            .as_ref()
            .filter(|music| self.audio.engine.is_active(music.instance));
        let step = sound::music_step(&asked, playing.map(|music| music.name.as_str()));
        let name = match (step, asked.music) {
            (MusicStep::Keep, _) | (_, None) => {
                // `setFade`: it fades out from now, and does not start over.
                if step == MusicStep::End
                    && let Some(music) = &mut sounds.music
                    && music.fading.is_none()
                {
                    music.fading = Some(Instant::now());
                    if let Some(instance) = self.audio.engine.instance_mut(music.instance) {
                        instance.looping = false;
                    }
                }
                return;
            }
            (_, Some(name)) => name,
        };
        // `switchMusic` stops what plays; what was held for it stays held for the next.
        let held = match sounds.music.take() {
            Some(old) => {
                self.audio.stop_instance(old.instance);
                old.held
            }
            None => false,
        };
        // `BattleMusicInstance`: on the music source, by the listener, over and over.
        let mut music = SoundInstance::new(
            &name,
            MUSIC,
            asked.volume,
            asked.pitch,
            random_source::create(),
            true,
            0,
            Attenuation::None,
            [0.0; 3],
            true,
        );
        music.ticking = Ticking::Native;
        let (instance, _) = self.audio.play_instance(music, false);
        // `initializeMusic`: once it plays, the game's music, ambience and records wait for its end.
        let held = held
            || (step == MusicStep::Start && self.audio.engine.is_active(instance) && {
                self.audio.pause_sources(&HELD_BY_MUSIC, Some(instance));
                true
            });
        sounds.count(&name);
        sounds.music = Some(Music {
            instance,
            name,
            volume: asked.volume,
            fading: None,
            held,
        });
    }

    /// `MusicManagerMixin`: whether a battle's music plays, which holds the game's own music where
    /// it is.
    pub(in super::super::super) fn pokemon_battle_music_plays(&self) -> bool {
        self.remote
            .as_ref()
            .and_then(|r| r.pokemon.sounds.music.as_ref())
            .is_some_and(|music| self.audio.engine.is_active(music.instance))
    }

    /// The click of Cobblemon's screens (`SimpleSoundInstance.forUI(CobblemonSounds.GUI_CLICK, 1.0F)`).
    pub(super) fn pokemon_ui_click(&mut self) {
        self.audio.play_instance(
            SoundInstance::for_ui(sound::GUI_CLICK, 1.0, 0.25, random_source::create()),
            false,
        );
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.sounds.count(sound::GUI_CLICK);
        }
    }

    /// `horizon-ctl status`: what is known of Cobblemon's sounds and what was started from here.
    pub(in super::super::super) fn pokemon_sounds_status(&self) -> serde_json::Value {
        let Some(sounds) = self.remote.as_ref().map(|r| &r.pokemon.sounds) else {
            return serde_json::Value::Null;
        };
        let read = READ.get();
        serde_json::json!({
            // What Cobblemon's resources hold, 0 until they are read.
            "events": read.map_or(0, |read| read.registry.len()),
            "files": read.map_or(0, |read| read.files.len()),
            // Whether the sound engine has them.
            "known": sounds.known,
            "unknown": sounds.unknown,
            "played": sounds.played,
            "waiting": sounds.later.len(),
            "trail": sounds.trail_at.map(|(id, at)| serde_json::json!({"pokemon": id, "at": at})),
            "music": sounds.music.as_ref().map(|music| serde_json::json!({
                "name": music.name,
                "volume": music.volume,
                "fading": music.fading.is_some(),
                "playing": self.audio.engine.is_active(music.instance),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobblelegacy_cobblemon::wire::PoseType;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn sounds_are_read_from_roots_that_stack() {
        let base =
            std::env::temp_dir().join(format!("horizon-cobblemon-sounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (pack, cobblemon, addon) = (
            base.join("pack"),
            base.join("cobblemon"),
            base.join("addon"),
        );
        write(
            &cobblemon.join("assets/cobblemon/sounds.json"),
            br#"{
                "poke_ball.throw": {"sounds": [{"name": "cobblemon:poke_ball/throw1", "volume": 0.8}, "cobblemon:poke_ball/throw2"]},
                "poke_ball.recall": {"sounds": ["cobblemon:poke_ball/recall"]},
                "pokemon.missing.cry": {"sounds": ["cobblemon:pokemon/missing/cry"]}
            }"#,
        );
        for file in ["poke_ball/throw1", "poke_ball/throw2", "poke_ball/recall"] {
            write(
                &cobblemon.join(format!("assets/cobblemon/sounds/{file}.ogg")),
                b"cobblemon",
            );
        }
        // Under Cobblemon: a file of its own, which Cobblemon's takes the place of.
        write(
            &pack.join("assets/cobblemon/sounds/poke_ball/recall.ogg"),
            b"pack",
        );
        // Over it: another namespace, one more throw, a recall of its own.
        write(
            &addon.join("assets/cobblemon/sounds.json"),
            br#"{
                "poke_ball.throw": {"sounds": ["cobblemon:poke_ball/throw3"]},
                "poke_ball.recall": {"replace": true, "sounds": ["cobblemon:poke_ball/recall"]}
            }"#,
        );
        write(
            &addon.join("assets/cobblemon/sounds/poke_ball/throw3.ogg"),
            b"addon",
        );
        write(
            &addon.join("assets/cobblemon/sounds/poke_ball/recall.ogg"),
            b"addon",
        );
        write(
            &addon.join("assets/addon/sounds.json"),
            br#"{"cry": {"sounds": ["addon:cry"]}}"#,
        );
        write(&addon.join("assets/addon/sounds/cry.ogg"), b"addon");

        let sounds = read_sounds(&[pack, cobblemon.clone(), addon.clone(), base.join("absent")]);
        let registry = &sounds.registry;
        assert_eq!(
            registry.tracks("cobblemon:poke_ball.throw"),
            [
                "cobblemon:poke_ball/throw1",
                "cobblemon:poke_ball/throw2",
                "cobblemon:poke_ball/throw3"
            ]
        );
        assert_eq!(registry.tracks("cobblemon:poke_ball.recall").len(), 1);
        assert_eq!(registry.tracks("addon:cry"), ["addon:cry"]);
        // An event whose file no root has is known, and plays nothing.
        assert!(registry.contains("cobblemon:pokemon.missing.cry"));
        assert_eq!(registry.weight("cobblemon:pokemon.missing.cry"), 0);
        // Each file is the highest root's.
        assert_eq!(
            sounds.files["cobblemon:sounds/poke_ball/recall.ogg"],
            addon.join("assets/cobblemon/sounds/poke_ball/recall.ogg")
        );
        assert_eq!(
            sounds.files["cobblemon:sounds/poke_ball/throw1.ogg"],
            cobblemon.join("assets/cobblemon/sounds/poke_ball/throw1.ogg")
        );
        assert_eq!(sounds.files.len(), 5);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_voice_is_the_cry_of_the_pose_the_pokemon_is_in() {
        let poser = Poser::parse(
            r#"{
                "animations": {"cry": "q.bedrock_stateful('testmon', 'cry')"},
                "poses": {
                    "standing": {"poseTypes": ["STAND", "WALK"], "animations": []},
                    "sleeping": {
                        "poseTypes": ["SLEEP"],
                        "animations": [],
                        "namedAnimations": {"cry": "q.bedrock_stateful('testmon', 'yawn')"}
                    }
                }
            }"#,
        )
        .unwrap();
        let file = Arc::new(
            AnimationFile::parse(
                r#"{"animations": {
                    "animation.testmon.cry": {"sound_effects": {"0.5": {"effect": "pokemon.testmon.cry"}}},
                    "animation.testmon.yawn": {"sound_effects": {"1.5": {"effect": "pokemon.testmon.yawn"}}}
                }}"#,
            )
            .unwrap(),
        );
        let mut load = |group: &str| (group == "testmon").then(|| file.clone());
        let animator = anim::Animator::new(&poser, &mut load);
        let voice = Voice::new(&poser, &animator, &mut load);
        let cry = ["cry".to_owned()];
        let heard = |variant: u8| -> Vec<(f32, &str)> {
            voice
                .sounds(variant, &cry)
                .iter()
                .map(|s| (s.seconds, s.sound.as_str()))
                .collect()
        };
        assert_eq!(
            heard(PoseType::Stand as u8),
            [(0.5, "cobblemon:pokemon.testmon.cry")]
        );
        assert_eq!(
            heard(anim::with_pose(PoseType::Stand as u8, PoseType::Sleep)),
            [(1.5, "cobblemon:pokemon.testmon.yawn")]
        );
        assert!(
            voice
                .sounds(PoseType::Stand as u8, &["faint".to_owned()])
                .is_empty()
        );
        // A look without a poser has no voice.
        assert!(
            Voice::default()
                .sounds(PoseType::Stand as u8, &cry)
                .is_empty()
        );
    }

    #[test]
    fn a_pokemon_that_leaves_takes_its_sounds_with_it() {
        let mut sounds = Sounds::default();
        let now = Instant::now();
        for entity in [7, 8] {
            sounds.later.push(Later {
                due: now,
                entity,
                sound: sound::SEND_OUT.into(),
                volume: sound::SEND_OUT_VOLUME,
                pitch: 1.0,
                at: Some([0.0; 3]),
            });
            sounds.trailed.insert(entity);
            sounds.trails.insert(
                entity,
                Trail {
                    instance: entity as InstanceId,
                    since: now,
                    at: None,
                },
            );
        }
        // The Ball's place is the trail's, for a Pokémon that has one.
        sounds.ball_at(7, [1.0, 2.0, 3.0]);
        sounds.ball_at(9, [4.0, 5.0, 6.0]);
        assert_eq!(sounds.trails[&7].at, Some([1.0, 2.0, 3.0]));
        assert_eq!(sounds.trail_at, Some((7, [1.0, 2.0, 3.0])));
        sounds.forget(7);
        assert_eq!(sounds.later.len(), 1);
        assert_eq!(sounds.later[0].entity, 8);
        // Its trail is to be stopped, and a Pokémon of that id would get one of its own.
        assert_eq!(sounds.stopped, [7]);
        assert!(!sounds.trailed.contains(&7) && sounds.trailed.contains(&8));
        assert!(trail_life() > Duration::from_secs(1));
    }

    /// Cobblemon's own sounds through the mixer, with no output device: the events are read, a Ball's
    /// throw decodes and is heard, and so does a battle's music, which is streamed. Run it with
    /// `HORIZON_COBBLEMON_ROOT` naming Cobblemon's resources.
    #[test]
    #[ignore = "needs HORIZON_COBBLEMON_ROOT, the roots of Cobblemon 1.8.1's resources"]
    fn cobblemon_s_sounds_decode_and_reach_the_mixer() {
        use horizon_client::audio::Audio;
        let roots: Vec<PathBuf> = std::env::split_paths(
            &std::env::var_os("HORIZON_COBBLEMON_ROOT").expect("HORIZON_COBBLEMON_ROOT"),
        )
        .collect();
        let sounds = read_sounds(&roots);
        let silent: Vec<&String> = sounds
            .registry
            .names()
            .filter(|name| sounds.registry.weight(name) <= 0)
            .collect();
        println!(
            "{} events, {} files, {} events without a file: {silent:?}",
            sounds.registry.len(),
            sounds.files.len(),
            silent.len()
        );
        assert!(sounds.registry.len() > 2000);
        let mut audio = Audio::silent(Path::new("/nonexistent/horizon-cobblemon-sounds"));
        audio.engine.loaded = true;
        audio.backend_live_for_bench();
        audio.add_sounds(Arc::new(sounds));
        let heard = |audio: &mut Audio, what: &str| {
            let started = Instant::now();
            let mut out = vec![0.0f32; 4096];
            while started.elapsed() < Duration::from_secs(30) {
                audio.mix_buffer(&mut out, 2, 48_000);
                if out.iter().any(|sample| sample.abs() > 1.0e-4) {
                    println!("{what}: heard after {:?}", started.elapsed());
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("{what}: nothing reached the mixer");
        };
        for event in [
            "cobblemon:poke_ball.throw",
            "cobblemon:poke_ball.recall",
            sound::BALL_TRAIL,
            sound::SEND_OUT,
            sound::SHINY_SEND_OUT,
            sound::GUI_CLICK,
            "cobblemon:pokemon.pikachu.cry",
        ] {
            audio.play_on(NEUTRAL, event, None, 1.0, 1.0);
            heard(&mut audio, event);
            audio.stop_all();
        }
        let music = "cobblemon:battle.pvw.default";
        audio.play_instance(
            SoundInstance::for_music(music, random_source::create()),
            false,
        );
        heard(&mut audio, music);
    }
}
