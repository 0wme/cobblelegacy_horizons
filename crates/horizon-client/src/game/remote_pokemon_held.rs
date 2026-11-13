//! The item a Pokémon shows (CobbleLegacy's fork): Cobblemon's `HeldItemRenderer`.
//!
//! A Pokémon that holds an item is seen with it. The server keeps `PokemonEntity.SHOWN_HELD_ITEM` to
//! the item it holds (or to what is in its hand, when the held item is hidden), and Cobblemon's client
//! draws that item's own model at a locator of the Pokémon's model: `item` for most, `item_face` for
//! what is worn on the face (glasses) and `item_hat` for what is worn on the head (a band, a banner),
//! when the model has those. A model without the locator shows nothing.
//!
//! The item is an entry of the Pokémon's data like its pose, and comes one of two ways
//! (`remote_pokemon.rs`): read by vanilla's reader when nothing of a type of Cobblemon's own is before
//! it in the packet, else among the bytes that reader left, where the session measures it.
//!
//! Not here: an item shown by an animation (`q.render_item`), the item of a Pokémon on a shoulder, and
//! the item in a screen (the summary's model shows none).
use super::super::super::Game;
use super::Assets;
use cobblelegacy_cobblemon::geo::LOCATOR_PREFIX;
use glam::{Mat4, Vec3};
use horizon_assets::item_model::DisplayContext;
use horizon_client::{
    items::ItemStack,
    mesher::Vertex,
    mobs::{Mob, Species, render},
};
use std::collections::HashMap;

/// `HeldItemRenderer.ITEM`, `ITEM_FACE` and `ITEM_HAT`: the locators an item is shown at.
const ITEM: &str = "item";
const ITEM_FACE: &str = "item_face";
const ITEM_HAT: &str = "item_hat";

/// `CobblemonItemTags.WEARABLE_FACE_ITEMS`, `WEARABLE_HAT_ITEMS` and `HIDDEN_ITEMS`.
const FACE_ITEMS: &str = "cobblemon:held/visibility/face";
const HAT_ITEMS: &str = "cobblemon:held/visibility/hat";
const HIDDEN_ITEMS: &str = "cobblemon:held/visibility/hidden";

/// What the Pokémon of the level show.
#[derive(Default)]
pub(super) struct Held {
    /// The item each one shows, by entity id.
    shown: HashMap<i32, ItemStack>,
    /// Cobblemon's item tags as its own data has them, by tag, read the first time one is asked for:
    /// what the game's own tags do not have, on a client that has no data of the server's mods.
    tags: HashMap<&'static str, Vec<String>>,
}

impl Held {
    /// Entity `id` is gone.
    pub(super) fn forget(&mut self, id: i32) {
        self.shown.remove(&id);
    }

    /// The item entity `id` shows.
    fn shown(&self, id: i32) -> Option<&ItemStack> {
        self.shown.get(&id)
    }
}

impl Assets {
    /// The values of the item tag `tag` (`cobblemon:held/visibility/hat`) as the roots' data have it:
    /// a root adds to what the roots under it say, or takes its place when it says `replace`. An item's
    /// id, or `#` and another tag's.
    fn item_tag(&self, tag: &str) -> Vec<String> {
        let Some((namespace, path)) = tag.split_once(':') else {
            return Vec::new();
        };
        let mut values = Vec::new();
        // `assets` has the root that wins first; each is a root's `assets/cobblemon`.
        for assets in self.assets.iter().rev() {
            let Some(root) = assets.parent().and_then(|assets| assets.parent()) else {
                continue;
            };
            let file = root.join(format!("data/{namespace}/tags/item/{path}.json"));
            let Some(json) = std::fs::read_to_string(file)
                .ok()
                .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            else {
                continue;
            };
            if json["replace"].as_bool() == Some(true) {
                values.clear();
            }
            for value in json["values"].as_array().into_iter().flatten() {
                // An entry is an id, or an object with one that need not be there.
                if let Some(id) = value.as_str().or_else(|| value["id"].as_str()) {
                    values.push(id.to_owned());
                }
            }
        }
        values
    }
}

/// `ItemDisplayContext.valueOf`, for the names a poser may give.
fn context_named(name: &str) -> Option<DisplayContext> {
    Some(match name {
        "NONE" => DisplayContext::None,
        "THIRD_PERSON_LEFT_HAND" => DisplayContext::ThirdPersonLeftHand,
        "THIRD_PERSON_RIGHT_HAND" => DisplayContext::ThirdPersonRightHand,
        "FIRST_PERSON_LEFT_HAND" => DisplayContext::FirstPersonLeftHand,
        "FIRST_PERSON_RIGHT_HAND" => DisplayContext::FirstPersonRightHand,
        "HEAD" => DisplayContext::Head,
        "GUI" => DisplayContext::Gui,
        "GROUND" => DisplayContext::Ground,
        "FIXED" => DisplayContext::Fixed,
        _ => return None,
    })
}

/// Where an item sits from its locator, for the way it is held (`HeldItemRenderer.renderAtLocator`,
/// for a Pokémon): lying flat and half its size on a locator that says nothing; turned into the hand
/// of a model that holds it; set on the face or on the head for what is worn there.
fn placed(context: DisplayContext, face: bool, hat: bool) -> Mat4 {
    // `setOriginAndScale`: moved by so many pixels of the item at its new size, then sized.
    let set = |x: f32, y: f32, z: f32, scale: f32| {
        Mat4::from_translation(Vec3::new(x, y, z) * 0.0625 * scale)
            * Mat4::from_scale(Vec3::splat(scale))
    };
    let quarter = std::f32::consts::FRAC_PI_2;
    // A model that stands with its arms out: the item is turned into the hand, and brought back a
    // pixel onto the locator.
    let hand = |side: f32| {
        Mat4::from_rotation_y(-quarter * side)
            * Mat4::from_rotation_z(quarter * side)
            * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.0625))
    };
    match context {
        DisplayContext::Fixed => set(0.0, 0.5, 0.0, 0.5) * Mat4::from_rotation_x(quarter),
        DisplayContext::ThirdPersonRightHand => hand(1.0),
        DisplayContext::ThirdPersonLeftHand => hand(-1.0),
        DisplayContext::Head if face => set(0.0, 1.0, 7.25 - 0.5, 0.62),
        DisplayContext::Head if hat => set(0.0, -7.25 + 0.5, 0.0, 0.62),
        _ => Mat4::IDENTITY,
    }
}

impl Game<'_> {
    /// `PokemonEntity.SHOWN_HELD_ITEM`: Pokémon `id` shows `stack` from now on, or nothing. An entity
    /// that is no Pokémon has other data at that index.
    pub(in super::super::super) fn pokemon_shown_item(
        &mut self,
        id: i32,
        stack: Option<ItemStack>,
    ) {
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        if !pokemon.seen.contains_key(&id) {
            return;
        }
        // The server says it again every tick (`PokemonServerDelegate` sets it from a copy of the
        // held item, which is never the stack it had): only another item is news.
        if pokemon.held.shown.get(&id) == stack.as_ref() {
            return;
        }
        if super::anim::trace() {
            let item = stack.as_ref().map_or("nothing", |stack| &*stack.item);
            log::info!("Horizon: Pokémon {id} shows {item}");
        }
        match stack {
            Some(stack) => pokemon.held.shown.insert(id, stack),
            None => pokemon.held.shown.remove(&id),
        };
    }

    /// Whether `item` is in Cobblemon's item tag `tag`: by the game's own tags when they have it,
    /// else by Cobblemon's data, a tag it names being the game's (`#minecraft:banners`).
    fn pokemon_item_tagged(&self, tag: &'static str, item: &str) -> bool {
        let tags = &self.res.items().item_tags;
        if let Some(known) = tags.get(tag) {
            return known.contains(item);
        }
        let Some(pokemon) = self.remote.as_ref().map(|r| &r.pokemon) else {
            return false;
        };
        let values = pokemon.held.tags.get(tag);
        values.into_iter().flatten().any(|value| {
            value.as_str() == item
                || value
                    .strip_prefix('#')
                    .and_then(|other| tags.get(other))
                    .is_some_and(|other| other.contains(item))
        })
    }

    /// Once a frame: Cobblemon's item tags are read from its data, the first time a Pokémon shows
    /// an item.
    pub(super) fn pokemon_held_frame(&mut self) {
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        if pokemon.held.shown.is_empty() || !pokemon.held.tags.is_empty() {
            return;
        }
        let assets = pokemon.assets.clone().flatten();
        for tag in [FACE_ITEMS, HAT_ITEMS, HIDDEN_ITEMS] {
            let values = assets.as_ref().map_or_else(Vec::new, |a| a.item_tag(tag));
            pokemon.held.tags.insert(tag, values);
        }
    }

    /// `HeldItemRenderer.renderOnModel`: the item `mob`, a Pokémon, shows, drawn at the locator of
    /// its model that is the item's place, in the way the model holds an item there.
    pub(in super::super::super) fn pokemon_item_geometry(
        &self,
        mob: &Mob,
        camera: glam::DVec3,
        alpha: f64,
        light: u32,
        out: &mut Vec<Vertex>,
    ) {
        if mob.species() != Species::Pokemon {
            return;
        }
        let Some(pokemon) = self.remote.as_ref().map(|r| &r.pokemon) else {
            return;
        };
        let id = mob.id as i32;
        let Some(stack) = pokemon.held.shown(id) else {
            return;
        };
        let item = &*stack.item;
        if self.pokemon_item_tagged(HIDDEN_ITEMS, item) {
            return;
        }
        let data = self.res.mob_data();
        // The locator as the model's poser has it in this frame; None for a model without it.
        let at = |locator: &str| {
            let part = format!("{LOCATOR_PREFIX}{locator}");
            render::part_transform(mob, alpha as f32, camera, data, &part)
        };
        let face = self.pokemon_item_tagged(FACE_ITEMS, item);
        let hat = self.pokemon_item_tagged(HAT_ITEMS, item);
        let worn = [(face, ITEM_FACE), (hat, ITEM_HAT)]
            .into_iter()
            .filter(|(tagged, _)| *tagged)
            .find_map(|(_, locator)| Some((locator, at(locator)?)));
        let Some((locator, matrix)) = worn.or_else(|| Some((ITEM, at(ITEM)?))) else {
            return;
        };
        // The way the model says it holds an item there; else on the head for what is worn, and
        // lying flat for the rest. A word that names no way draws nothing (Cobblemon throws).
        let said = pokemon.players.animator(id);
        let said = said.and_then(|animator| animator.display_context(locator));
        let context = match said {
            Some(name) => match context_named(name) {
                Some(context) => context,
                None => return,
            },
            None if locator != ITEM => DisplayContext::Head,
            None => DisplayContext::Fixed,
        };
        // `LocatorAccess.update`: the flip every model is drawn under is undone at a locator.
        let pose =
            matrix * Mat4::from_rotation_z(std::f32::consts::PI) * placed(context, face, hat);
        let owner = super::super::super::item_draw::owner_at(
            [mob.position.x, mob.position.y, mob.position.z],
            mob.body_yaw,
            &mob.kind.id,
        );
        let hand = horizon_client::cit::Hand::Main;
        let state = self.resolve_item_in_hand(stack, context, Some(&owner), 0, hand);
        self.submit_item_world(&state, pose, light, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `matrix` puts the item's own origin and its three axes where `wanted` says.
    fn puts(matrix: Mat4, wanted: [[f32; 3]; 4]) -> bool {
        let found = [
            matrix.transform_point3(Vec3::ZERO),
            matrix.transform_vector3(Vec3::X),
            matrix.transform_vector3(Vec3::Y),
            matrix.transform_vector3(Vec3::Z),
        ];
        let close = |(found, wanted): (&Vec3, &[f32; 3])| {
            (*found - Vec3::from_array(*wanted)).abs().max_element() < 1.0e-5
        };
        found.iter().zip(&wanted).all(close)
    }

    #[test]
    fn an_item_is_placed_by_the_way_it_is_held() {
        // On a locator that says nothing: half its size, a quarter of a pixel up, lying flat (its
        // up is the locator's forward).
        assert!(puts(
            placed(DisplayContext::Fixed, false, false),
            [
                [0.0, 0.015_625, 0.0],
                [0.5, 0.0, 0.0],
                [0.0, 0.0, 0.5],
                [0.0, -0.5, 0.0]
            ]
        ));
        // In the right hand: turned a quarter twice, a pixel along what was its depth.
        assert!(puts(
            placed(DisplayContext::ThirdPersonRightHand, false, false),
            [
                [0.0625, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, -1.0],
                [-1.0, 0.0, 0.0]
            ]
        ));
        // The left hand is its mirror.
        assert!(puts(
            placed(DisplayContext::ThirdPersonLeftHand, false, false),
            [
                [-0.0625, 0.0, 0.0],
                [0.0, -1.0, 0.0],
                [0.0, 0.0, -1.0],
                [1.0, 0.0, 0.0]
            ]
        ));
        // Worn: glasses in front of the face, a band on top of the head, each at 0.62 of its size.
        let sized = [[0.62, 0.0, 0.0], [0.0, 0.62, 0.0], [0.0, 0.0, 0.62]];
        let [x, y, z] = sized;
        assert!(puts(
            placed(DisplayContext::Head, true, false),
            [[0.0, 0.038_75, 0.261_562_5], x, y, z]
        ));
        assert!(puts(
            placed(DisplayContext::Head, false, true),
            [[0.0, -0.261_562_5, 0.0], x, y, z]
        ));
        // Any other way is the locator's own frame.
        assert_eq!(placed(DisplayContext::Ground, true, true), Mat4::IDENTITY);
        assert_eq!(placed(DisplayContext::Head, false, false), Mat4::IDENTITY);
    }

    #[test]
    fn a_poser_names_the_way_as_minecraft_does() {
        assert_eq!(
            context_named("THIRD_PERSON_RIGHT_HAND"),
            Some(DisplayContext::ThirdPersonRightHand)
        );
        assert_eq!(context_named("FIXED"), Some(DisplayContext::Fixed));
        assert_eq!(context_named("HEAD"), Some(DisplayContext::Head));
        assert_eq!(context_named("third_person_right_hand"), None);
        assert_eq!(context_named(""), None);
    }
}
