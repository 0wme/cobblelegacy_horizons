//! The files of a Pokémon's look, read away from the render thread (CobbleLegacy's fork).
//!
//! A look is a handful of Cobblemon's files: a geometry, a poser and its animations, a texture, the
//! species. Read where the server's packets are handled, they held a frame for as long as the disk took
//! each time a species was first seen: a tenth of a second on a fast disk, more than a second on a hard
//! drive. They are read and parsed on a thread of their own instead. Until they are, the Pokémon that
//! wait for them have no body, and a portrait is empty.
//!
//! What stays the render thread's is what only it can do: the texture's place in the atlas and the mob
//! type (`pokemon_look_made` in `remote_pokemon.rs`).
use super::{Assets, anim, fit, part, path_of, sounds};
use cobblelegacy_cobblemon::{coded_posers, geo, poser::Poser};
use horizon_client::mobs::Part;
use std::sync::{Arc, mpsc};

/// What names a look: its model and its texture.
pub(super) type Key = (String, String);

/// A look to read.
pub(super) struct Wanted {
    pub(super) key: Key,
    /// The species' id (`cobblemon:pikachu`).
    pub(super) species: String,
    /// The poser its resolver names.
    pub(super) poser: String,
}

/// A look's files, read and parsed.
pub(super) struct Read {
    /// The model's drawn root, as the mob renderer's part.
    pub(super) root: Part,
    /// The texture, no larger than a slot of the atlas: width, height, RGBA.
    pub(super) texture: (u32, u32, Vec<u8>),
    pub(super) animator: Option<anim::Animator>,
    /// The sounds of its named animations (a cry).
    pub(super) voice: sounds::Voice,
    /// The poser's `portraitScale` and `portraitTranslation`.
    pub(super) portrait: (f32, [f32; 3]),
    /// The species' `baseScale`, and its hitbox's width and height.
    pub(super) dimensions: (f32, f32, f32),
}

/// A look asked for, and what reading it gave.
pub(super) type Done = (Wanted, Result<Read, String>);

/// The thread that reads looks, one after the other in the order they were asked for.
pub(super) struct Loader {
    assets: Arc<Assets>,
    wanted: mpsc::Sender<Wanted>,
    read: mpsc::Receiver<Done>,
    /// The looks read here, for want of a thread.
    here: Vec<Done>,
}

impl Loader {
    pub(super) fn new(assets: Arc<Assets>) -> Self {
        let (wanted, jobs) = mpsc::channel::<Wanted>();
        let (done, read) = mpsc::channel();
        let on_thread = assets.clone();
        // It ends when the game leaves the server: the sender goes with what the window knows of it.
        let spawned = std::thread::Builder::new()
            .name("cobblemon looks".into())
            .spawn(move || {
                for job in jobs {
                    let result = read_look(&on_thread, &job);
                    if done.send((job, result)).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!(
                "Horizon: no thread to read Pokémon models on ({e}): they are read as they appear"
            );
        }
        Self {
            assets,
            wanted,
            read,
            here: Vec::new(),
        }
    }

    pub(super) fn ask(&mut self, wanted: Wanted) {
        // The thread is not there only if the system gave none: the look is read here then.
        if let Err(mpsc::SendError(wanted)) = self.wanted.send(wanted) {
            let read = read_look(&self.assets, &wanted);
            self.here.push((wanted, read));
        }
    }

    /// A look whose reading is over, if there is one.
    pub(super) fn next(&mut self) -> Option<Done> {
        self.here.pop().or_else(|| self.read.try_recv().ok())
    }
}

/// Everything of a look that is in files.
fn read_look(assets: &Assets, wanted: &Wanted) -> Result<Read, String> {
    let (model_id, texture_id) = &wanted.key;
    let model = assets
        .models
        .get(path_of(model_id))
        .ok_or_else(|| format!("no model {model_id}"))?;
    let json = std::fs::read_to_string(model).map_err(|e| e.to_string())?;
    let geometry = geo::Geometry::parse(&json).map_err(|e| e.0)?;
    let poser = path_of(&wanted.poser);
    // The species' poser file, or for one of the posers Cobblemon writes in code, what
    // `tools/cobblelegacy/posers_from_kotlin.py` made of that class (`coded_posers`).
    let poser_file = assets
        .posers
        .get(poser)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .or_else(|| coded_posers::coded_poser(poser).map(str::to_owned))
        .and_then(|json| match Poser::parse(&json) {
            Ok(parsed) => Some(parsed),
            Err(e) => {
                log::warn!("Horizon: Cobblemon poser {poser}: {}", e.0);
                None
            }
        });
    let root_bone = poser_file.as_ref().and_then(|p| p.root_bone.clone());
    let portrait = poser_file.as_ref().map_or((1.0, [0.0; 3]), |p| {
        (p.portrait_scale, p.portrait_translation)
    });
    let root = geometry
        .drawn_root(root_bone.as_deref(), poser)
        .ok_or("a model without a root bone")?;
    let texture = assets
        .texture(texture_id)
        .ok_or_else(|| format!("no texture {texture_id}"))?;
    let png = std::fs::read(texture).map_err(|e| format!("{texture_id}: {e}"))?;
    let (width, height, rgba) =
        horizon_client::hud::decode_bytes(&png).map_err(|e| e.to_string())?;
    let mut load = |group: &str| assets.animation(group);
    let animator = poser_file
        .as_ref()
        .map(|poser| anim::Animator::new(poser, &mut load));
    let voice = poser_file
        .as_ref()
        .zip(animator.as_ref())
        .map(|(poser, animator)| sounds::Voice::new(poser, animator, &mut load))
        .unwrap_or_default();
    Ok(Read {
        root: part(root, geometry.texture_size),
        texture: fit(width, height, rgba),
        animator,
        voice,
        portrait,
        dimensions: assets.dimensions(path_of(&wanted.species)),
    })
}
