//! The files of a Pokémon's look, read away from the render thread (CobbleLegacy's fork).
//!
//! A look is a handful of Cobblemon's files: a geometry, a poser and its animations, a texture, the
//! species. Read where the server's packets are handled, they held a frame for as long as the disk took
//! each time a species was first seen: a tenth of a second on a fast disk, more than a second on a hard
//! drive. They are read and parsed on a thread of their own instead. Until they are, the Pokémon that
//! wait for them have no body, and a portrait is empty.
//!
//! What stays the render thread's is what only it can do: the textures' place in the atlas and the mob
//! type (`pokemon_look_made` in `remote_pokemon.rs`).
//!
//! A look's textures are its model's own and those of its layers (a flame, eyes that glow), some of
//! several frames. They are laid out here in as few of the atlas's slots as hold them
//! (`cobblelegacy_cobblemon::sheet`), each under a sprite name made from the look's own.
use super::{Assets, anim, fit, part, path_of, sounds};
use cobblelegacy_cobblemon::{
    coded_posers, geo,
    poser::Poser,
    resolver::{Resolved, Texture},
    sheet,
};
use horizon_client::hud::POKEMON_SLOT_SIZE;
use horizon_client::mobs::Part;
use std::sync::{Arc, mpsc};

/// What names a look: its model, and the sprite of its texture ([`sprite`]), which tells apart what it
/// is drawn with.
pub(super) type Key = (String, String);

/// The sprite a look's own texture is in the atlas under: `cobblemon/` and the texture's path, then,
/// for a look drawn with more than that one texture, what tells its textures from another look's.
pub(super) fn sprite(resolved: &Resolved) -> Option<String> {
    let first = resolved.texture.frames().first()?;
    Some(format!("cobblemon/{}{}", path_of(first), resolved.dress()))
}

/// The sprite of the texture numbered `index` among a look's (its own frames, then its layers' in their
/// order), `look` being the first's ([`sprite`]). No texture id has a `|`.
fn frame_sprite(look: &str, index: usize) -> String {
    if index == 0 {
        look.to_owned()
    } else {
        format!("{look}|{index}")
    }
}

/// The name the slot of a look's sheet numbered `index` is kept under: the look's own sprite for the
/// first, which holds it.
fn sheet_name(look: &str, index: usize) -> String {
    if index == 0 {
        look.to_owned()
    } else {
        format!("{look}|sheet{index}")
    }
}

/// The look a sprite or a sheet's name is of: what [`frame_sprite`] and [`sheet_name`] were given.
pub(super) fn look_of(name: &str) -> &str {
    name.split_once('|').map_or(name, |(look, _)| look)
}

/// A look to read.
pub(super) struct Wanted {
    pub(super) key: Key,
    /// The species' id (`cobblemon:pikachu`).
    pub(super) species: String,
    /// What its resolver picked: the poser, the texture and the layers.
    pub(super) resolved: Resolved,
}

/// An image for one slot of the atlas, and the sprites in it.
pub(super) struct Sheet {
    /// What its slot is kept under ([`sheet_name`]).
    pub(super) name: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Vec<u8>,
    /// Each sprite's name and its texels in the image: x, y, width, height.
    pub(super) sprites: Vec<(String, [u32; 4])>,
}

/// A look's files, read and parsed.
pub(super) struct Read {
    /// The model's drawn root, as the mob renderer's part.
    pub(super) root: Part,
    /// Its textures, each sheet no larger than a slot of the atlas. The first holds the look's own
    /// texture at its top left corner, under the look's sprite.
    pub(super) sheets: Vec<Sheet>,
    pub(super) animator: Option<anim::Animator>,
    /// The sounds of its named animations (a cry).
    pub(super) voice: sounds::Voice,
    /// The poser's `portraitScale` and `portraitTranslation`.
    pub(super) portrait: (f32, [f32; 3]),
    /// Its `profileScale` and `profileTranslation`, and the summary screen's own where the poser
    /// gives them (`profileSummaryScale`, `profileSummaryTranslation`), else the profile's again.
    pub(super) profile: (f32, [f32; 3]),
    pub(super) summary: (f32, [f32; 3]),
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

/// The image of texture `id`, no larger than a slot of the atlas.
fn image(assets: &Assets, id: &str) -> Result<sheet::Image, String> {
    let file = assets
        .texture(id)
        .ok_or_else(|| format!("no texture {id}"))?;
    let png = std::fs::read(file).map_err(|e| format!("{id}: {e}"))?;
    let (width, height, rgba) =
        horizon_client::hud::decode_bytes(&png).map_err(|e| format!("{id}: {e}"))?;
    let (width, height, rgba) = fit(width, height, rgba);
    Ok(sheet::Image {
        width,
        height,
        rgba,
    })
}

/// What a look is drawn with: the sheets to put in the atlas, the model's own texture by its sprites
/// when it has several frames, and its layers.
type Dress = (Vec<Sheet>, Option<Texture>, Vec<anim::Overlay>);

/// The textures of the look `look` names (its sprite): the model's own, which the look needs, and each
/// layer's. A layer one of whose frames does not load is left out, and said so.
fn dress(assets: &Assets, look: &str, resolved: &Resolved, species: &str) -> Result<Dress, String> {
    let mut images = Vec::new();
    for id in resolved.texture.frames() {
        images.push(image(assets, id)?);
    }
    if images.is_empty() {
        return Err("a look without a texture".into());
    }
    // The sprites of the images from `from` on, in their order.
    let named = |from: usize, count: usize| -> Vec<String> {
        (from..from + count)
            .map(|index| frame_sprite(look, index))
            .collect()
    };
    let skin = (images.len() > 1).then(|| resolved.texture.over(&named(0, images.len())));
    let mut overlays = Vec::new();
    for layer in resolved.drawn_layers() {
        let Some(texture) = &layer.texture else {
            continue;
        };
        let frames: Result<Vec<sheet::Image>, String> = texture
            .frames()
            .iter()
            .map(|id| image(assets, id))
            .collect();
        let mut frames = match frames {
            Ok(frames) if !frames.is_empty() => frames,
            Ok(_) => continue,
            Err(e) => {
                log::warn!(
                    "Horizon: {species}'s layer `{}` is not drawn: {e}",
                    layer.name
                );
                continue;
            }
        };
        // `PosableModel.getLayer`: only a translucent layer is drawn where texels blend.
        if !layer.translucent {
            for frame in &mut frames {
                sheet::cutout(&mut frame.rgba);
            }
        }
        overlays.push(anim::Overlay {
            texture: texture.over(&named(images.len(), frames.len())),
            emissive: layer.emissive,
            translucent: layer.translucent,
        });
        images.append(&mut frames);
    }
    let sheets = sheet::pack(&images, POKEMON_SLOT_SIZE)
        .ok_or("textures that do not fit the atlas")?
        .into_iter()
        .enumerate()
        .map(|(index, packed)| Sheet {
            name: sheet_name(look, index),
            width: packed.image.width,
            height: packed.image.height,
            rgba: packed.image.rgba,
            sprites: packed
                .cells
                .iter()
                .map(|cell| {
                    (
                        frame_sprite(look, cell.image),
                        [cell.x, cell.y, cell.width, cell.height],
                    )
                })
                .collect(),
        })
        .collect();
    Ok((sheets, skin, overlays))
}

/// Everything of a look that is in files.
fn read_look(assets: &Assets, wanted: &Wanted) -> Result<Read, String> {
    let (model_id, look) = &wanted.key;
    let model = assets
        .models
        .get(path_of(model_id))
        .ok_or_else(|| format!("no model {model_id}"))?;
    let json = std::fs::read_to_string(model).map_err(|e| e.to_string())?;
    let geometry = geo::Geometry::parse(&json).map_err(|e| e.0)?;
    let poser = path_of(&wanted.resolved.poser);
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
    let profile = poser_file.as_ref().map_or((1.0, [0.0; 3]), |p| {
        (p.profile_scale, p.profile_translation)
    });
    let summary = poser_file.as_ref().map_or(profile, |p| {
        (
            p.profile_summary_scale.unwrap_or(p.profile_scale),
            p.profile_summary_translation
                .unwrap_or(p.profile_translation),
        )
    });
    let root = geometry
        .drawn_root(root_bone.as_deref(), poser)
        .ok_or("a model without a root bone")?;
    let (sheets, skin, overlays) = dress(assets, look, &wanted.resolved, &wanted.species)?;
    let mut load = |group: &str| assets.animation(group);
    let animator = poser_file
        .as_ref()
        .map(|poser| anim::Animator::new(poser, &mut load));
    let voice = poser_file
        .as_ref()
        .zip(animator.as_ref())
        .map(|(poser, animator)| sounds::Voice::new(poser, animator, &mut load))
        .unwrap_or_default();
    // What it is drawn with as time goes is the animator's to say, even for a model nothing poses.
    let animator = if skin.is_none() && overlays.is_empty() {
        animator
    } else {
        Some(
            animator
                .unwrap_or_else(anim::Animator::still)
                .dressed(skin, overlays),
        )
    };
    Ok(Read {
        root: part(root, geometry.texture_size),
        sheets,
        animator,
        voice,
        portrait,
        profile,
        summary,
        dimensions: assets.dimensions(path_of(&wanted.species)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobblelegacy_cobblemon::resolver::Layer;

    const COAT: &str = "cobblemon:textures/pokemon/0077_ponyta/ponyta.png";

    fn resolved(layers: Vec<Layer>) -> Resolved {
        Resolved {
            model: "cobblemon:ponyta.geo".into(),
            poser: "cobblemon:ponyta".into(),
            texture: Texture::Static(COAT.into()),
            layers,
        }
    }

    fn mane() -> Layer {
        Layer {
            name: "emissive".into(),
            enabled: true,
            texture: Some(Texture::Animated {
                frames: (1..=4)
                    .map(|i| format!("cobblemon:textures/pokemon/0077_ponyta/fire/normal/{i}.png"))
                    .collect(),
                fps: 10.0,
                looping: true,
                interpolation: false,
            }),
            emissive: true,
            translucent: false,
            translucent_cull: false,
        }
    }

    #[test]
    fn a_look_s_sprites_are_named_from_its_own() {
        // One texture and nothing over it: the texture's path, as the atlas named it all along.
        let plain = sprite(&resolved(Vec::new())).unwrap();
        assert_eq!(plain, "cobblemon/textures/pokemon/0077_ponyta/ponyta.png");
        // With layers, the same coat is another look: its slot holds other textures too.
        let look = sprite(&resolved(vec![mane()])).unwrap();
        assert_ne!(look, plain);
        assert!(look.starts_with("cobblemon/textures/pokemon/0077_ponyta/ponyta.png#"));
        assert_eq!(sprite(&resolved(vec![mane()])).unwrap(), look);
        // Its own texture is the look's sprite and its first sheet; the others are told by number.
        assert_eq!(frame_sprite(&look, 0), look);
        assert_eq!(frame_sprite(&look, 3), format!("{look}|3"));
        assert_eq!(sheet_name(&look, 0), look);
        assert_eq!(sheet_name(&look, 2), format!("{look}|sheet2"));
        assert_ne!(frame_sprite(&look, 1), sheet_name(&look, 1));
        // Every one of them leads back to the look.
        for name in [
            frame_sprite(&look, 0),
            frame_sprite(&look, 4),
            sheet_name(&look, 1),
        ] {
            assert_eq!(look_of(&name), look);
        }
        // What else is in the slots (a Ball, a particle's sheet) is its own look.
        let particle = "cobblemon/particle/flash#16x16";
        assert_eq!(look_of(particle), particle);
        // A texture chosen while the game runs has no sprite: no look.
        let mut variable = resolved(Vec::new());
        variable.texture = Texture::Variable;
        assert_eq!(sprite(&variable), None);
    }
}
