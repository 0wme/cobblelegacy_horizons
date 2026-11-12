// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Cobblemon model: a Bedrock geometry file (`*.geo.json`) as the tree of parts and boxes Cobblemon 1.8.1
//! builds from it (`TexturedModel.createWithUvOverride`, which fills vanilla 1.21.1 `ModelPart`s), and each
//! box's faces as vanilla's `ModelPart.Cube` bakes them.
//!
//! Everything is in vanilla's model part space: pixels (sixteenths of a block), Y pointing down. A part sits
//! at `offset` from its parent and is then turned by `rotation` (`ModelPart.translateAndRotate`).
//!
//! A file Cobblemon loads no model from is an error here. The JSON is read strictly where Gson is lenient
//! (comments, unquoted names); no geometry of Cobblemon 1.8.1 needs the leniency.
use crate::json;
use horizon_protocol::{Error, Result};
use serde::{
    Deserialize, Deserializer,
    de::{
        IgnoredAny, MapAccess, SeqAccess, Visitor,
        value::{MapAccessDeserializer, SeqAccessDeserializer},
    },
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    marker::PhantomData,
};

/// `LocatorAccess.PREFIX`: what a locator's name gets in front of it, as a part.
pub const LOCATOR_PREFIX: &str = "internal_locator__";

/// How deep bones may nest. A part tree is walked by recursion (to draw it, to drop it), so a file is not
/// allowed to make that arbitrarily deep; Cobblemon 1.8.1's deepest Pokémon has 25 bones in a line.
const MAX_DEPTH: usize = 256;

/// One geometry file, as the parts Cobblemon builds from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// `texture_width` and `texture_height`, in pixels: what [`Cube::quads`] divides a box's UV by.
    pub texture_size: [u32; 2],
    /// The parts directly under the model: the bones without a parent, in file order, then the empty
    /// `internal_locator__root` part Cobblemon adds to every model.
    pub roots: Vec<Bone>,
}

/// A part: a bone, or one of the two things Cobblemon turns into parts, a rotated cube and a locator.
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    /// A bone's own name, `%<bone>%<n>` for that bone's nth rotated cube (from 0), or
    /// `internal_locator__<locator>`.
    pub name: String,
    /// Pixels from the parent part's origin, Y down.
    pub offset: [f32; 3],
    /// Radians around x, y and z, applied as vanilla's `ModelPart` does (`rotationZYX`): Rz * Ry * Rx, so a
    /// point is turned around x first.
    pub rotation: [f32; 3],
    /// The bone's cubes that have no rotation of their own, in file order.
    pub cubes: Vec<Cube>,
    /// The child bones in file order, then the bone's rotated cubes (a part each, holding that cube), then
    /// its locators (empty parts). Vanilla keeps children in a hash map, so their order means nothing there.
    pub children: Vec<Bone>,
}

/// A box of a part, in that part's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cube {
    /// Where the box starts; it reaches `size` further. Y is already turned down.
    pub min: [f32; 3],
    /// A few Cobblemon models have negative sizes: nothing treats them apart, the box is inside out.
    pub size: [f32; 3],
    /// How far every side is pushed outwards. The UV layout does not grow with it.
    pub inflate: f32,
    /// Box UV: the texture pixel where the box's unfolded faces start.
    pub uv: [f32; 2],
    pub mirror: bool,
}

/// One face of a [`Cube`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    /// Which way the face looks, in the part's space: one axis, at length 1.
    pub normal: [f32; 3],
    /// x, y, z in pixels of the part's space, then u, v as fractions of the texture.
    pub vertices: [[f32; 5]; 4],
}

/// `ModelPart.Cube`'s eight vertices, `vertex` to `vertex8`: for x, y and z, 0 is the low side and 1 the
/// high one.
const CORNERS: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 1, 0],
    [0, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [1, 1, 1],
    [0, 1, 1],
];

/// `ModelPart.Cube`'s six polygons, in the order its constructor makes them: down, up, west, north, east,
/// south. For each, its four [`CORNERS`], its texture rectangle `[u1, v1, u2, v2]` as indices into the
/// box-UV layout's six columns and three rows, and `Direction.step()`. Y points down in part space, so
/// "down" (0, -1, 0) is the face seen from above.
const FACES: [([usize; 4], [usize; 4], [f32; 3]); 6] = [
    ([5, 4, 0, 1], [1, 0, 2, 1], [0.0, -1.0, 0.0]),
    ([2, 3, 7, 6], [2, 1, 3, 0], [0.0, 1.0, 0.0]),
    ([0, 4, 7, 3], [0, 1, 1, 2], [-1.0, 0.0, 0.0]),
    ([1, 0, 3, 2], [1, 1, 2, 2], [0.0, 0.0, -1.0]),
    ([5, 1, 2, 6], [2, 1, 4, 2], [1.0, 0.0, 0.0]),
    ([4, 5, 6, 7], [4, 1, 5, 2], [0.0, 0.0, 1.0]),
];

impl Cube {
    /// The six faces vanilla 1.21.1 draws for this box (`ModelPart.Cube` and `ModelPart.Polygon`), in its
    /// order: down, up, west, north, east, south. All six always, a flat box included.
    ///
    /// `mirror` swaps the box's two x sides before the vertices are made, then reverses each face's vertices
    /// and negates its normal's x. The UV layout is measured with `size`, not the inflated box, from `uv`
    /// cut to whole pixels (vanilla holds it as integers).
    pub fn quads(&self, texture_size: [u32; 2]) -> Vec<Quad> {
        let [dx, dy, dz] = self.size;
        let mut low = self.min.map(|c| c - self.inflate);
        let mut high: [f32; 3] = std::array::from_fn(|i| self.min[i] + self.size[i] + self.inflate);
        if self.mirror {
            std::mem::swap(&mut low[0], &mut high[0]);
        }
        let sides = [low, high];
        let [u, v] = self.uv.map(|c| c as i32 as f32);
        let us = [
            u,
            u + dz,
            u + dz + dx,
            u + dz + dx + dx,
            u + dz + dx + dz,
            u + dz + dx + dz + dx,
        ];
        let vs = [v, v + dz, v + dz + dy];
        let [width, height] = texture_size.map(|side| side as f32);
        // `Polygon` moves each UV by "0 / size", written out so the result is vanilla's bit for bit.
        let (du, dv) = (0.0 / width, 0.0 / height);
        FACES
            .iter()
            .map(|&(corners, rect, step)| {
                let (u1, v1, u2, v2) = (us[rect[0]], vs[rect[1]], us[rect[2]], vs[rect[3]]);
                let uvs = [
                    [u2 / width - du, v1 / height + dv],
                    [u1 / width + du, v1 / height + dv],
                    [u1 / width + du, v2 / height - dv],
                    [u2 / width - du, v2 / height - dv],
                ];
                let mut vertices: [[f32; 5]; 4] = std::array::from_fn(|k| {
                    let [x, y, z] = CORNERS[corners[k]];
                    [sides[x][0], sides[y][1], sides[z][2], uvs[k][0], uvs[k][1]]
                });
                let mut normal = step;
                if self.mirror {
                    vertices.reverse();
                    normal[0] = -normal[0];
                }
                Quad { normal, vertices }
            })
            .collect()
    }
}

/// A JSON object's entries in file order, which `serde_json`'s own map keeps only under a feature.
struct Ordered<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Entries<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for Entries<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Ordered(entries))
            }
        }
        deserializer.deserialize_map(Entries(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawFile {
    #[serde(rename = "minecraft:geometry")]
    geometry: Option<Vec<RawGeometry>>,
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawGeometry {
    description: Option<RawDescription>,
    bones: Option<Vec<RawBone>>,
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawDescription {
    texture_width: Option<f64>,
    texture_height: Option<f64>,
}

/// `ModelBone`. Bedrock's bone-level `mirror` is not among its fields: Cobblemon never reads it.
#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawBone {
    #[serde(default)]
    name: String,
    parent: Option<String>,
    pivot: Option<Vec<f32>>,
    rotation: Option<Vec<f32>>,
    cubes: Option<Vec<RawCube>>,
    locators: Option<Ordered<RawLocator>>,
}

#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawCube {
    origin: Option<Vec<f32>>,
    size: Option<Vec<f32>>,
    pivot: Option<Vec<f32>>,
    rotation: Option<Vec<f32>>,
    uv: Option<RawUv>,
    inflate: Option<f32>,
    mirror: Option<bool>,
}

/// A cube's `uv`: `[u, v]` for box UV, or Bedrock's object of faces, which Cobblemon cannot read.
enum RawUv {
    Box(Vec<f64>),
    PerFace,
}

impl<'de> Deserialize<'de> for RawUv {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Uv;
        impl<'de> Visitor<'de> for Uv {
            type Value = RawUv;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a uv, `[u, v]`")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                Vec::deserialize(SeqAccessDeserializer::new(seq)).map(RawUv::Box)
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RawUv::PerFace)
            }
        }
        deserializer.deserialize_any(Uv)
    }
}

/// `LocatorBoneAdapter`: a locator is its offset alone, or an object of `offset` and `rotation`.
#[derive(Deserialize)]
#[serde(remote = "Self")]
struct RawLocator {
    offset: Option<Vec<f32>>,
    rotation: Option<Vec<f32>>,
}

impl<'de> Deserialize<'de> for RawLocator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Locator;
        impl<'de> Visitor<'de> for Locator {
            type Value = RawLocator;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a locator, `[x, y, z]` or an object")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                Ok(RawLocator {
                    offset: Some(Vec::deserialize(SeqAccessDeserializer::new(seq))?),
                    rotation: None,
                })
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                RawLocator::deserialize(MapAccessDeserializer::new(map))
            }
        }
        deserializer.deserialize_any(Locator)
    }
}

json::object_only!(RawFile, RawGeometry, RawDescription, RawBone, RawCube);

fn bad(message: impl fmt::Display) -> Error {
    Error::new(format!("geometry: {message}"))
}

/// The first three numbers of a list, which are the ones Cobblemon indexes; it fails on a shorter list.
fn vec3(list: &[f32], what: impl FnOnce() -> String) -> Result<[f32; 3]> {
    match list {
        [x, y, z, ..] => Ok([*x, *y, *z]),
        _ => Err(bad(format_args!("{} needs three numbers", what()))),
    }
}

fn pivot(bone: &RawBone) -> Result<[f32; 3]> {
    vec3(bone.pivot.as_deref().unwrap_or_default(), || {
        format!("the pivot of bone `{}`", bone.name)
    })
}

/// Where a part goes under its parent, from the two Bedrock pivots: x and z keep their sign, y turns over.
fn offset(parent: [f32; 3], own: [f32; 3]) -> [f32; 3] {
    [
        -(parent[0] - own[0]),
        parent[1] - own[1],
        -(parent[2] - own[2]),
    ]
}

/// `Math.toRadians(degrees.toDouble()).toFloat()` on each angle; the signs stay as the file has them.
fn radians(degrees: [f32; 3]) -> [f32; 3] {
    degrees.map(|angle| f64::from(angle).to_radians() as f32)
}

/// Gson reads these as `Int`: a whole number, however it is written.
fn whole(number: f64) -> bool {
    number.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&number)
}

fn texture_side(side: Option<f64>, key: &str) -> Result<u32> {
    match side {
        Some(pixels) if whole(pixels) && pixels >= 1.0 => Ok(pixels as u32),
        _ => Err(bad(format_args!(
            "{key} must be a whole number of pixels, 1 or more"
        ))),
    }
}

fn box_uv(uv: &RawUv, cube: &dyn Fn() -> String) -> Result<[f32; 2]> {
    match uv {
        RawUv::Box(list) if list.len() >= 2 && list.iter().all(|&n| whole(n)) => {
            Ok([list[0] as i32 as f32, list[1] as i32 as f32])
        }
        RawUv::Box(_) => Err(bad(format_args!(
            "{} needs a uv of two whole numbers",
            cube()
        ))),
        RawUv::PerFace => Err(bad(format_args!(
            "{} has per-face UV, only box UV is read",
            cube()
        ))),
    }
}

fn empty_part(name: String, offset: [f32; 3], rotation: [f32; 3]) -> Bone {
    Bone {
        name,
        offset,
        rotation,
        cubes: Vec::new(),
        children: Vec::new(),
    }
}

/// What a bone holds besides its child bones: its plain cubes, then the parts made of its rotated cubes
/// followed by those made of its locators.
fn contents(bone: &RawBone) -> Result<(Vec<Cube>, Vec<Bone>)> {
    let mut cubes = Vec::new();
    let mut parts = Vec::new();
    // A cube without `uv` is added at the texture offset its `CubeListBuilder` was left at: the last one
    // set on the bone's shared builder, or (0, 0) on a rotated cube's own.
    let mut shared_uv = [0.0; 2];
    for (n, cube) in bone.cubes.iter().flatten().enumerate() {
        let what = || format!("cube {n} of bone `{}`", bone.name);
        let own_uv = cube.uv.as_ref().map(|uv| box_uv(uv, &what)).transpose()?;
        let uv = if cube.rotation.is_some() {
            own_uv.unwrap_or_default()
        } else {
            shared_uv = own_uv.unwrap_or(shared_uv);
            shared_uv
        };
        // A cube lacking its origin or its size adds no box, and is not an error to Cobblemon.
        let boxed = match (&cube.origin, &cube.size) {
            (Some(origin), Some(size)) => {
                let origin = vec3(origin, || format!("the origin of {}", what()))?;
                let size = vec3(size, || format!("the size of {}", what()))?;
                let around = match &cube.pivot {
                    Some(own) => vec3(own, || format!("the pivot of {}", what()))?,
                    None => pivot(bone)?,
                };
                Some(Cube {
                    min: [
                        origin[0] - around[0],
                        // Y is turned over, which also means measuring from the box's other side.
                        -(origin[1] - around[1] + size[1]),
                        origin[2] - around[2],
                    ],
                    size,
                    inflate: cube.inflate.unwrap_or(0.0),
                    uv,
                    mirror: cube.mirror == Some(true),
                })
            }
            _ => None,
        };
        let Some(rotation) = &cube.rotation else {
            cubes.extend(boxed);
            continue;
        };
        let rotation = vec3(rotation, || format!("the rotation of {}", what()))?;
        let own = cube
            .pivot
            .as_deref()
            .ok_or_else(|| bad(format_args!("{} is rotated around no pivot", what())))?;
        let own = vec3(own, || format!("the pivot of {}", what()))?;
        let rotated = parts.len();
        parts.push(Bone {
            name: format!("%{}%{rotated}", bone.name),
            offset: offset(pivot(bone)?, own),
            rotation: radians(rotation),
            cubes: boxed.into_iter().collect(),
            children: Vec::new(),
        });
    }
    let mut seen = HashSet::new();
    for (name, locator) in bone.locators.iter().flat_map(|locators| &locators.0) {
        if !seen.insert(name) {
            return Err(bad(format_args!(
                "bone `{}` has two locators named `{name}`",
                bone.name
            )));
        }
        let numbers = |list: &Option<Vec<f32>>, of: &str| match list {
            Some(list) => vec3(list, || format!("the {of} of locator `{name}`")),
            None => Ok([0.0; 3]),
        };
        parts.push(empty_part(
            format!("{LOCATOR_PREFIX}{name}"),
            offset(pivot(bone)?, numbers(&locator.offset, "offset")?),
            radians(numbers(&locator.rotation, "rotation")?),
        ));
    }
    Ok((cubes, parts))
}

impl Geometry {
    /// Reads a `*.geo.json`: its first geometry, as `TexturedModel.create` turns it into parts.
    pub fn parse(json: &str) -> Result<Self> {
        let file: RawFile = json::parse(json, "geometry")?;
        let geometry = file
            .geometry
            .and_then(|all| all.into_iter().next())
            .ok_or_else(|| bad("no minecraft:geometry[0]"))?;
        let bones = geometry.bones.ok_or_else(|| bad("no bones"))?;
        let description = geometry.description.ok_or_else(|| bad("no description"))?;
        let texture_size = [
            texture_side(description.texture_width, "texture_width")?,
            texture_side(description.texture_height, "texture_height")?,
        ];

        // Cobblemon makes the parts in file order and looks a bone's parent up among those made so far.
        let mut index = HashMap::with_capacity(bones.len());
        let mut parents = Vec::with_capacity(bones.len());
        let mut depths: Vec<usize> = Vec::with_capacity(bones.len());
        let mut built: Vec<Bone> = Vec::with_capacity(bones.len());
        let mut extras = Vec::with_capacity(bones.len());
        for (i, bone) in bones.iter().enumerate() {
            let parent = match &bone.parent {
                None => None,
                Some(name) => Some(*index.get(name.as_str()).ok_or_else(|| {
                    bad(format_args!(
                        "the parent of bone `{}`, `{name}`, is no earlier bone",
                        bone.name
                    ))
                })?),
            };
            let depth = parent.map_or(1, |p: usize| depths[p] + 1);
            if depth > MAX_DEPTH {
                return Err(bad(format_args!(
                    "bone `{}` is nested deeper than {MAX_DEPTH}",
                    bone.name
                )));
            }
            if index.insert(bone.name.as_str(), i).is_some() {
                return Err(bad(format_args!("two bones are named `{}`", bone.name)));
            }
            // A bone without a parent is put at the model's origin: its own pivot and rotation only
            // matter to what hangs from it.
            let (at, rotation) = match parent {
                None => ([0.0; 3], [0.0; 3]),
                Some(p) => (
                    offset(pivot(&bones[p])?, pivot(bone)?),
                    match &bone.rotation {
                        Some(rotation) => radians(vec3(rotation, || {
                            format!("the rotation of bone `{}`", bone.name)
                        })?),
                        None => [0.0; 3],
                    },
                ),
            };
            let (cubes, parts) = contents(bone)?;
            parents.push(parent);
            depths.push(depth);
            built.push(Bone {
                name: bone.name.clone(),
                offset: at,
                rotation,
                cubes,
                children: Vec::new(),
            });
            extras.push(parts);
        }

        // Children come after their parent, so taking the bones from the last one back, each is complete
        // (its child bones are all in, last first) when it is handed to its own parent.
        let mut roots = Vec::new();
        while let Some(mut bone) = built.pop() {
            let i = built.len();
            bone.children.reverse();
            bone.children.append(&mut extras[i]);
            match parents[i] {
                Some(p) => built[p].children.push(bone),
                None => roots.push(bone),
            }
        }
        roots.reverse();
        roots.push(empty_part(
            format!("{LOCATOR_PREFIX}root"),
            [0.0; 3],
            [0.0; 3],
        ));
        Ok(Self {
            texture_size,
            roots,
        })
    }

    /// The part a poser draws, one of the roots: the poser file's `rootBone` when a root has that name,
    /// else the root named as the poser itself (`bulbasaur` for `cobblemon:bulbasaur`), else the first root
    /// that is no locator (`VaryingModelRepository.loadJsonPoser`). That last choice follows file order
    /// here and a hash map's order in Cobblemon, which can differ in a file with several roots.
    ///
    /// A poser without a file is one Cobblemon builds in code, where it names its root: with no
    /// `rootBone`, this finds that same part for every such poser of Cobblemon 1.8.1.
    pub fn drawn_root(&self, root_bone: Option<&str>, poser: &str) -> Option<&Bone> {
        let named = |name: &str| self.roots.iter().find(|root| root.name == name);
        root_bone
            .and_then(named)
            .or_else(|| named(poser))
            .or_else(|| {
                self.roots
                    .iter()
                    .find(|root| !root.name.contains(LOCATOR_PREFIX))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, FRAC_PI_8, PI};

    /// A geometry file around `bones`, with a 64 by 32 texture.
    fn geometry(bones: &str) -> Result<Geometry> {
        Geometry::parse(&format!(
            r#"{{"format_version": "1.12.0", "minecraft:geometry": [{{
                "description": {{"identifier": "geometry.test", "texture_width": 64, "texture_height": 32}},
                "bones": {bones}
            }}]}}"#
        ))
    }

    fn names(parts: &[Bone]) -> Vec<&str> {
        parts.iter().map(|part| part.name.as_str()).collect()
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6)
    }

    #[test]
    fn a_bone_hangs_from_its_parent_by_the_difference_of_their_pivots() {
        let model = geometry(
            r#"[
                {"name": "body", "pivot": [1, 5, -2], "rotation": [90, 0, 0]},
                {"name": "head", "parent": "body", "pivot": [3, 12, -6], "rotation": [-22.5, 45, 180]},
                {"name": "tail", "parent": "body", "pivot": [1, 4.5, 3]}
            ]"#,
        )
        .unwrap();
        assert_eq!(model.texture_size, [64, 32]);
        // A bone without a parent goes to the origin, unturned, whatever its pivot and rotation.
        let body = &model.roots[0];
        assert_eq!(
            (body.name.as_str(), body.offset, body.rotation),
            ("body", [0.0; 3], [0.0; 3])
        );
        assert_eq!(names(&body.children), ["head", "tail"]);
        // x and z as in the file, y turned over; the angles keep their signs.
        let head = &body.children[0];
        assert_eq!(head.offset, [2.0, -7.0, -4.0]);
        assert!(
            close(head.rotation, [-FRAC_PI_8, FRAC_PI_4, PI]),
            "{:?}",
            head.rotation
        );
        let tail = &body.children[1];
        assert_eq!((tail.offset, tail.rotation), ([0.0, 0.5, 5.0], [0.0; 3]));
    }

    #[test]
    fn a_cube_is_measured_from_its_own_pivot_or_its_bone_s() {
        let model = geometry(
            r#"[{"name": "body", "pivot": [1, 5, -2], "cubes": [
                {"origin": [-3, 4, -1], "size": [2, 3, 4], "uv": [10, 20]},
                {"origin": [-3, 4, -1], "size": [2, 3, 4], "pivot": [0, 10, 2], "uv": [0, -6], "inflate": 0.5}
            ]}]"#,
        )
        .unwrap();
        // y: the box's top, 4 + 3 = 7, is 2 above the pivot's 5, so it starts at -2 and reaches 3 down.
        assert_eq!(
            model.roots[0].cubes,
            [
                Cube {
                    min: [-4.0, -2.0, 1.0],
                    size: [2.0, 3.0, 4.0],
                    inflate: 0.0,
                    uv: [10.0, 20.0],
                    mirror: false,
                },
                Cube {
                    min: [-3.0, 3.0, -3.0],
                    size: [2.0, 3.0, 4.0],
                    inflate: 0.5,
                    uv: [0.0, -6.0],
                    mirror: false,
                },
            ]
        );
    }

    #[test]
    fn a_rotated_cube_is_a_part_of_its_own() {
        let model = geometry(
            r#"[
                {"name": "body", "pivot": [1, 5, -2], "cubes": [
                    {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0]},
                    {"origin": [-3, 4, -1], "size": [2, 3, 4], "pivot": [0, 10, 2], "rotation": [0, 0, 90], "uv": [10, 20]},
                    {"origin": [0, 0, 0], "size": [1, 2, 1], "pivot": [1, 5, -2], "rotation": [-45, 0, 0], "uv": [4, 4]}
                ]},
                {"name": "leg", "parent": "body", "pivot": [1, 5, -2]}
            ]"#,
        )
        .unwrap();
        let body = &model.roots[0];
        assert_eq!(body.cubes.len(), 1);
        assert_eq!(names(&body.children), ["leg", "%body%0", "%body%1"]);
        // Placed by its pivot against the bone's, as a bone is against its parent, and holding the cube
        // measured from that pivot.
        let first = &body.children[1];
        assert_eq!(first.offset, [-1.0, -5.0, 4.0]);
        assert!(
            close(first.rotation, [0.0, 0.0, FRAC_PI_2]),
            "{:?}",
            first.rotation
        );
        assert_eq!(
            first.cubes,
            [Cube {
                min: [-3.0, 3.0, -3.0],
                size: [2.0, 3.0, 4.0],
                inflate: 0.0,
                uv: [10.0, 20.0],
                mirror: false,
            }]
        );
        assert!(first.children.is_empty());
        let second = &body.children[2];
        assert_eq!(second.offset, [0.0; 3]);
        assert!(
            close(second.rotation, [-FRAC_PI_4, 0.0, 0.0]),
            "{:?}",
            second.rotation
        );
        assert_eq!(second.cubes[0].min, [-1.0, 3.0, 2.0]);
    }

    #[test]
    fn a_locator_is_an_empty_part_under_its_bone() {
        let model = geometry(
            r#"[
                {"name": "body", "pivot": [1, 5, -2],
                 "locators": {
                    "seat": [1, 9, 0],
                    "held": {"offset": [0, 5, -4], "rotation": [0, 90, 0]},
                    "bare": {}
                 },
                 "cubes": [{"origin": [0, 0, 0], "size": [1, 1, 1], "pivot": [1, 5, -2], "rotation": [0, 0, 0], "uv": [0, 0]}]},
                {"name": "head", "parent": "body", "pivot": [1, 5, -2]}
            ]"#,
        )
        .unwrap();
        // Bones first, then the rotated cubes, then the locators as the file lists them.
        let body = &model.roots[0];
        assert_eq!(
            names(&body.children),
            [
                "head",
                "%body%0",
                "internal_locator__seat",
                "internal_locator__held",
                "internal_locator__bare"
            ]
        );
        let [seat, held, bare] = &body.children[2..] else {
            unreachable!()
        };
        assert_eq!((seat.offset, seat.rotation), ([0.0, -4.0, 2.0], [0.0; 3]));
        assert_eq!(held.offset, [-1.0, 0.0, -2.0]);
        assert!(
            close(held.rotation, [0.0, FRAC_PI_2, 0.0]),
            "{:?}",
            held.rotation
        );
        assert_eq!((bare.offset, bare.rotation), ([-1.0, 5.0, 2.0], [0.0; 3]));
        assert!(
            body.children[2..]
                .iter()
                .all(|locator| locator.cubes.is_empty() && locator.children.is_empty())
        );
        // And the one every model gets, beside its roots.
        assert_eq!(names(&model.roots), ["body", "internal_locator__root"]);
        assert_eq!(
            model.roots[1],
            empty_part("internal_locator__root".into(), [0.0; 3], [0.0; 3])
        );
    }

    #[test]
    fn mirror_is_the_cube_s_own_and_off_when_it_gives_none() {
        // Bedrock hands a bone's `mirror` down to its cubes; Cobblemon does not read it.
        let model = geometry(
            r#"[{"name": "arm", "pivot": [0, 0, 0], "mirror": true, "cubes": [
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0]},
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0], "mirror": true},
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0], "mirror": false}
            ]}]"#,
        )
        .unwrap();
        let mirrors: Vec<bool> = model.roots[0].cubes.iter().map(|c| c.mirror).collect();
        assert_eq!(mirrors, [false, true, false]);
    }

    #[test]
    fn a_cube_without_uv_takes_the_one_its_builder_was_left_at() {
        let model = geometry(
            r#"[{"name": "body", "pivot": [0, 0, 0], "cubes": [
                {"origin": [0, 0, 0], "size": [1, 1, 1]},
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [6, 8]},
                {"origin": [0, 0, 0], "size": [1, 1, 1], "pivot": [0, 0, 0], "rotation": [0, 0, 0]},
                {"origin": [0, 0, 0], "size": [1, 1, 1]}
            ]}]"#,
        )
        .unwrap();
        let body = &model.roots[0];
        let uvs: Vec<[f32; 2]> = body.cubes.iter().map(|cube| cube.uv).collect();
        assert_eq!(uvs, [[0.0, 0.0], [6.0, 8.0], [6.0, 8.0]]);
        assert_eq!(body.children[0].cubes[0].uv, [0.0, 0.0]);
    }

    /// From (1, -2, -3), 2 wide, 3 high and 4 deep, unfolded at (10, 20) of a 64 by 32 texture.
    const BOX: Cube = Cube {
        min: [1.0, -2.0, -3.0],
        size: [2.0, 3.0, 4.0],
        inflate: 0.0,
        uv: [10.0, 20.0],
        mirror: false,
    };

    /// A face of [`BOX`], its u and v given in pixels.
    fn face(normal: [f32; 3], vertices: [[f32; 5]; 4]) -> Quad {
        Quad {
            normal,
            vertices: vertices.map(|[x, y, z, u, v]| [x, y, z, u / 64.0, v / 32.0]),
        }
    }

    #[test]
    fn a_box_has_the_six_faces_vanilla_gives_it() {
        // Box UV: the two y faces, 4 pixels deep, in rows 20 to 24, between columns 14, 16 and 18; the
        // four sides, 3 pixels high, in rows 24 to 27, between columns 10, 14, 16, 20 and 22.
        let expected = [
            face(
                [0.0, -1.0, 0.0],
                [
                    [3.0, -2.0, 1.0, 16.0, 20.0],
                    [1.0, -2.0, 1.0, 14.0, 20.0],
                    [1.0, -2.0, -3.0, 14.0, 24.0],
                    [3.0, -2.0, -3.0, 16.0, 24.0],
                ],
            ),
            face(
                [0.0, 1.0, 0.0],
                [
                    [3.0, 1.0, -3.0, 18.0, 24.0],
                    [1.0, 1.0, -3.0, 16.0, 24.0],
                    [1.0, 1.0, 1.0, 16.0, 20.0],
                    [3.0, 1.0, 1.0, 18.0, 20.0],
                ],
            ),
            face(
                [-1.0, 0.0, 0.0],
                [
                    [1.0, -2.0, -3.0, 14.0, 24.0],
                    [1.0, -2.0, 1.0, 10.0, 24.0],
                    [1.0, 1.0, 1.0, 10.0, 27.0],
                    [1.0, 1.0, -3.0, 14.0, 27.0],
                ],
            ),
            face(
                [0.0, 0.0, -1.0],
                [
                    [3.0, -2.0, -3.0, 16.0, 24.0],
                    [1.0, -2.0, -3.0, 14.0, 24.0],
                    [1.0, 1.0, -3.0, 14.0, 27.0],
                    [3.0, 1.0, -3.0, 16.0, 27.0],
                ],
            ),
            face(
                [1.0, 0.0, 0.0],
                [
                    [3.0, -2.0, 1.0, 20.0, 24.0],
                    [3.0, -2.0, -3.0, 16.0, 24.0],
                    [3.0, 1.0, -3.0, 16.0, 27.0],
                    [3.0, 1.0, 1.0, 20.0, 27.0],
                ],
            ),
            face(
                [0.0, 0.0, 1.0],
                [
                    [1.0, -2.0, 1.0, 22.0, 24.0],
                    [3.0, -2.0, 1.0, 20.0, 24.0],
                    [3.0, 1.0, 1.0, 20.0, 27.0],
                    [1.0, 1.0, 1.0, 22.0, 27.0],
                ],
            ),
        ];
        assert_eq!(BOX.quads([64, 32]), expected);
    }

    #[test]
    fn a_mirrored_box_trades_its_x_sides_and_winds_the_other_way() {
        let plain = BOX.quads([64, 32]);
        let mirrored = Cube {
            mirror: true,
            ..BOX
        }
        .quads([64, 32]);
        for (plain, mirrored) in plain.iter().zip(&mirrored) {
            // Each face keeps its texture rectangle and goes to the other x side (1 and 3 trade places),
            // its vertices in reverse.
            let mut expected = plain.vertices.map(|[x, y, z, u, v]| [4.0 - x, y, z, u, v]);
            expected.reverse();
            assert_eq!(mirrored.vertices, expected);
            let [x, y, z] = plain.normal;
            assert_eq!(mirrored.normal, [-x, y, z]);
        }
        // So the face with the leftmost strip of the sides' row, at x = 1 on the plain box, is at x = 3.
        assert_eq!(
            mirrored[2],
            face(
                [1.0, 0.0, 0.0],
                [
                    [3.0, 1.0, -3.0, 14.0, 27.0],
                    [3.0, 1.0, 1.0, 10.0, 27.0],
                    [3.0, -2.0, 1.0, 10.0, 24.0],
                    [3.0, -2.0, -3.0, 14.0, 24.0],
                ]
            )
        );
    }

    #[test]
    fn inflate_pushes_every_side_out_and_leaves_the_texture_alone() {
        let plain = BOX.quads([64, 32]);
        let inflated = Cube {
            inflate: 0.5,
            ..BOX
        }
        .quads([64, 32]);
        let middle = [2.0, -0.5, -1.0];
        for (plain, inflated) in plain.iter().zip(&inflated) {
            assert_eq!(inflated.normal, plain.normal);
            for (plain, inflated) in plain.vertices.iter().zip(&inflated.vertices) {
                for axis in 0..3 {
                    let outwards = (plain[axis] - middle[axis]).signum();
                    assert_eq!(inflated[axis], plain[axis] + 0.5 * outwards);
                }
                assert_eq!(inflated[3..], plain[3..]);
            }
        }
    }

    /// What vanilla 1.21.1 itself bakes for ten boxes: plain, mirrored, inflated, flat, inside out, at
    /// uneven coordinates, on textures of uneven sizes. Each was built as Cobblemon builds one
    /// (`CubeListBuilder.create().texOffs(u, v).mirror(mirror).addBox(x, y, z, dx, dy, dz,
    /// CubeDeformation(inflate))`, baked by `LayerDefinition.create(mesh, width, height).bakeRoot()`), then
    /// its `ModelPart.Cube.polygons` were printed. A `cube` line is u, v, x, y, z, dx, dy, dz and inflate
    /// as float bits, then mirror, width and height; each of its six `quad` lines is a polygon's normal
    /// and its four vertices (x, y, z, u, v), as float bits.
    const VANILLA: &str = include_str!("../tests/fixtures/vanilla_cubes.txt");

    #[test]
    fn quads_are_vanilla_s_to_the_bit() {
        let bits = |word: &str| u32::from_str_radix(word, 16).unwrap();
        let mut lines = VANILLA.lines();
        let mut cubes = 0;
        while let Some(line) = lines.next() {
            let words: Vec<&str> = line.split(' ').collect();
            assert_eq!(words[0], "cube");
            let n: Vec<f32> = words[1..10]
                .iter()
                .map(|word| f32::from_bits(bits(word)))
                .collect();
            let cube = Cube {
                uv: [n[0], n[1]],
                min: [n[2], n[3], n[4]],
                size: [n[5], n[6], n[7]],
                inflate: n[8],
                mirror: words[10] == "1",
            };
            let quads = cube.quads([words[11].parse().unwrap(), words[12].parse().unwrap()]);
            assert_eq!(quads.len(), 6, "{cube:?}");
            for (face, quad) in quads.iter().enumerate() {
                let expected: Vec<u32> = lines
                    .next()
                    .and_then(|line| line.strip_prefix("quad "))
                    .unwrap()
                    .split(' ')
                    .map(bits)
                    .collect();
                let actual: Vec<u32> = quad
                    .normal
                    .iter()
                    .chain(quad.vertices.iter().flatten())
                    .map(|number| number.to_bits())
                    .collect();
                assert_eq!(actual, expected, "face {face} of {cube:?}");
            }
            cubes += 1;
        }
        assert_eq!(cubes, 10);
    }

    #[test]
    fn per_face_uv_is_refused() {
        let error = geometry(
            r#"[{"name": "body", "pivot": [0, 0, 0], "cubes": [
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0]},
                {"origin": [0, 0, 0], "size": [1, 1, 1], "uv": {
                    "north": {"uv": [0, 0], "uv_size": [1, 1]},
                    "up": {"uv": [1, 0], "uv_size": [1, 1]}
                }}
            ]}]"#,
        )
        .unwrap_err();
        assert!(
            error.0.contains("cube 1 of bone `body` has per-face UV"),
            "{}",
            error.0
        );
    }

    #[test]
    fn a_file_cobblemon_loads_no_model_from_is_an_error() {
        let error = |bones: &str| geometry(bones).unwrap_err().0;
        let cube = |rest: &str| {
            format!(
                r#"[{{"name": "body", "pivot": [0, 0, 0], "cubes": [
                    {{"origin": [0, 0, 0], "size": [1, 1, 1], {rest}}}
                ]}}]"#
            )
        };
        // A parent has to come first: the parts are made in file order.
        let unborn = error(
            r#"[{"name": "head", "parent": "body", "pivot": [0, 0, 0]}, {"name": "body", "pivot": [0, 0, 0]}]"#,
        );
        assert!(unborn.contains("`body`, is no earlier bone"), "{unborn}");
        let twice = error(r#"[{"name": "body"}, {"name": "body"}]"#);
        assert!(twice.contains("two bones are named `body`"), "{twice}");
        let no_pivot = error(&cube(r#""rotation": [0, 0, 45], "uv": [0, 0]"#));
        assert!(no_pivot.contains("rotated around no pivot"), "{no_pivot}");
        let half_pixel = error(&cube(r#""uv": [0.5, 0]"#));
        assert!(half_pixel.contains("two whole numbers"), "{half_pixel}");
        assert!(error(&cube(r#""uv": [3]"#)).contains("two whole numbers"));
        assert!(error(&cube(r#""uv": "3""#)).contains("expected a uv"));
        let short = error(
            r#"[{"name": "a", "pivot": [0, 0]}, {"name": "b", "parent": "a", "pivot": [0, 0, 0]}]"#,
        );
        assert!(
            short.contains("the pivot of bone `a` needs three numbers"),
            "{short}"
        );
        let two_seats = error(
            r#"[{"name": "body", "pivot": [0, 0, 0], "locators": {"seat": [0, 0, 0], "seat": [1, 1, 1]}}]"#,
        );
        assert!(
            two_seats.contains("two locators named `seat`"),
            "{two_seats}"
        );

        // An array is not read as an object's fields in order, nor a pair of arrays as a locator's.
        let listed = error(r#"[["body", null, [0, 0, 0]]]"#);
        assert!(listed.contains("expected an object"), "{listed}");
        let pair = error(
            r#"[{"name": "body", "pivot": [0, 0, 0], "locators": {"seat": [[0, 1, 0], [0, 90, 0]]}}]"#,
        );
        assert!(pair.contains("geometry: invalid type"), "{pair}");

        // A bone alone needs no pivot: nothing is measured from it.
        assert_eq!(geometry(r#"[{"name": "a"}]"#).unwrap().roots.len(), 2);

        let whole_file = |json: &str| Geometry::parse(json).unwrap_err().0;
        assert!(whole_file("{}").contains("no minecraft:geometry[0]"));
        assert!(whole_file(r#"{"minecraft:geometry": []}"#).contains("no minecraft:geometry[0]"));
        let description = r#""description": {"texture_width": 64, "texture_height": 32}"#;
        assert!(
            whole_file(&format!(r#"{{"minecraft:geometry": [{{{description}}}]}}"#))
                .contains("no bones")
        );
        assert!(
            whole_file(r#"{"minecraft:geometry": [{"bones": []}]}"#).contains("no description")
        );
        for (description, key) in [
            (
                r#"{"texture_width": 0, "texture_height": 32}"#,
                "texture_width",
            ),
            (r#"{"texture_width": 64}"#, "texture_height"),
            (
                r#"{"texture_width": 64, "texture_height": 31.5}"#,
                "texture_height",
            ),
        ] {
            let error = whole_file(&format!(
                r#"{{"minecraft:geometry": [{{"description": {description}, "bones": []}}]}}"#
            ));
            assert!(error.contains(key), "{error}");
        }
        // The format before 1.12 keeps its geometries under other keys.
        assert!(
            whole_file(r#"{"format_version": "1.8.0", "geometry.old": {"bones": []}}"#)
                .contains("no minecraft:geometry[0]")
        );
    }

    #[test]
    fn a_cut_or_endless_file_is_an_error_not_a_panic() {
        let whole = r#"{"minecraft:geometry": [{"description": {"texture_width": 64, "texture_height": 32},
            "bones": [{"name": "body", "pivot": [0, 1, 0], "locators": {"seat": [0, 2, 0]},
                "cubes": [{"origin": [0, 0, 0], "size": [1, 1, 1], "uv": [0, 0]}]}]}]}"#;
        assert!(Geometry::parse(whole).is_ok());
        for cut in 0..whole.len() {
            assert!(Geometry::parse(&whole[..cut]).is_err(), "cut at {cut}");
        }
        // A line of bones, each the child of the one before, as long as is allowed and one longer.
        let line = |bones: usize| {
            let bones: Vec<String> = (0..bones)
                .map(|i| match i {
                    0 => r#"{"name": "0", "pivot": [0, 0, 0]}"#.to_owned(),
                    _ => format!(
                        r#"{{"name": "{i}", "parent": "{}", "pivot": [0, {i}, 0]}}"#,
                        i - 1
                    ),
                })
                .collect();
            geometry(&format!("[{}]", bones.join(",")))
        };
        let longest = line(MAX_DEPTH).unwrap();
        let mut deepest = &longest.roots[0];
        let mut depth = 1;
        while let Some(child) = deepest.children.first() {
            assert_eq!(child.offset, [0.0, -1.0, 0.0]);
            (deepest, depth) = (child, depth + 1);
        }
        assert_eq!(depth, MAX_DEPTH);
        let error = line(MAX_DEPTH + 1).unwrap_err().0;
        assert!(error.contains("nested deeper than 256"), "{error}");
    }

    #[test]
    fn a_poser_draws_the_root_it_names_or_the_one_named_after_it() {
        let model = geometry(
            r#"[
                {"name": "extra", "pivot": [0, 0, 0]},
                {"name": "pikachu", "pivot": [0, 0, 0]},
                {"name": "other", "pivot": [0, 0, 0]},
                {"name": "ear", "parent": "pikachu", "pivot": [0, 0, 0]}
            ]"#,
        )
        .unwrap();
        let drawn = |root_bone, poser| model.drawn_root(root_bone, poser).unwrap().name.as_str();
        assert_eq!(drawn(Some("other"), "pikachu"), "other");
        // Only a root can be drawn: `ear` is not one.
        assert_eq!(drawn(Some("ear"), "pikachu"), "pikachu");
        assert_eq!(drawn(None, "pikachu"), "pikachu");
        assert_eq!(drawn(None, "raichu"), "extra");
        // A model of no bones still has its root locator, which is never what is drawn.
        assert_eq!(geometry("[]").unwrap().drawn_root(None, "pikachu"), None);
    }
}
