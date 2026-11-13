//! Pokémon of a Cobblemon server (CobbleLegacy's fork), out of remote.rs, which may not grow.
//!
//! Cobblemon never sends vanilla's AddEntity for a Pokémon: its own `cobblemon:spawn_pokemon_entity`
//! carries the species, aspects and scale, then the vanilla packet's bytes
//! (`cobblelegacy_cobblemon::wire`). Everything after that is vanilla, by entity id: moves, head turns,
//! motion, metadata, removal. So a Pokémon is made a mob like any other here, and those packets find it
//! through the paths every remote mob uses.
//!
//! Its body is Cobblemon's own model when `HORIZON_COBBLEMON_ROOT` names Cobblemon's resources (the folder
//! that holds `assets/cobblemon` and `data/cobblemon`; several folders stack as resource packs do, see
//! `Assets::index`): the species' resolver picks a Bedrock geometry and
//! a texture for the Pokémon's aspects, the geometry becomes a `Part` tree and the texture goes into one of
//! the atlas's Pokémon slots. Its poser then poses it each draw (`remote_pokemon_anim.rs`): the species'
//! poser file, or for the third of the species whose poser Cobblemon writes in Kotlin, that class converted
//! to the same format (`cobblelegacy_cobblemon::coded_posers`). No layer (emissive parts, Charizard's
//! flame) is drawn yet. Without the resources, or for a species that fails to load, a vanilla
//! body stands in.
//!
//! The same looks draw the portraits of the player's own party (`remote_pokemon_party.rs`).
use super::super::Game;
use super::Tracked;
use cobblelegacy_cobblemon::{
    animation::AnimationFile,
    geo, resolver,
    wire::{PokemonData, SERVER_SETTINGS, SET_CLIENT_PLAYERDATA, SPAWN_POKEMON, SpawnPokemon},
};
use horizon_client::hud::{POKEMON_SLOT_SIZE, POKEMON_SLOTS};
use horizon_client::mobs::{Anim, Mob, MobType, Part, Polygon, Species, own};
use horizon_client::skins::SkinSlots;
use horizon_core::geometry::Vec3;
use horizon_protocol::adapter::UNREAD_ENTITY_DATA;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

#[path = "remote_pokemon_anim.rs"]
mod anim;
#[path = "remote_pokemon_capture.rs"]
mod capture;
#[path = "remote_pokemon_battle.rs"]
mod fight;
#[path = "remote_pokemon_label.rs"]
mod label;
#[path = "remote_pokemon_looks.rs"]
mod looks;
#[path = "remote_pokemon_particles.rs"]
mod particles;
#[path = "remote_pokemon_party.rs"]
mod party;
#[path = "remote_pokemon_pc.rs"]
mod pc;
#[path = "remote_pokemon_phase.rs"]
mod phase;
#[path = "remote_pokemon_sounds.rs"]
mod sounds;
#[path = "remote_pokemon_summary.rs"]
mod summary;

/// Whether `channel` carries one of the messages of a Cobblemon server that are read here: a Pokémon in
/// place of AddEntity, the part of its data the adapter could not read, the server's settings, the
/// player's Pokédex, the player's party and what changes in it.
pub(super) fn reads(channel: &str) -> bool {
    matches!(
        channel,
        SPAWN_POKEMON | UNREAD_ENTITY_DATA | SERVER_SETTINGS | SET_CLIENT_PLAYERDATA
    ) || cobblelegacy_cobblemon::party::reads(channel)
        || cobblelegacy_cobblemon::pc::reads(channel)
        || cobblelegacy_cobblemon::battle::reads(channel)
        || channel == cobblelegacy_cobblemon::moves::MOVES_SYNC
        || channel == cobblelegacy_cobblemon::sound::PLAY_ANIMATION
        || channel == cobblelegacy_cobblemon::capture::SPAWN_POKEBALL
        || cobblelegacy_cobblemon::snowstorm::reads(channel)
}

/// The vanilla type that lends a Pokémon its attributes, and its body when Cobblemon's own cannot be
/// drawn. A parrot, because Cobblemon's `PokemonEntity` is a `ShoulderRidingEntity` like it: the metadata
/// both inherit (entity, living, mob, ageable, tamable) sits at the same indexes.
const STAND_IN: &str = "minecraft:parrot";

/// Vanilla's `LivingEntityRenderer` puts a model's y = 24 pixels on the ground (its -1.501 blocks);
/// Cobblemon's models stand on y = 0, and `PosableEntityModel` moves them 1.5 blocks back.
const GROUND: f32 = 24.0;

/// What the window knows of the server's Pokémon.
#[derive(Default)]
pub(super) struct Pokemon {
    /// Cobblemon's resources, indexed when the first Pokémon appears. `Some(None)`: there are none.
    assets: Option<Option<Arc<Assets>>>,
    /// One look per model with a texture, made the first time it is seen. `None`: that look failed to
    /// load, and is not tried again.
    looks: HashMap<looks::Key, Option<Look>>,
    /// The looks whose files are being read (`remote_pokemon_looks.rs`), each with the Pokémon in the
    /// level that wait for it.
    reading: HashMap<looks::Key, Vec<i32>>,
    /// The thread that reads them, started with the first look.
    loader: Option<looks::Loader>,
    /// The stand-in type of each species without a look.
    stand_ins: HashMap<String, Arc<MobType>>,
    /// Which texture is in which of the atlas's Pokémon slots.
    slots: Option<SkinSlots>,
    /// What the server said of each Pokémon in view when it appeared, by entity id.
    seen: HashMap<i32, SpawnPokemon>,
    /// What their labels read: each one's name and level, the server's switches, the player's Pokédex.
    labels: label::Labels,
    /// The player's own Pokémon, which the overlay down the left of the screen shows.
    party: party::Party,
    /// The player's PC, and its screen while it is open.
    pcs: pc::Pcs,
    /// The battle the player is in.
    fight: fight::Fight,
    /// The server's moves, sent as the player joins: a battle's tiles take each move's type from it.
    moves: Option<cobblelegacy_cobblemon::moves::Moves>,
    /// The Pokémon coming out of their Balls and going back in.
    phases: phase::Phases,
    /// What is heard of them: a Ball's trail, the cries to come, a battle's music.
    sounds: sounds::Sounds,
    /// The Balls thrown at Pokémon to catch them.
    captures: capture::Captures,
    /// The particle effects that play.
    particles: particles::Particles,
    /// The summary screen of one of the player's Pokémon.
    summary: summary::Summary,
}

/// What there is of a look when it is asked for.
enum Looked {
    Made(Look),
    /// Its files are being read.
    Reading,
    /// There is none: no resources, a model that does not load, no slot left in the atlas.
    None,
}

/// A look ready to draw: the mob type a model and its texture make, and how the model's poser frames
/// it where only its head shows.
#[derive(Clone)]
struct Look {
    kind: Arc<MobType>,
    /// The species' `baseScale`, which the type's model is drawn at.
    base_scale: f32,
    /// The poser's `portraitScale` and `portraitTranslation`.
    portrait_scale: f32,
    portrait_translation: [f32; 3],
    /// The sounds of its named animations (a cry).
    voice: Arc<sounds::Voice>,
    /// The poser's `profileScale` and `profileTranslation`, which stand the whole model in a slot of
    /// the summary's party, and the summary's own for the model it shows large.
    profile_scale: f32,
    profile_translation: [f32; 3],
    summary_scale: f32,
    summary_translation: [f32; 3],
}

impl Pokemon {
    pub(super) fn forget(&mut self, id: i32) {
        self.seen.remove(&id);
        self.labels.forget(id);
        self.phases.forget(id);
        self.sounds.forget(id);
        self.captures.forget(id);
    }
}

/// Where each of Cobblemon's files is, by the id other files name it with, over every resource root:
/// Cobblemon's own resources and the resource packs under them.
struct Assets {
    /// The `assets/cobblemon` of each root, the one that wins first.
    assets: Vec<PathBuf>,
    /// `pikachu_male.geo` to its `*.geo.json`, wherever under `bedrock/pokemon/models`.
    models: HashMap<String, PathBuf>,
    /// `pikachu` to its poser file.
    posers: HashMap<String, PathBuf>,
    /// `pikachu` to its resolver files, in their `order`.
    resolvers: HashMap<String, Vec<resolver::ResolverFile>>,
    /// `pikachu` to its species file under `data/cobblemon/species`.
    species: HashMap<String, PathBuf>,
    /// `pikachu` to its `pikachu.animation.json`.
    animations: HashMap<String, PathBuf>,
    /// The animation groups read so far. `None`: the group has no file, or one that does not parse.
    animation_files: Mutex<HashMap<String, Option<Arc<AnimationFile>>>>,
}

/// Every file under `dir` whose name ends with `suffix`, by that name without the suffix.
fn files(dir: &Path, suffix: &str, out: &mut HashMap<String, PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, suffix, out);
        } else if let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(suffix))
        {
            out.insert(id.to_owned(), path);
        }
    }
}

/// `cobblemon:pikachu` as `pikachu`.
fn path_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, path)| path)
}

impl Assets {
    /// Cobblemon's resources under the folders `HORIZON_COBBLEMON_ROOT` names: one root, or several as
    /// a path list (`;` on Windows), the one that wins last. None without the variable, or when the
    /// atlas was built without the Pokémon's slots (it has them only when the variable was there).
    fn load() -> Option<Arc<Self>> {
        let roots: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("HORIZON_COBBLEMON_ROOT")?).collect();
        horizon_client::hud::pokemon_slots()
            .then(|| Self::index(&roots))
            .flatten()
            .map(Arc::new)
    }

    /// `roots` from the one a file is taken from last to the one that wins, as resource packs stack: a
    /// server's pack first, Cobblemon's own resources after it when the pack sits under the mods' (what
    /// CobbleLegacy's launcher sets up, so the pack only adds what Cobblemon lacks).
    fn index(roots: &[PathBuf]) -> Option<Self> {
        let mut models = HashMap::new();
        let mut posers = HashMap::new();
        let mut species = HashMap::new();
        let mut animations = HashMap::new();
        // Resolvers by their path under `resolvers`: a root replaces the file a lower one has there, and
        // every other file adds its variations.
        let mut resolver_files: HashMap<PathBuf, PathBuf> = HashMap::new();
        for root in roots {
            let pokemon = root.join("assets/cobblemon/bedrock/pokemon");
            files(&pokemon.join("models"), ".json", &mut models);
            files(&pokemon.join("posers"), ".json", &mut posers);
            files(&root.join("data/cobblemon/species"), ".json", &mut species);
            files(
                &pokemon.join("animations"),
                ".animation.json",
                &mut animations,
            );
            let folder = pokemon.join("resolvers");
            let mut found = HashMap::new();
            files(&folder, ".json", &mut found);
            for path in found.into_values() {
                if let Ok(relative) = path.strip_prefix(&folder) {
                    resolver_files.insert(relative.to_owned(), path);
                }
            }
        }
        let mut resolvers: HashMap<String, Vec<resolver::ResolverFile>> = HashMap::new();
        // By path, so files of one order keep one sequence from run to run.
        let mut paths: Vec<(&PathBuf, &PathBuf)> = resolver_files.iter().collect();
        paths.sort();
        for (_, path) in paths {
            let parsed = std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|json| resolver::ResolverFile::parse(&json).map_err(|e| e.0));
            match parsed {
                Ok(file) => resolvers
                    .entry(path_of(&file.species).to_owned())
                    .or_default()
                    .push(file),
                Err(e) => log::debug!("Horizon: Cobblemon resolver {}: {e}", path.display()),
            }
        }
        for list in resolvers.values_mut() {
            list.sort_by_key(|file| file.order);
        }
        if models.is_empty() || resolvers.is_empty() {
            log::warn!(
                "Horizon: no Cobblemon models under {roots:?}: Pokémon keep a stand-in body"
            );
            return None;
        }
        log::info!(
            "Horizon: Cobblemon resources from {} roots: {} models, {} posers, {} animation groups, {} species with a resolver",
            roots.len(),
            models.len(),
            posers.len(),
            animations.len(),
            resolvers.len()
        );
        Some(Self {
            assets: roots
                .iter()
                .rev()
                .map(|root| root.join("assets/cobblemon"))
                .collect(),
            models,
            posers,
            resolvers,
            species,
            animations,
            animation_files: Mutex::default(),
        })
    }

    /// The file of a texture id (`cobblemon:textures/pokemon/0025_pikachu/pikachu.png`), in the root that
    /// wins among those that have it.
    fn texture(&self, id: &str) -> Option<PathBuf> {
        self.file(path_of(id))
    }

    /// The file at `path` under `assets/cobblemon`, in the root that wins among those that have it.
    fn file(&self, path: &str) -> Option<PathBuf> {
        self.assets
            .iter()
            .map(|assets| assets.join(path))
            .find(|path| path.is_file())
    }

    /// A Bedrock animation group (`pikachu`), read the first time a pose names it.
    fn animation(&self, group: &str) -> Option<Arc<AnimationFile>> {
        self.animation_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(group.to_owned())
            .or_insert_with(|| {
                let json = std::fs::read_to_string(self.animations.get(group)?).ok()?;
                match AnimationFile::parse(&json) {
                    Ok(file) => Some(Arc::new(file)),
                    Err(e) => {
                        log::warn!("Horizon: Cobblemon animations of {group}: {}", e.0);
                        None
                    }
                }
            })
            .clone()
    }

    /// `baseScale` and the `hitbox`'s width and height of a species file.
    fn dimensions(&self, species: &str) -> (f32, f32, f32) {
        let read = || -> Option<(f32, f32, f32)> {
            let json = std::fs::read_to_string(self.species.get(species)?).ok()?;
            let data: serde_json::Value = serde_json::from_str(&json).ok()?;
            let number = |v: &serde_json::Value| v.as_f64().map(|n| n as f32);
            Some((
                number(&data["baseScale"]).unwrap_or(1.0),
                number(&data["hitbox"]["width"]).unwrap_or(1.0),
                number(&data["hitbox"]["height"]).unwrap_or(1.0),
            ))
        };
        read().unwrap_or((1.0, 1.0, 1.0))
    }

    /// What the party overlay needs of a species file and the server does not send: the base HP and
    /// the experience group, the form's own where the form `form` (its Showdown id, `normal` for the
    /// species' own) gives one.
    fn facts(&self, species: &str, form: &str) -> Option<party::Facts> {
        let json = std::fs::read_to_string(self.species.get(species)?).ok()?;
        let data: serde_json::Value = serde_json::from_str(&json).ok()?;
        // `FormData.formOnlyShowdownId`: the name in lower case, without what is no letter or digit.
        let id = |name: &str| -> String {
            name.chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect()
        };
        let of_form = data["forms"].as_array().and_then(|forms| {
            forms
                .iter()
                .find(|f| f["name"].as_str().is_some_and(|n| id(n) == form))
        });
        let pick = |key: &str, inner: Option<&str>| {
            [of_form, Some(&data)]
                .into_iter()
                .flatten()
                .find_map(|from| {
                    let value = &from[key];
                    let value = inner.map_or(value, |inner| &value[inner]);
                    (!value.is_null()).then(|| value.clone())
                })
        };
        Some(party::Facts {
            base_health: u32::try_from(pick("baseStats", Some("hp"))?.as_u64()?).ok()?,
            experience: pick("experienceGroup", None).and_then(|group| {
                cobblelegacy_cobblemon::party::ExperienceGroup::named(group.as_str()?)
            }),
        })
    }
}

/// A geometry's part as the mob renderer's: the same pixels, pose and faces (`ModelPart`'s conventions on
/// both sides).
fn part(bone: &geo::Bone, texture: [u32; 2]) -> Part {
    let [x, y, z] = bone.offset;
    let [rx, ry, rz] = bone.rotation;
    Part {
        name: bone.name.clone(),
        pose: [x, y, z, rx, ry, rz, 1.0, 1.0, 1.0],
        polygons: bone
            .cubes
            .iter()
            .flat_map(|cube| cube.quads(texture))
            .map(|quad| Polygon {
                normal: quad.normal,
                vertices: quad.vertices,
                cull_back: false,
            })
            .collect(),
        children: bone.children.iter().map(|b| part(b, texture)).collect(),
    }
}

/// An image no larger than a Pokémon slot: halved (every other texel) while it is.
fn fit(mut width: u32, mut height: u32, mut rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    while width > POKEMON_SLOT_SIZE || height > POKEMON_SLOT_SIZE {
        let (w, h) = ((width / 2).max(1), (height / 2).max(1));
        let mut half = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let at = ((y * 2 * width + x * 2) * 4) as usize;
                half.extend_from_slice(&rgba[at..at + 4]);
            }
        }
        (width, height, rgba) = (w, h, half);
    }
    (width, height, rgba)
}

impl Game<'_> {
    /// The look of a Pokémon of `species_id` with `aspects`: its species' model for them as a mob type,
    /// with its texture in the atlas. `None` when Cobblemon's resources are not there, the look does not
    /// load, every one of the atlas's slots is worn by a Pokémon in view, or its files are still being
    /// read: asked for again, it is there once they are.
    fn pokemon_look(
        &mut self,
        species_id: &str,
        aspects: &[String],
        _template: &MobType,
    ) -> Option<Look> {
        match self.pokemon_look_for(species_id, aspects, None) {
            Looked::Made(look) => Some(look),
            Looked::Reading | Looked::None => None,
        }
    }

    /// [`Self::pokemon_look`] for `waiting`, a Pokémon in the level: when the look's files are still
    /// being read, it gets its body as soon as they are (`pokemon_looks_frame`).
    fn pokemon_look_for(
        &mut self,
        species_id: &str,
        aspects: &[String],
        waiting: Option<i32>,
    ) -> Looked {
        let Some(r) = self.remote.as_mut() else {
            return Looked::None;
        };
        let pokemon = &mut r.pokemon;
        let Some(assets) = pokemon.assets.get_or_insert_with(Assets::load).clone() else {
            return Looked::None;
        };
        let resolved = assets
            .resolvers
            .get(path_of(species_id))
            .and_then(|resolvers| resolver::resolve(resolvers, aspects));
        let Some(resolved) = resolved else {
            return Looked::None;
        };
        let Some(texture_id) = resolved.texture.at(0.0) else {
            return Looked::None;
        };
        let key = (resolved.model.clone(), texture_id.to_owned());
        if let Some(look) = pokemon.looks.get(&key) {
            return look.clone().map_or(Looked::None, Looked::Made);
        }
        if let Some(ids) = pokemon.reading.get_mut(&key) {
            ids.extend(waiting);
            return Looked::Reading;
        }
        pokemon
            .reading
            .insert(key.clone(), waiting.into_iter().collect());
        pokemon
            .loader
            .get_or_insert_with(|| looks::Loader::new(assets.clone()))
            .ask(looks::Wanted {
                key,
                species: species_id.to_owned(),
                poser: resolved.poser.clone(),
            });
        Looked::Reading
    }

    /// Once a frame: the looks whose files have been read since the last get their place in the atlas,
    /// and the Pokémon that waited for them their body.
    fn pokemon_looks_frame(&mut self) {
        loop {
            let Some(r) = self.remote.as_mut() else {
                return;
            };
            let pokemon = &mut r.pokemon;
            let Some((wanted, read)) = pokemon.loader.as_mut().and_then(looks::Loader::next) else {
                return;
            };
            let waiting = pokemon.reading.remove(&wanted.key).unwrap_or_default();
            let look = match read {
                Ok(read) => self.pokemon_look_made(&wanted, read),
                Err(e) => {
                    log::warn!("Horizon: {}'s model does not load: {e}", wanted.species);
                    pokemon.looks.insert(wanted.key.clone(), None);
                    None
                }
            };
            // A look that could not be made leaves them the body that stands in for it.
            let kind = match look {
                Some(look) => Some(look.kind),
                None => self.pokemon_stand_in(&wanted.species),
            };
            let Some(kind) = kind else {
                continue;
            };
            for id in waiting {
                // One that left the level while its look was read is not there any more.
                if let Some(mob) = self.mob_index.get_mut(&mut self.mobs, id as u32)
                    && mob.species() == Species::Pokemon
                {
                    mob.kind = kind.clone();
                }
            }
        }
    }

    /// What the render thread does of a look once its files are read: the texture's place in the atlas
    /// and the mob type. `None` when every one of the atlas's slots is worn at once: the look is read
    /// again the next time it is seen.
    fn pokemon_look_made(&mut self, wanted: &looks::Wanted, read: looks::Read) -> Option<Look> {
        let template = self.res.mob_types().get(STAND_IN).cloned()?;
        let (base_scale, width, height) = read.dimensions;
        // Stood on the ground and sized, around its feet: a part of its own above the model's
        // root, whose pose animations change (`PokemonRenderer` scales outside the model too).
        let body = Part {
            name: "#stand".into(),
            pose: [
                0.0, GROUND, 0.0, 0.0, 0.0, 0.0, base_scale, base_scale, base_scale,
            ],
            polygons: Vec::new(),
            children: vec![read.root],
        };
        let sprite = format!("cobblemon/{}", path_of(&wanted.key.1));
        self.pokemon_skin(&sprite, read.texture)?;
        let mut kind = pokemon_type(&template, &wanted.species);
        kind.model = Arc::new(body);
        kind.anim = Anim::Own(
            read.animator
                .map_or(own::NONE, |a| own::register(Arc::new(a))),
        );
        kind.texture = sprite;
        // `PokemonEntity.getDimensions`: the hitbox by the base scale.
        kind.width = f64::from(width * base_scale);
        kind.height = f64::from(height * base_scale);
        kind.eye_height = kind.height * 0.85;
        let (portrait_scale, portrait_translation) = read.portrait;
        let (profile_scale, profile_translation) = read.profile;
        let (summary_scale, summary_translation) = read.summary;
        let look = Look {
            kind: Arc::new(kind),
            base_scale,
            portrait_scale,
            portrait_translation,
            voice: Arc::new(read.voice),
            profile_scale,
            profile_translation,
            summary_scale,
            summary_translation,
        };
        self.remote
            .as_mut()?
            .pokemon
            .looks
            .insert(wanted.key.clone(), Some(look.clone()));
        Some(look)
    }

    /// The vanilla body a Pokémon of `species_id` has when Cobblemon's own cannot be drawn.
    fn pokemon_stand_in(&mut self, species_id: &str) -> Option<Arc<MobType>> {
        let template = self.res.mob_types().get(STAND_IN).cloned()?;
        let stand_ins = &mut self.remote.as_mut()?.pokemon.stand_ins;
        Some(
            stand_ins
                .entry(species_id.to_owned())
                .or_insert_with(|| Arc::new(pokemon_type(&template, species_id)))
                .clone(),
        )
    }

    /// Once a frame, before it is drawn: what of the Pokémon moves on by itself.
    pub(in super::super) fn pokemon_frame(&mut self) {
        self.pokemon_looks_frame();
        self.pokemon_phase_frame();
        self.pokemon_sounds_frame();
        self.pokemon_capture_frame();
        self.pokemon_particles_frame();
    }

    /// An image in one of the atlas's Pokémon slots, as the sprite `sprite`. The slots are shared: only
    /// the textures the Pokémon in the level, the party's portraits and the Balls in flight wear keep
    /// theirs, and one nothing wears any more gives its slot to this one. `None` when every slot is worn
    /// at once.
    fn pokemon_skin(&mut self, sprite: &str, (w, h, rgba): (u32, u32, Vec<u8>)) -> Option<()> {
        let pokemon = &mut self.remote.as_mut()?.pokemon;
        let worn: HashSet<String> = self
            .mobs
            .iter()
            .filter(|m| m.species() == Species::Pokemon)
            .map(|m| m.kind.texture.clone())
            .chain(pokemon.party.textures())
            .chain(pokemon.pcs.textures())
            .chain(pokemon.phases.textures())
            .chain(pokemon.phases.ball_textures(pokemon.captures.names()))
            .chain(pokemon.particles.textures())
            .collect();
        let slots = pokemon
            .slots
            .get_or_insert_with(|| SkinSlots::new(POKEMON_SLOTS));
        slots.next_frame(worn);
        let (slot, evicted) = slots.place(sprite.to_owned())?;
        let atlas = &mut self.gui.atlas;
        if let Some(old) = evicted {
            atlas.sprites.remove(&old);
            // The looks that wore it are made again the next time they are seen.
            pokemon
                .looks
                .retain(|_, look| look.as_ref().is_none_or(|l| l.kind.texture != old));
            pokemon.party.forget_texture(&old);
            pokemon.pcs.forget_texture(&old);
            pokemon.phases.forget_texture(&old);
            pokemon.particles.forget_texture(&old);
        }
        let mut rect = *atlas.sprites.get(&format!("horizon/pokemon{slot}"))?;
        let (aw, ah) = (atlas.width as f32, atlas.height as f32);
        self.map_patches.push(horizon_client::font::Patch {
            x: (rect.u0 * aw).round() as u32,
            y: (rect.v0 * ah).round() as u32,
            width: w,
            height: h,
            rgba,
        });
        // The texture is the slot's top left corner.
        rect.u1 = rect.u0 + w as f32 / aw;
        rect.v1 = rect.v0 + h as f32 / ah;
        atlas.sprites.insert(sprite.to_owned(), rect);
        Some(())
    }

    /// A message of a Cobblemon server on one of the channels [`reads`] names.
    pub(super) fn remote_pokemon_payload(&mut self, channel: &str, data: &[u8]) {
        // The PC's own messages; those it shares with the party go on to the party's reader.
        if self.remote_pokemon_pc(channel, data) {
            return;
        }
        match channel {
            SPAWN_POKEMON => self.remote_pokemon_spawn(data),
            UNREAD_ENTITY_DATA => self.remote_pokemon_data(data),
            cobblelegacy_cobblemon::sound::PLAY_ANIMATION => self.pokemon_animation_sounds(data),
            cobblelegacy_cobblemon::capture::SPAWN_POKEBALL => self.remote_pokemon_ball_spawn(data),
            battle if cobblelegacy_cobblemon::battle::reads(battle) => {
                self.pokemon_battle_sounds(battle, data);
                self.remote_pokemon_battle(battle, data);
            }
            particle if cobblelegacy_cobblemon::snowstorm::reads(particle) => {
                self.pokemon_particle_payload(particle, data);
            }
            cobblelegacy_cobblemon::moves::MOVES_SYNC => {
                match cobblelegacy_cobblemon::moves::Moves::read(data) {
                    Ok(moves) => {
                        if let Some(r) = self.remote.as_mut() {
                            r.pokemon.moves = Some(moves);
                        }
                    }
                    Err(e) => log::warn!("Horizon: Cobblemon's moves do not decode: {e}"),
                }
            }
            _ => {
                let Some(r) = self.remote.as_mut() else {
                    return;
                };
                match channel {
                    SERVER_SETTINGS => r.pokemon.labels.settings(data),
                    SET_CLIENT_PLAYERDATA => r.pokemon.labels.pokedex(data),
                    _ => {
                        let pokemon = &mut r.pokemon;
                        let max_level = pokemon.labels.max_level();
                        let assets = pokemon.assets.get_or_insert_with(Assets::load).as_deref();
                        pokemon.party.read(channel, data, max_level, assets);
                    }
                }
            }
        }
    }

    /// A Pokémon's data from Cobblemon's own types on, which the protocol adapter sends after the data
    /// vanilla reads (`horizon_protocol::adapter::UNREAD_ENTITY_DATA`: the entity's id, then the
    /// server's bytes). The pose the server puts the Pokémon in is there (asleep, in flight, swimming),
    /// and what its label says of its level. Its other fields (aspects, raft) are read and not used
    /// yet.
    fn remote_pokemon_data(&mut self, data: &[u8]) {
        let mut r = horizon_protocol::buf::Reader::new(data);
        let Ok(id) = r.varint() else {
            return;
        };
        let rest = r.rest();
        // A thrown Ball's, which has its own layout.
        if self.remote_pokemon_ball_data(id, rest) {
            return;
        }
        // Another mod's entity, whose layout nothing here knows.
        if !self
            .remote
            .as_ref()
            .is_some_and(|r| r.pokemon.seen.contains_key(&id))
        {
            return;
        }
        let data = match PokemonData::read_rest(rest) {
            Ok(data) => data,
            Err(e) => {
                log::debug!("Horizon: the data of Pokémon {id} does not decode: {e}");
                return;
            }
        };
        if let Some(r) = self.remote.as_mut() {
            r.pokemon.labels.own_data(id, &data);
        }
        if let Some(mode) = data.beam_mode {
            self.pokemon_phase(id, mode);
        }
        if let Some(pose) = data.pose
            && let Some(mob) = self.mob_index.get_mut(&mut self.mobs, id as u32)
        {
            mob.extra.variant = anim::with_pose(mob.extra.variant, pose);
            // What a test reads in the log, a pose being nothing `horizon-ctl` can ask for.
            static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *TRACE.get_or_init(|| std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some()) {
                log::info!("Horizon: Pokémon {id} is now in pose {pose:?}");
            }
        }
    }

    /// `SpawnPokemonPacket.spawnAndApply`: what `AddEntity` does for a vanilla mob, with the type the
    /// species has here in place of a registry lookup (the server's number for `cobblemon:pokemon` names
    /// nothing in the window's registries).
    fn remote_pokemon_spawn(&mut self, data: &[u8]) {
        let spawn = match SpawnPokemon::read(data) {
            Ok(spawn) => spawn,
            Err(e) => {
                log::warn!("Horizon: a Pokémon spawn that does not decode: {e}");
                return;
            }
        };
        let Some(template) = self.res.mob_types().get(STAND_IN).cloned() else {
            return;
        };
        let id = spawn.entity.id;
        let kind = match self.pokemon_look_for(&spawn.species, &spawn.aspects, Some(id)) {
            Looked::Made(look) => look.kind,
            // No body until its files are read: `pokemon_looks_frame` gives it one.
            Looked::Reading => {
                let mut bodiless = pokemon_type(&template, &spawn.species);
                bodiless.model = Arc::new(Part {
                    name: "#stand".into(),
                    pose: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                    polygons: Vec::new(),
                    children: Vec::new(),
                });
                Arc::new(bodiless)
            }
            Looked::None => match self.pokemon_stand_in(&spawn.species) {
                Some(kind) => kind,
                None => return,
            },
        };
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        let entity = &spawn.entity;
        let beam_mode = spawn.beam_mode;
        // The AddEntity arm of `handle_packet`: an id the level holds already starts afresh.
        r.ticked_before_player.remove(&(id as u32));
        r.level_player_textures.remove(&id);
        r.level_players.remove(&id);
        r.level_player_names.remove(&id);
        self.audio.remove_sound_entity(id as u32);
        self.persist.uuids.insert(
            id as u32,
            horizon_client::lan::playerdata::uuid_ints(entity.uuid),
        );
        // `spawn_entity`, for a type that is not in the window's tables.
        self.detach_remote_tracking_entity(id);
        let [x, y, z] = entity.position;
        let mut mob = Mob::new(id as u32, kind, Vec3::new(x, y, z), entity.head_yaw);
        super::super::remote_mob_data::client_defaults(&mut mob);
        (mob.yaw, mob.previous_yaw) = (entity.yaw, entity.yaw);
        (mob.head_pitch, mob.previous_head_pitch) = (entity.pitch, entity.pitch);
        mob.persistent = true;
        // `PokemonRenderer`: the Pokémon's own size on top of its species'.
        mob.extra.model_scale = spawn.scale_modifier;
        // The pose it appears in, which its animation starts from; the top bit is `MOVING`, which
        // metadata sets afterwards (`remote_mob_data`).
        mob.extra.variant = spawn.pose as u8;
        self.mobs.retain(|m| m.id != id as u32);
        self.mobs.push(mob);
        let velocity = entity.velocity;
        if let Some(r) = self.remote.as_mut() {
            r.stepped.remove(&id);
            r.cart_lerps.remove(&id);
            r.lerps.remove(&id);
            r.entities.insert(
                id,
                Tracked {
                    kind: spawn.species.clone(),
                    position: entity.position,
                    codec_base: entity.position,
                    silent: spawn.silent,
                    vehicle: None,
                },
            );
            r.pokemon.labels.appeared(id, &spawn);
            r.pokemon.phases.forget(id);
            r.pokemon.phases.target(id, spawn.phasing_target);
            r.pokemon.seen.insert(id, spawn);
        }
        // Entity.recreateFromPacket: the packet's motion, once the mob exists.
        self.remote_motion(id, velocity);
        // `SpawnPokemonPacket.applyData` sets the mode last: one thrown out starts in its Ball.
        self.pokemon_phase(id, beam_mode);
    }
}

/// A Pokémon species' mob type: the stand-in's attributes and body under the species' id, with nothing of
/// the stand-in's behaviour, layers, sounds or poses.
fn pokemon_type(template: &MobType, species: &str) -> MobType {
    let mut kind = template.clone();
    species.clone_into(&mut kind.id);
    kind.species = Species::Pokemon;
    kind.anim = Anim::Own(horizon_client::mobs::own::NONE);
    kind.layers.clear();
    kind.sounds = [""; 3];
    kind.step_sound = None;
    kind
}
