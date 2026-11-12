// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a Pokémon is drawn with, from its species and aspects: Cobblemon's resolver files
//! (`bedrock/pokemon/resolvers/<nnnn_species>/*.json`) and the choice `VaryingRenderableResolver` makes
//! among their variations.
//!
//! Ids are written out in full, `namespace:path`, as `ResourceLocation.parse` reads them. The JSON is read
//! strictly where Gson is lenient, but for a boolean written as a string: that is all of Gson's leniency
//! the resolvers and posers of Cobblemon 1.8.1 need.
use crate::json;
use horizon_protocol::{Error, Result};
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, Visitor, value::MapAccessDeserializer},
};
use std::fmt;

/// One resolver file (`ModelVariationSet`).
#[derive(Clone, Debug, PartialEq)]
pub struct ResolverFile {
    /// Whose variations these are, such as `cobblemon:bulbasaur`: the file's `species` (or `name`, or
    /// `pokeball`).
    pub species: String,
    /// Where this file's variations go among those of the species' other files: lowest first, so the
    /// highest has the last word.
    pub order: i32,
    pub variations: Vec<Variation>,
}

/// `ModelAssetVariation`: what an entity having all of `aspects` is drawn with, for the keys it gives.
#[derive(Clone, Debug, PartialEq)]
pub struct Variation {
    /// Empty in the base variation, which fits every entity.
    pub aspects: Vec<String>,
    /// A poser id such as `cobblemon:bulbasaur`: the file `bulbasaur.json` under `bedrock/pokemon/posers`,
    /// or a poser built into Cobblemon.
    pub poser: Option<String>,
    /// A model id such as `cobblemon:bulbasaur_male.geo`: the file `bulbasaur_male.geo.json`, whatever
    /// folder of `bedrock/pokemon/models` it is in.
    pub model: Option<String>,
    pub texture: Option<Texture>,
    pub layers: Vec<Layer>,
}

/// `ModelTextureSupplier`: a texture id such as `cobblemon:textures/pokemon/0001_bulbasaur/bulbasaur.png`,
/// or several shown in turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Texture {
    Static(String),
    Animated {
        frames: Vec<String>,
        /// Frames per second of the entity's animation time; 1 when the file gives none.
        fps: f32,
        /// Whether the frames start over after the last one, which otherwise stays. `loop` in the file,
        /// true when it gives none.
        looping: bool,
        /// Whether each frame is blended into the next, which Cobblemon does for a layer's texture only.
        interpolation: bool,
    },
    /// The string `variable`: an NPC's texture, chosen while the game runs. No Pokémon file has it.
    Variable,
}

impl Texture {
    /// The texture id `seconds` into the entity's animation (`AnimatedModelTextureSupplier.invoke`). None
    /// for [`Texture::Variable`], and where Cobblemon fails, as it does without frames.
    pub fn at(&self, seconds: f32) -> Option<&str> {
        match self {
            Self::Static(id) => Some(id),
            Self::Animated {
                frames,
                fps,
                looping,
                ..
            } => {
                let frame = (seconds * fps).floor() as i32;
                let count = i32::try_from(frames.len()).ok()?;
                let index = if frame >= count && !looping {
                    count - 1
                } else {
                    frame.checked_rem(count)?
                };
                frames.get(usize::try_from(index).ok()?).map(String::as_str)
            }
            Self::Variable => None,
        }
    }
}

/// `ModelLayer`: the model drawn once more over itself, with another texture. Its `tint` and `scrolling`
/// are not read; no Pokémon file has them.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// What tells layers apart: a later variation's layer of the same name replaces this one. Empty when
    /// the file gives none.
    pub name: String,
    /// False takes the layer away: [`resolve`] returns none of those.
    pub enabled: bool,
    /// Cobblemon draws nothing for a layer without one.
    pub texture: Option<Texture>,
    pub emissive: bool,
    pub translucent: bool,
    pub translucent_cull: bool,
}

/// What [`resolve`] picked for one entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub model: String,
    pub poser: String,
    pub texture: Texture,
    /// In the order their names first came up.
    pub layers: Vec<Layer>,
}

/// A boolean as Gson reads one: `true`, `false`, or a string, true when it says `true` in any case
/// (`Boolean.parseBoolean`). Eleven layers of Cobblemon 1.8.1 have `"emissive": "true"`.
struct Flag(bool);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Boolean;
        impl Visitor<'_> for Boolean {
            type Value = Flag;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a boolean")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Flag, E> {
                Ok(Flag(value))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Flag, E> {
                Ok(Flag(value.eq_ignore_ascii_case("true")))
            }
        }
        deserializer.deserialize_any(Boolean)
    }
}

/// Whether a boolean of the file is on, `default` standing for one that is missing or null.
fn on(flag: Option<Flag>, default: bool) -> bool {
    flag.map_or(default, |flag| flag.0)
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawFile {
    #[serde(rename = "name", alias = "species", alias = "pokeball")]
    species: Option<String>,
    #[serde(default)]
    order: i32,
    #[serde(default)]
    variations: Vec<RawVariation>,
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawVariation {
    #[serde(default)]
    aspects: Vec<String>,
    condition: Option<IgnoredAny>,
    poser: Option<String>,
    model: Option<String>,
    texture: Option<RawTexture>,
    layers: Option<Vec<RawLayer>>,
}

/// `ModelTextureSupplierAdapter`: a string is one texture (or `variable`), an object an animated one.
enum RawTexture {
    Id(String),
    Animated(RawFrames),
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawFrames {
    #[serde(rename = "loop")]
    looping: Option<Flag>,
    fps: Option<f32>,
    frames: Option<Vec<String>>,
    interpolation: Option<Flag>,
}

impl<'de> Deserialize<'de> for RawTexture {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Supplier;
        impl<'de> Visitor<'de> for Supplier {
            type Value = RawTexture;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a texture, its id or an object of frames")
            }
            fn visit_str<E: de::Error>(self, id: &str) -> std::result::Result<RawTexture, E> {
                Ok(RawTexture::Id(id.to_owned()))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<RawTexture, A::Error> {
                RawFrames::deserialize(MapAccessDeserializer::new(map)).map(RawTexture::Animated)
            }
        }
        deserializer.deserialize_any(Supplier)
    }
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawLayer {
    #[serde(default)]
    name: String,
    enabled: Option<Flag>,
    texture: Option<RawTexture>,
    emissive: Option<Flag>,
    translucent: Option<Flag>,
    translucent_cull: Option<Flag>,
}

/// `rootBone` is all that is read of a poser file.
#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawPoser {
    #[serde(rename = "rootBone", default, deserialize_with = "given")]
    root_bone: Option<String>,
}

/// A `rootBone` that is there is a string: Cobblemon fails on a null.
fn given<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

json::object_only!(RawFile, RawVariation, RawLayer, RawPoser);

fn bad(message: impl fmt::Display) -> Error {
    Error::new(format!("resolver: {message}"))
}

/// `ResourceLocation.parse`: `namespace:path` in vanilla's characters, `minecraft` standing in for a missing
/// namespace.
fn identifier(text: &str) -> Result<String> {
    let (namespace, path) = match text.split_once(':') {
        None => ("minecraft", text),
        Some(("", path)) => ("minecraft", path),
        Some(parts) => parts,
    };
    let valid = |part: &str, slash: bool| {
        part.bytes().all(|b| {
            matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.') || (slash && b == b'/')
        })
    };
    if valid(namespace, false) && valid(path, true) {
        Ok(format!("{namespace}:{path}"))
    } else {
        Err(bad(format_args!("`{text}` is not an id")))
    }
}

fn texture(raw: RawTexture) -> Result<Texture> {
    Ok(match raw {
        RawTexture::Id(id) if id == "variable" => Texture::Variable,
        RawTexture::Id(id) => Texture::Static(identifier(&id)?),
        RawTexture::Animated(raw) => Texture::Animated {
            frames: raw
                .frames
                .ok_or_else(|| bad("an animated texture needs frames"))?
                .into_iter()
                .map(|frame| identifier(&frame))
                .collect::<Result<_>>()?,
            fps: raw.fps.unwrap_or(1.0),
            looping: on(raw.looping, true),
            interpolation: on(raw.interpolation, false),
        },
    })
}

impl ResolverFile {
    /// Reads one resolver file.
    ///
    /// A variation with a `condition` is an error: Cobblemon evaluates that MoLang against the entity, which
    /// nothing here can, and no Pokémon file of Cobblemon 1.8.1 has one.
    pub fn parse(json: &str) -> Result<Self> {
        let file: RawFile = json::parse(json, "resolver")?;
        let species = file
            .species
            .ok_or_else(|| bad("no species"))
            .and_then(|species| identifier(&species))?;
        let variations = file
            .variations
            .into_iter()
            .map(|variation| {
                if variation.condition.is_some() {
                    return Err(bad(format_args!(
                        "a variation of {species} has a condition"
                    )));
                }
                Ok(Variation {
                    aspects: variation.aspects,
                    poser: variation.poser.as_deref().map(identifier).transpose()?,
                    model: variation.model.as_deref().map(identifier).transpose()?,
                    texture: variation.texture.map(texture).transpose()?,
                    layers: variation
                        .layers
                        .into_iter()
                        .flatten()
                        .map(|layer| {
                            Ok(Layer {
                                name: layer.name,
                                enabled: on(layer.enabled, true),
                                texture: layer.texture.map(texture).transpose()?,
                                emissive: on(layer.emissive, false),
                                translucent: on(layer.translucent, false),
                                translucent_cull: on(layer.translucent_cull, false),
                            })
                        })
                        .collect::<Result<_>>()?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            species,
            order: file.order,
            variations,
        })
    }
}

/// What an entity with `aspects` is drawn with, `files` being every resolver file of its species.
///
/// As `VaryingRenderableResolver` does: the files' variations are put in one list, by the files' `order`,
/// and those whose aspects the entity all has fit. The poser, the model and the texture are each taken from
/// the last fitting variation that gives one, so not always from the same variation. Layers are gathered
/// from every fitting variation, a later one replacing an earlier one of the same name.
///
/// Files of one `order` keep the order they are given in. Cobblemon has them by path (a resource listing
/// is sorted), so give them that way: twelve species of Cobblemon 1.8.1 have such files.
///
/// None when no fitting variation gives a poser, a model or a texture, where Cobblemon throws.
pub fn resolve(files: &[ResolverFile], aspects: &[String]) -> Option<Resolved> {
    let mut files: Vec<&ResolverFile> = files.iter().collect();
    files.sort_by_key(|file| file.order);
    let fitting: Vec<&Variation> = files
        .iter()
        .flat_map(|file| &file.variations)
        .filter(|variation| {
            variation
                .aspects
                .iter()
                .all(|aspect| aspects.contains(aspect))
        })
        .collect();
    let poser = fitting.iter().rev().find_map(|v| v.poser.clone())?;
    let model = fitting.iter().rev().find_map(|v| v.model.clone())?;
    let texture = fitting.iter().rev().find_map(|v| v.texture.clone())?;
    let mut layers: Vec<Layer> = Vec::new();
    for layer in fitting.iter().flat_map(|variation| &variation.layers) {
        match layers.iter_mut().find(|known| known.name == layer.name) {
            Some(known) => *known = layer.clone(),
            None => layers.push(layer.clone()),
        }
    }
    layers.retain(|layer| layer.enabled);
    Some(Resolved {
        model,
        poser,
        texture,
        layers,
    })
}

/// A poser file's top-level `rootBone`: the name of the model's root part that poser draws, when the file
/// gives one. [`crate::geo::Geometry::drawn_root`] finds the part, with or without it.
pub fn poser_root_bone(json: &str) -> Result<Option<String>> {
    json::parse::<RawPoser>(json, "poser").map(|poser| poser.root_bone)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "species": "cobblemon:testmon",
        "order": 0,
        "variations": [
            {
                "aspects": [],
                "poser": "cobblemon:testmon",
                "model": "cobblemon:testmon.geo",
                "texture": "cobblemon:textures/pokemon/testmon.png",
                "layers": [
                    {"name": "glow", "texture": "cobblemon:textures/pokemon/testmon_glow.png", "emissive": true}
                ]
            },
            {
                "aspects": ["shiny"],
                "texture": "cobblemon:textures/pokemon/testmon_shiny.png",
                "layers": [
                    {"name": "sparkle", "texture": {
                        "frames": ["cobblemon:sparkle_a.png", "cobblemon:sparkle_b.png", "cobblemon:sparkle_c.png"],
                        "fps": 10
                    }, "translucent": true},
                    {"name": "glow", "texture": "cobblemon:textures/pokemon/testmon_glow_shiny.png", "emissive": "true"}
                ]
            }
        ]
    }"#;

    const FEMALE: &str = r#"{
        "species": "cobblemon:testmon",
        "order": 1,
        "variations": [
            {
                "aspects": ["female"],
                "model": "cobblemon:testmon_female.geo",
                "texture": "cobblemon:textures/pokemon/testmon_female.png"
            },
            {
                "aspects": ["shiny", "female"],
                "texture": "cobblemon:textures/pokemon/testmon_female_shiny.png"
            }
        ]
    }"#;

    /// The species' two files, the later one first: `order` puts them right.
    fn files() -> Vec<ResolverFile> {
        vec![
            ResolverFile::parse(FEMALE).unwrap(),
            ResolverFile::parse(BASE).unwrap(),
        ]
    }

    fn pick(files: &[ResolverFile], aspects: &[&str]) -> Option<Resolved> {
        let aspects: Vec<String> = aspects.iter().map(|&aspect| aspect.to_owned()).collect();
        resolve(files, &aspects)
    }

    fn still(texture: &str) -> Texture {
        Texture::Static(format!("cobblemon:textures/pokemon/{texture}.png"))
    }

    fn layer(name: &str, texture: Texture) -> Layer {
        Layer {
            name: name.to_owned(),
            enabled: true,
            texture: Some(texture),
            emissive: false,
            translucent: false,
            translucent_cull: false,
        }
    }

    #[test]
    fn an_entity_without_aspects_gets_the_base_variation() {
        // An aspect no variation asks for changes nothing.
        for aspects in [&[][..], &["male", "galarian"]] {
            assert_eq!(
                pick(&files(), aspects).unwrap(),
                Resolved {
                    model: "cobblemon:testmon.geo".into(),
                    poser: "cobblemon:testmon".into(),
                    texture: still("testmon"),
                    layers: vec![Layer {
                        emissive: true,
                        ..layer("glow", still("testmon_glow"))
                    }],
                }
            );
        }
    }

    #[test]
    fn each_key_comes_from_the_last_fitting_variation_that_gives_it() {
        // Shiny: only the texture changes, the model and the poser stay the base's.
        let shiny = pick(&files(), &["shiny"]).unwrap();
        assert_eq!(
            (shiny.model.as_str(), shiny.poser.as_str(), &shiny.texture),
            (
                "cobblemon:testmon.geo",
                "cobblemon:testmon",
                &still("testmon_shiny")
            )
        );
        // Female, from the file of the higher order: its model and texture, the base's poser and layer.
        let female = pick(&files(), &["female"]).unwrap();
        assert_eq!(
            (
                female.model.as_str(),
                female.poser.as_str(),
                &female.texture
            ),
            (
                "cobblemon:testmon_female.geo",
                "cobblemon:testmon",
                &still("testmon_female")
            )
        );
        assert_eq!(female.layers.len(), 1);
        // Both: three variations give a texture and fit, the last of them wins.
        let both = pick(&files(), &["female", "shiny"]).unwrap();
        assert_eq!(
            (both.model.as_str(), &both.texture),
            (
                "cobblemon:testmon_female.geo",
                &still("testmon_female_shiny")
            )
        );
        assert_eq!(both.layers, shiny.layers);
    }

    #[test]
    fn a_later_layer_replaces_the_one_of_its_name_where_that_one_was() {
        let shiny = pick(&files(), &["shiny"]).unwrap();
        assert_eq!(
            shiny.layers,
            [
                // `"emissive": "true"`, a string, as some of Cobblemon's own files have it.
                Layer {
                    emissive: true,
                    ..layer("glow", still("testmon_glow_shiny"))
                },
                Layer {
                    translucent: true,
                    ..layer(
                        "sparkle",
                        Texture::Animated {
                            frames: vec![
                                "cobblemon:sparkle_a.png".into(),
                                "cobblemon:sparkle_b.png".into(),
                                "cobblemon:sparkle_c.png".into(),
                            ],
                            fps: 10.0,
                            looping: true,
                            interpolation: false,
                        }
                    )
                },
            ]
        );

        // A layer turned off by a later variation is gone, and a file's place is its `order`'s alone.
        let mut with_night = files();
        with_night.insert(
            0,
            ResolverFile::parse(
                r#"{"species": "cobblemon:testmon", "order": 2, "variations": [
                    {"aspects": ["night"], "layers": [
                        {"name": "glow", "enabled": false},
                        {"name": "moon", "texture": "cobblemon:moon.png", "translucent_cull": true}
                    ]}
                ]}"#,
            )
            .unwrap(),
        );
        assert_eq!(
            pick(&with_night, &["night"]).unwrap().layers,
            [Layer {
                translucent_cull: true,
                ..layer("moon", Texture::Static("cobblemon:moon.png".into()))
            }]
        );
        assert_eq!(pick(&with_night, &[]).unwrap().layers.len(), 1);
    }

    #[test]
    fn files_of_one_order_keep_the_order_they_are_given_in() {
        let file = |texture: &str| {
            ResolverFile::parse(&format!(
                r#"{{"species": "cobblemon:testmon", "variations": [
                    {{"poser": "cobblemon:testmon", "model": "cobblemon:testmon.geo", "texture": "{texture}"}}
                ]}}"#
            ))
            .unwrap()
        };
        let (first, second) = (file("cobblemon:first.png"), file("cobblemon:second.png"));
        assert_eq!((first.order, second.order), (0, 0));
        let texture = |files: &[ResolverFile]| pick(files, &[]).unwrap().texture;
        assert_eq!(
            texture(&[first.clone(), second.clone()]),
            Texture::Static("cobblemon:second.png".into())
        );
        assert_eq!(
            texture(&[second, first]),
            Texture::Static("cobblemon:first.png".into())
        );
    }

    #[test]
    fn nothing_is_resolved_without_a_poser_a_model_and_a_texture() {
        let female = [ResolverFile::parse(FEMALE).unwrap()];
        // No variation fits; then one does, but none gives a poser.
        assert_eq!(pick(&female, &[]), None);
        assert_eq!(pick(&female, &["female"]), None);
        assert_eq!(pick(&[], &["female"]), None);
    }

    #[test]
    fn an_animated_texture_shows_its_frames_in_turn() {
        let texture = |extra: &str| {
            let file = ResolverFile::parse(&format!(
                r#"{{"name": "testmon", "variations": [{{"texture": {{
                    "frames": ["cobblemon:a.png", "b.png", ":c.png"]{extra}
                }}}}]}}"#
            ))
            .unwrap();
            file.variations[0].texture.clone().unwrap()
        };
        // Ids are written out; one frame a second unless the file says more.
        assert_eq!(
            texture(""),
            Texture::Animated {
                frames: vec![
                    "cobblemon:a.png".into(),
                    "minecraft:b.png".into(),
                    "minecraft:c.png".into()
                ],
                fps: 1.0,
                looping: true,
                interpolation: false,
            }
        );
        let fast = texture(r#", "fps": 4, "interpolation": true"#);
        let frames: Vec<_> = [0.0, 0.24, 0.25, 0.6, 0.75, 1.3]
            .iter()
            .map(|&seconds| fast.at(seconds).unwrap())
            .collect();
        assert_eq!(
            frames,
            [
                "cobblemon:a.png",
                "cobblemon:a.png",
                "minecraft:b.png",
                "minecraft:c.png",
                "cobblemon:a.png",
                "minecraft:c.png"
            ]
        );
        assert!(matches!(
            fast,
            Texture::Animated {
                interpolation: true,
                ..
            }
        ));
        // Not looping, the last frame stays.
        let once = texture(r#", "fps": 4, "loop": false"#);
        assert_eq!(once.at(0.3), Some("minecraft:b.png"));
        assert_eq!(once.at(0.75), Some("minecraft:c.png"));
        assert_eq!(once.at(60.0), Some("minecraft:c.png"));
        // Cobblemon indexes its list with a frame before the first, and fails.
        assert_eq!(fast.at(-0.1), None);
        assert_eq!(
            still("testmon").at(3.0),
            Some("cobblemon:textures/pokemon/testmon.png")
        );
        assert_eq!(Texture::Variable.at(0.0), None);
    }

    #[test]
    fn a_file_is_named_by_any_of_three_keys_and_ids_are_checked() {
        let species = |json: &str| ResolverFile::parse(json).map(|file| file.species);
        assert_eq!(
            species(r#"{"name": "cobblemon:a"}"#).unwrap(),
            "cobblemon:a"
        );
        assert_eq!(
            species(r#"{"pokeball": "cobblemon:b"}"#).unwrap(),
            "cobblemon:b"
        );
        assert_eq!(
            species(r#"{"species": "c", "order": 3}"#).unwrap(),
            "minecraft:c"
        );
        let error = |json: &str| ResolverFile::parse(json).unwrap_err().0;
        assert!(error(r#"{"order": 1, "variations": []}"#).contains("no species"));
        assert!(
            error(r#"{"species": "cobblemon:Testmon"}"#)
                .contains("`cobblemon:Testmon` is not an id")
        );
        assert!(error(r#"{"species": "a:b:c"}"#).contains("is not an id"));
        let variation = |keys: &str| {
            error(&format!(
                r#"{{"species": "a:b", "variations": [{{{keys}}}]}}"#
            ))
        };
        assert!(variation(r#""model": "my model.geo""#).contains("`my model.geo` is not an id"));
        assert!(variation(r#""texture": {"fps": 2}"#).contains("needs frames"));
        assert!(variation(r#""texture": 7"#).contains("expected a texture"));
        // An array is not read as an object's fields in order.
        let listed = error(r#"{"species": "a:b", "variations": [[["shiny"], null, "a:b"]]}"#);
        assert!(listed.contains("expected an object"), "{listed}");
        assert!(
            variation(r#""layers": [{"name": "x", "emissive": 1}]"#).contains("expected a boolean")
        );
        // MoLang, which this cannot weigh: better no file than a wrong pick.
        assert!(variation(r#""condition": "q.is_shiny""#).contains("has a condition"));
        assert_eq!(
            ResolverFile::parse(
                r#"{"species": "a:b", "variations": [{"texture": "variable", "condition": null}]}"#
            )
            .unwrap()
            .variations[0]
                .texture,
            Some(Texture::Variable)
        );
        for cut in 0..BASE.len() {
            assert!(ResolverFile::parse(&BASE[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn a_poser_names_its_root_bone_or_none() {
        let poser = r#"{"portraitScale": 1.5, "rootBone": "testmon", "poses": {"standing": {"poseTypes": ["STAND"]}}}"#;
        assert_eq!(poser_root_bone(poser).unwrap().as_deref(), Some("testmon"));
        assert_eq!(poser_root_bone(r#"{"poses": {}}"#).unwrap(), None);
        // Only the top level's counts.
        assert_eq!(
            poser_root_bone(r#"{"poses": {"rootBone": "deep"}}"#).unwrap(),
            None
        );
        for bad in [
            r#"{"rootBone": 5}"#,
            r#"{"rootBone": null}"#,
            r#"["rootBone"]"#,
            "",
            "{",
        ] {
            let error = poser_root_bone(bad).unwrap_err();
            assert!(error.0.starts_with("poser: "), "{bad}: {}", error.0);
        }
    }
}
