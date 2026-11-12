//! The player's party down the left edge of the screen (CobbleLegacy's fork): Cobblemon's `PartyOverlay`.
//!
//! A Cobblemon server keeps its client told of the player's party, a message a slot and then one for
//! each field that changes (`cobblelegacy_cobblemon::party`). Every slot has a tile: for a Pokémon its
//! portrait, level, name, sex, the ball it was caught in, a bar of its health, one of its experience,
//! and its status; an empty slot has a tile of its own. The portrait is the Pokémon's own model, the one
//! drawn in the world (`remote_pokemon.rs`), in its poser's portrait pose and framed as its poser says
//! (`drawPosablePortrait`).
//!
//! Two bars need what the server never sends: a Pokémon's full health and the experience its level
//! spans. Cobblemon's client works them out from the species' file, read here from the same resources
//! as the models ([`Facts`]); the server's own data pack may say otherwise, and is not read yet.
//!
//! The party's keys are Cobblemon's (`CobblemonKeyBinds`), at its defaults: the up and down arrows move
//! the selection, O hides the overlay, and R sends the selected Pokémon out or calls it back, whichever
//! the server finds it is not (`cobblemon:send_out_pokemon`). R acts when it is let go: turning the
//! wheel while it is held moves the selection instead, and then letting go does nothing. They have a
//! meaning only on a server that named a party; they are not in the Controls screen yet, so they
//! cannot be bound to other keys.
//!
//! Not here yet: what R does with something alive in the way that is not one's own Pokémon (a battle's
//! challenge to a wild Pokémon, a player's interactions: nothing is sent then), getting off a ridden
//! Pokémon, the icon of a ridden Pokémon, the held item, what plays when experience, a level, a move
//! or an evolution is gained, the toast that offers a starter, and portraits that move (Cobblemon's
//! default leaves them still as well).
use super::super::super::{Game, Screen, ray_box};
use super::{Assets, Look, anim, label::species_name};
use cobblelegacy_cobblemon::{
    party::{self, EXPERIENCE_UPDATE, ExperienceGroup, PartyPokemon, State, Storage},
    wire::{Gender, PoseType},
};
use horizon_client::{
    hud::{HudAtlas, HudBatch, rgba},
    mobs::{Mob, MobType},
    text::Text,
};
use horizon_core::geometry::Vec3;
use horizon_protocol::packets::{configuration::CustomPayload, play::Serverbound};
use std::{collections::HashMap, sync::Arc};
use winit::keyboard::KeyCode;

/// `PartyOverlay`'s measures, in GUI pixels.
const SLOT_HEIGHT: i32 = 30;
const SLOT_WIDTH: f32 = 62.0;
const SLOT_SPACING: i32 = 4;
const PORTRAIT: f32 = 21.0;
/// How far right the selected slot's tile is drawn.
const SELECTED_SHIFT: f32 = 6.0;
/// What the small texts and icons are drawn at.
const SCALE: f32 = 0.5;
/// The height of a full bar.
const BAR: f32 = 18.0;
/// `BattleOverlay.PORTRAIT_DIAMETER`, which `drawPosablePortrait` frames every portrait by.
const FRAMING: f32 = 28.0;

/// What a species' file says that the overlay needs and the server does not send.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Facts {
    pub(super) base_health: u32,
    pub(super) experience: Option<ExperienceGroup>,
}

/// Where `drawPosablePortrait` is asked to draw a portrait.
#[derive(Clone, Copy, Debug)]
pub(super) struct Framing {
    /// Where the caller moves to before the call, in GUI pixels.
    pub(super) origin: (f32, f32),
    /// Pixels a block of the model: 13 in the party's tiles, 18 in a battle's.
    pub(super) scale: f32,
    /// Mirrored, for a tile on the right of the screen.
    pub(super) reversed: bool,
    /// The frame the model is cut to: left, top, right, bottom.
    pub(super) clip: [f32; 4],
}

/// The look a Pokémon's portrait was given, and what it was made for.
struct Portrait {
    species: String,
    aspects: Vec<String>,
    look: Look,
}

/// The overlay's own textures, by the names the atlas gives a content mod's GUI textures.
mod sprite {
    pub(super) const PORTRAIT_BACKGROUND: &str =
        "cobblemon:gui/party/party_slot_portrait_background";
    pub(super) const SLOT: &str = "cobblemon:gui/party/party_slot";
    pub(super) const SLOT_ACTIVE: &str = "cobblemon:gui/party/party_slot_active";
    pub(super) const SLOT_FAINTED: &str = "cobblemon:gui/party/party_slot_fainted";
    pub(super) const SLOT_FAINTED_ACTIVE: &str = "cobblemon:gui/party/party_slot_fainted_active";
    pub(super) const SLOT_COLLAPSED: &str = "cobblemon:gui/party/party_slot_collapsed";
    pub(super) const MALE: &str = "cobblemon:gui/party/party_gender_male";
    pub(super) const FEMALE: &str = "cobblemon:gui/party/party_gender_female";
    pub(super) const RELEASED: &str = "cobblemon:gui/party/party_icon_released";
    pub(super) const SHOULDER_LEFT: &str = "cobblemon:gui/party/party_icon_shoulder_left";
    pub(super) const SHOULDER_RIGHT: &str = "cobblemon:gui/party/party_icon_shoulder_right";

    /// A status' icon, by the status' Showdown name.
    pub(super) fn status(name: &str) -> Option<&'static str> {
        Some(match name {
            "brn" => "cobblemon:gui/party/status_brn",
            "frz" => "cobblemon:gui/party/status_frz",
            "par" => "cobblemon:gui/party/status_par",
            "psn" => "cobblemon:gui/party/status_psn",
            "slp" => "cobblemon:gui/party/status_slp",
            "tox" => "cobblemon:gui/party/status_tox",
            _ => return None,
        })
    }
}

/// What the window knows of the player's own Pokémon.
#[derive(Default)]
pub(super) struct Party {
    storage: Storage,
    /// By species and form. `None`: the species' file is not there, or does not say.
    facts: HashMap<(String, String), Option<Facts>>,
    /// By the Pokémon's id.
    portraits: HashMap<u128, Portrait>,
    /// What each slot shows, worked out when a message last changed the party and kept until the
    /// next one: the overlay draws every frame, and reads none of the storage then.
    tiles: Option<Arc<[Option<Tile>]>>,
    /// The key that throws the selected Pokémon, while it is held.
    send: SendKey,
    /// The overlay is hidden (`HidePartyBinding.shouldHide`).
    hidden: bool,
}

/// `PartySendBinding`: the key that sends the selected Pokémon out, which acts when it is let go.
#[derive(Debug, Default, PartialEq)]
struct SendKey {
    /// Held, since a press the party took.
    down: bool,
    /// The wheel moved the selection while it was held: letting go does nothing (`actioned`).
    scrolled: bool,
    /// What the wheel has turned short of a whole step.
    wheel: f32,
}

/// How far Cobblemon looks for something alive in the way of a throw (`battleSpectateMaxDistance`).
const TARGET_REACH: f64 = 64.0;

impl Party {
    /// A message on one of the channels `cobblelegacy_cobblemon::party::reads` names.
    pub(super) fn read(
        &mut self,
        channel: &str,
        data: &[u8],
        max_level: u32,
        assets: Option<&Assets>,
    ) {
        match self.storage.read(channel, data) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                log::warn!("Horizon: Cobblemon's {channel} does not decode: {e}");
                return;
            }
        }
        // `setExperienceAndUpdateLevel`: the level is the client's to work out.
        if channel == EXPERIENCE_UPDATE {
            let facts = &mut self.facts;
            for pokemon in self.storage.party_mut() {
                let group = facts_of(facts, assets, pokemon).and_then(|facts| facts.experience);
                if let Some(group) = group {
                    pokemon.level_from(group, max_level);
                }
            }
        }
        let party = self.storage.party();
        self.portraits
            .retain(|id, _| party.iter().flatten().any(|p| p.uuid == *id));
        self.tiles = None;
    }

    /// A key pressed in play: whether it is one of the party's, which only a server that named a
    /// party gives a meaning. The arrows move the selection, O hides the overlay, and R is only held:
    /// it acts when let go ([`Self::released`]).
    fn key(&mut self, key: KeyCode) -> bool {
        if self.storage.party().is_empty() {
            return false;
        }
        match key {
            KeyCode::ArrowUp => self.storage.shift_selected(false),
            KeyCode::ArrowDown => self.storage.shift_selected(true),
            KeyCode::KeyO => self.hidden = !self.hidden,
            KeyCode::KeyR => {
                self.send = SendKey {
                    down: true,
                    ..SendKey::default()
                }
            }
            _ => return false,
        }
        true
    }

    /// The wheel turned by `steps` in play, up when positive: whether the party took it, which it
    /// does while the throwing key is held. Each whole step moves the selection, up to the Pokémon
    /// before (`MouseHandlerMixin.cobblemon$scrollParty`).
    fn wheel(&mut self, steps: f32) -> bool {
        if !self.send.down {
            return false;
        }
        self.send.wheel += steps;
        let whole = self.send.wheel.trunc();
        self.send.wheel -= whole;
        // A turn of more than the party is long comes back to where it would stop anyway.
        for _ in 0..(whole.abs() as usize).min(self.storage.party().len()) {
            self.storage.shift_selected(whole < 0.0);
            self.send.scrolled = true;
        }
        true
    }

    /// A key let go: the slot whose Pokémon the throwing key asks the server to send out or call
    /// back, when it was held, the wheel did not move the selection meanwhile, and the selected slot
    /// has a Pokémon (`PartySendBinding.onRelease`).
    fn released(&mut self, key: KeyCode) -> Option<u8> {
        if key != KeyCode::KeyR {
            return None;
        }
        let send = std::mem::take(&mut self.send);
        if !send.down || send.scrolled {
            return None;
        }
        let slot = self.storage.selected_slot()?;
        self.storage.party().get(slot)?.as_ref()?;
        u8::try_from(slot).ok()
    }

    /// Whether the entity `id` is one of the party's own Pokémon, out in the level.
    fn sent_out(&self, id: i32) -> bool {
        self.storage
            .party()
            .iter()
            .flatten()
            .any(|pokemon| matches!(pokemon.state, State::SentOut { entity, .. } if entity == id))
    }

    /// The player's party as the server told it, for the summary screen.
    pub(super) fn storage(&self) -> &Storage {
        &self.storage
    }

    /// The same to change what the client itself decides of a Pokémon; the tiles are made again.
    pub(super) fn storage_mut(&mut self) -> &mut Storage {
        self.tiles = None;
        &mut self.storage
    }

    /// The textures the portraits wear, which keep their slots in the atlas.
    pub(super) fn textures(&self) -> impl Iterator<Item = String> + '_ {
        self.portraits.values().map(|p| p.look.kind.texture.clone())
    }

    /// `texture` lost its slot in the atlas: the portraits that wore it are made again.
    pub(super) fn forget_texture(&mut self, texture: &str) {
        self.portraits.retain(|_, p| p.look.kind.texture != texture);
    }
}

/// The species' facts for `pokemon`, read from its file the first time.
fn facts_of(
    known: &mut HashMap<(String, String), Option<Facts>>,
    assets: Option<&Assets>,
    pokemon: &PartyPokemon,
) -> Option<Facts> {
    let key = (pokemon.species.clone(), pokemon.form.clone());
    if let Some(facts) = known.get(&key) {
        return *facts;
    }
    // Without the resources nothing is kept: they may be there by the next message.
    let facts = assets?.facts(super::path_of(&pokemon.species), &pokemon.form);
    if facts.is_none() {
        log::warn!(
            "Horizon: no base stats for {} ({}): its health bar stays full",
            pokemon.species,
            pokemon.form
        );
    }
    known.insert(key, facts);
    facts
}

/// What one tile shows of its Pokémon.
#[derive(Clone, Debug, PartialEq)]
struct Tile {
    uuid: u128,
    species: String,
    aspects: Vec<String>,
    name: Text,
    level: u32,
    /// The level as the tile writes it.
    level_text: Text,
    health: u32,
    /// `None`: the species' base stats are not known.
    max_health: Option<u32>,
    /// How much of its level's experience it has, 0 to 1.
    experience: f32,
    gender: Gender,
    /// The sprite of the ball it was caught in.
    ball: String,
    status: Option<&'static str>,
    /// The icon of a Pokémon that is out of its ball (`PokemonState.getIcon`): in the level, or on
    /// a shoulder.
    out: Option<&'static str>,
}

impl Tile {
    fn of(pokemon: &PartyPokemon, facts: Option<Facts>) -> Self {
        let group = facts.and_then(|facts| facts.experience);
        Self {
            uuid: pokemon.uuid,
            species: pokemon.species.clone(),
            aspects: pokemon.aspects.clone(),
            // `Pokemon.getDisplayName`: the nickname, unless it is empty.
            name: pokemon
                .nickname
                .as_ref()
                .map(horizon_client::entity_nbt::name_text)
                .filter(|name| !name.plain().is_empty())
                .unwrap_or_else(|| species_name(&pokemon.species)),
            level: pokemon.level,
            level_text: Text::literal(pokemon.level.to_string()),
            health: pokemon.health,
            max_health: facts.map(|facts| {
                party::max_health(
                    &pokemon.species,
                    facts.base_health,
                    pokemon.health_iv,
                    pokemon.health_ev,
                    pokemon.level,
                )
            }),
            experience: group.map_or(0.0, |group| {
                experience_ratio(group, pokemon.level, pokemon.experience)
            }),
            gender: pokemon.gender,
            ball: match pokemon.caught_ball.split_once(':') {
                Some((namespace, path)) => format!("{namespace}:gui/ball/{path}"),
                None => format!("cobblemon:gui/ball/{}", pokemon.caught_ball),
            },
            status: pokemon.status_icon(),
            // A ridden Pokémon has an icon of its own (`party_icon_mounted`): not told apart yet.
            out: match pokemon.state {
                State::Inactive => None,
                State::SentOut { .. } => Some(sprite::RELEASED),
                State::Shouldered { left: true } => Some(sprite::SHOULDER_LEFT),
                State::Shouldered { left: false } => Some(sprite::SHOULDER_RIGHT),
            },
        }
    }

    fn fainted(&self) -> bool {
        self.health == 0
    }

    /// How full the health bar is. With a full health nobody knows, full unless fainted.
    fn health_ratio(&self) -> f32 {
        match self.max_health {
            Some(max) if max > 0 => (self.health as f32 / max as f32).clamp(0.0, 1.0),
            _ => f32::from(u8::from(!self.fainted())),
        }
    }
}

/// How far through `level` a Pokémon with `experience` is, as the overlay's blue bar shows it.
fn experience_ratio(group: ExperienceGroup, level: u32, experience: u32) -> f32 {
    // Level 1 starts at none, whatever the curve says below it.
    let start = if level == 1 {
        0
    } else {
        group.experience(level)
    };
    let span = group
        .experience(level + 1)
        .saturating_sub(group.experience(level));
    if span == 0 {
        return 0.0;
    }
    (experience.saturating_sub(start) as f32 / span as f32).clamp(0.0, 1.0)
}

/// The top of slot `index` of a party of `slots` on a screen `height` GUI pixels high: the tiles are
/// centred on the left edge, a little above the middle.
fn slot_top(height: i32, slots: usize, index: usize) -> i32 {
    let total = slots as i32 * SLOT_HEIGHT;
    let start = (height / 2 - total / 2) - (SLOT_SPACING * 5) / 2;
    start + (SLOT_HEIGHT + SLOT_SPACING) * index as i32
}

/// `getDepletableRedGreen`: green down to half, through yellow to red at a fifth; and the overlay's
/// own dimming and blue.
pub(super) fn health_colour(ratio: f32) -> u32 {
    let red = if ratio > 0.2 { 2.0 - 2.0 * ratio } else { 1.0 };
    let green = if ratio > 0.5 {
        1.0
    } else if ratio > 0.2 {
        ratio / 0.5
    } else {
        0.0
    };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
    rgba(byte(red * 0.8), byte(green * 0.8), byte(0.27), 255)
}

/// One of the overlay's own textures ([`sprite`]), stretched to `w` by `h` as `blitk` does.
fn image(batch: &mut HudBatch, atlas: &HudAtlas, name: &str, x: f32, y: f32, w: f32, h: f32) {
    batch.sprite_scaled(atlas, name, x, y, w, h, u32::MAX);
}

/// `drawScaledText` at the overlay's half scale: white, from `x` or centred on it.
pub(super) fn small_text(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    text: &Text,
    x: f32,
    y: f32,
    centered: bool,
    shadow: bool,
) {
    let start = batch.vertices.len();
    let left = if centered {
        x - atlas.font.width_text(text) / 2.0
    } else {
        x
    };
    batch.text_styled(atlas, text, left, y, u32::MAX, shadow);
    batch.scale_since(start, x, y, SCALE, SCALE);
}

/// A bar that fills from its foot: `ratio` of [`BAR`] high, `width` wide, its foot 23 pixels under
/// the tile's top.
fn bar(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    x: f32,
    top: f32,
    width: f32,
    ratio: f32,
    colour: u32,
) {
    let height = ratio * BAR;
    if height > 0.0 {
        batch.fill(atlas, x, top + (BAR - height) + 5.0, width, height, colour);
    }
}

impl Game<'_> {
    /// The party's tiles, when it has a Pokémon (`PartyOverlay.render`).
    pub(in super::super::super) fn draw_pokemon_party(&mut self, batch: &mut HudBatch) {
        // `PartyOverlay.canRender`: under no screen but the chat's, and not with F3's.
        if !matches!(self.screen, Screen::Playing | Screen::Chat) || self.debug {
            return;
        }
        // `CobblemonClient.beforeChatRender`: during a battle its own overlay takes this one's place.
        if self
            .remote
            .as_ref()
            .is_some_and(|r| r.pokemon.party.hidden || r.pokemon.fight.fighting())
        {
            return;
        }
        let Some((tiles, selected)) = self.pokemon_party_tiles() else {
            return;
        };
        let Some(template) = self.res.mob_types().get(super::STAND_IN).cloned() else {
            return;
        };
        let looks: Vec<Option<Look>> = tiles
            .iter()
            .map(|tile| {
                let tile = tile.as_ref()?;
                self.pokemon_portrait(tile.uuid, &tile.species, &tile.aspects, &template)
            })
            .collect();
        let (_, height) = self.gui_size();
        let atlas = &self.gui.atlas;
        let level = Text::tr("cobblemon.ui.lv", &[]);
        for (index, tile) in tiles.iter().enumerate() {
            let top = slot_top(height as i32, tiles.len(), index) as f32;
            let active = selected == Some(index);
            let shift = if active { SELECTED_SHIFT } else { 0.0 };
            // The portrait goes under the tile, which frames it.
            if tile.is_some() {
                let (x, y) = (22.0 + shift, top + 2.0);
                image(
                    batch,
                    atlas,
                    sprite::PORTRAIT_BACKGROUND,
                    x,
                    y,
                    PORTRAIT,
                    PORTRAIT,
                );
                if let Some(look) = &looks[index] {
                    // `PartyOverlay`: the model 12 pixels above the frame's top, at 13 a block.
                    let framing = Framing {
                        origin: (x + PORTRAIT / 2.0 - 1.0, y - 12.0),
                        scale: 13.0,
                        reversed: false,
                        clip: [x, y, x + PORTRAIT, y + PORTRAIT],
                    };
                    self.draw_pokemon_portrait(batch, look, framing);
                }
            }
            let slot = match tile {
                None => sprite::SLOT_COLLAPSED,
                Some(tile) => match (tile.fainted(), active) {
                    (false, false) => sprite::SLOT,
                    (false, true) => sprite::SLOT_ACTIVE,
                    (true, false) => sprite::SLOT_FAINTED,
                    (true, true) => sprite::SLOT_FAINTED_ACTIVE,
                },
            };
            image(batch, atlas, slot, 0.0, top, SLOT_WIDTH, SLOT_HEIGHT as f32);
            let Some(tile) = tile else {
                continue;
            };
            if let Some(out) = tile.out {
                image(
                    batch,
                    atlas,
                    out,
                    shift + 8.0,
                    top + 3.0,
                    24.0 * SCALE,
                    17.0 * SCALE,
                );
            }
            small_text(batch, atlas, &level, shift + 6.5, top + 13.5, true, true);
            small_text(
                batch,
                atlas,
                &tile.level_text,
                shift + 6.5,
                top + 18.0,
                true,
                true,
            );
            small_text(
                batch,
                atlas,
                &tile.name,
                shift + 2.5,
                top + 25.0,
                false,
                false,
            );
            let gender = match tile.gender {
                Gender::Male => Some(sprite::MALE),
                Gender::Female => Some(sprite::FEMALE),
                Gender::Genderless => None,
            };
            if let Some(gender) = gender {
                image(
                    batch,
                    atlas,
                    gender,
                    shift + 40.0,
                    top + 25.0,
                    5.0 * SCALE,
                    7.0 * SCALE,
                );
            }
            let health = tile.health_ratio();
            bar(
                batch,
                atlas,
                shift + 46.0,
                top,
                2.0,
                health,
                health_colour(health),
            );
            bar(
                batch,
                atlas,
                shift + 49.0,
                top,
                1.0,
                tile.experience,
                rgba(51, 165, 214, 255),
            );
            // The ball's texture holds two, one over the other: the upper one for a Pokémon in its
            // ball, the lower one (the ball open) for one that is out.
            if let Some(mut ball) = atlas.sprites.get(&tile.ball).copied() {
                let middle = (ball.v0 + ball.v1) / 2.0;
                if tile.out.is_some() {
                    ball.v0 = middle;
                } else {
                    ball.v1 = middle;
                }
                let (w, h) = (18.0 * SCALE, 22.0 * SCALE);
                batch.rect_uv(shift + 43.5, top + 22.0, w, h, ball, u32::MAX);
            }
            let status = tile.status.filter(|_| !tile.fainted());
            if let Some(status) = status.and_then(sprite::status) {
                image(batch, atlas, status, shift + 51.0, top + 8.0, 4.0, 14.0);
            }
        }
    }

    /// What each slot of the player's party shows, and the selected slot; none without a Pokémon.
    fn pokemon_party_tiles(&mut self) -> Option<(Arc<[Option<Tile>]>, Option<usize>)> {
        let pokemon = &mut self.remote.as_mut()?.pokemon;
        let party = &mut pokemon.party;
        let selected = party.storage.selected_slot();
        if let Some(tiles) = &party.tiles {
            return (!tiles.is_empty()).then(|| (tiles.clone(), selected));
        }
        // No tile without a Pokémon: an empty list says so until the party changes.
        let tiles: Arc<[Option<Tile>]> = if party.storage.party().iter().all(Option::is_none) {
            Arc::new([])
        } else {
            let assets = pokemon.assets.get_or_insert_with(Assets::load).as_deref();
            let facts = &mut party.facts;
            party
                .storage
                .party()
                .iter()
                .map(|slot| {
                    let member = slot.as_ref()?;
                    Some(Tile::of(member, facts_of(facts, assets, member)))
                })
                .collect()
        };
        party.tiles = Some(tiles.clone());
        (!tiles.is_empty()).then_some((tiles, selected))
    }

    /// The look of `tile`'s portrait: the one it has, or a new one when its species or aspects changed.
    pub(super) fn pokemon_portrait(
        &mut self,
        uuid: u128,
        species: &str,
        aspects: &[String],
        template: &MobType,
    ) -> Option<Look> {
        let portraits = &self.remote.as_ref()?.pokemon.party.portraits;
        if let Some(portrait) = portraits.get(&uuid)
            && portrait.species == species
            && portrait.aspects == aspects
        {
            return Some(portrait.look.clone());
        }
        // A look that does not load is asked for again each frame; `pokemon_look` remembers which.
        let look = self.pokemon_look(species, aspects, template)?;
        self.remote.as_mut()?.pokemon.party.portraits.insert(
            uuid,
            Portrait {
                species: species.to_owned(),
                aspects: aspects.to_vec(),
                look: look.clone(),
            },
        );
        Some(look)
    }

    /// `drawPosablePortrait`: the model in its portrait pose, turned a little to its right, framed by
    /// its poser and cut to `framing`'s rectangle.
    pub(super) fn draw_pokemon_portrait(
        &self,
        batch: &mut HudBatch,
        look: &Look,
        framing: Framing,
    ) {
        use glam::{Mat4, Vec3 as V};
        let (scale, [x, y, z]) = (look.portrait_scale, look.portrait_translation);
        // `reversed`: the model looks the other way, for the tiles on the right.
        let side = if framing.reversed { -1.0 } else { 1.0 };
        let by = framing.scale;
        let gui = Mat4::from_translation(V::new(framing.origin.0, framing.origin.1, 0.0))
            * Mat4::from_translation(V::new(0.0, FRAMING + 2.0, 0.0))
            * Mat4::from_scale(V::new(by, by, -by))
            * Mat4::from_translation(V::new(0.0, -FRAMING / 18.0, 0.0))
            * Mat4::from_translation(V::new(x * side, y + 1.5 * scale, z - 4.0))
            * Mat4::from_scale(V::new(scale, scale, 1.0 / scale))
            * Mat4::from_rotation_y((-32.0 * side).to_radians())
            * Mat4::from_rotation_x(5f32.to_radians());
        // At rest: Cobblemon's portraits do not move unless its client is set to.
        self.draw_pokemon_model(batch, look, PoseType::Portrait, 0.0, gui, framing.clip);
    }

    /// A Pokémon's model in the GUI: in `pose`, its animations `age` ticks in, placed by `gui` (what
    /// Cobblemon's pose stack holds when it renders the model) and cut to `clip` (left, top, right,
    /// bottom).
    pub(super) fn draw_pokemon_model(
        &self,
        batch: &mut HudBatch,
        look: &Look,
        pose: PoseType,
        age: f32,
        gui: glam::Mat4,
        clip: [f32; 4],
    ) {
        use glam::{Mat4, Vec3 as V};
        // Stood at the origin, facing the viewer.
        let mut mob = Mob::new(0, look.kind.clone(), Vec3::new(0.0, 0.0, 0.0), 180.0);
        mob.previous = mob.position;
        (mob.body_yaw, mob.previous_body_yaw) = (180.0, 180.0);
        (mob.head_yaw, mob.previous_head_yaw) = (180.0, 180.0);
        (mob.head_pitch, mob.previous_head_pitch) = (0.0, 0.0);
        mob.extra.model_scale = 1.0;
        mob.extra.variant = anim::with_pose(0, pose);
        mob.age = age.max(0.0) as u32;
        let atlas = &self.gui.atlas;
        let lookup = |name: &str| atlas.sprites.get(name).map(|r| [r.u0, r.v0, r.u1, r.v1]);
        let mut vertices = Vec::new();
        horizon_client::mobs::cem_anim::in_gui(|| {
            horizon_client::mobs::render::render_with(
                &mob,
                age.max(0.0).fract(),
                glam::DVec3::ZERO,
                &lookup,
                240 | 240 << 8,
                self.res.mob_data(),
                &[],
                &mut vertices,
            );
        });
        // The mob renderer stood the model in the world, y up and sized by its species; the frame
        // is laid out around the model itself, y down and unsized.
        let unsized_model = 1.0 / look.base_scale;
        let gui = gui * Mat4::from_scale(V::new(-unsized_model, -unsized_model, unsized_model));
        // The faces turned to the viewer, each at its own depth: a model's small parts (an eye on a
        // cheek) sit a hair in front of the face they are on, and on the cheek turned away their
        // middle is farther than the face's, so ordering whole faces by distance hid them.
        let mut quads: Vec<(f32, [[f32; 3]; 4], [[f32; 2]; 4], u32)> = Vec::new();
        for quad in vertices.as_chunks::<4>().0 {
            let p: [V; 4] =
                std::array::from_fn(|i| gui.transform_point3(V::from_array(quad[i].pos)));
            let normal = (p[1] - p[0]).cross(p[2] - p[0]);
            if normal.z >= 0.0 {
                continue;
            }
            let normal = normal.normalize_or_zero();
            let shade = (0.55 + 0.45 * -normal.z - 0.15 * normal.y).clamp(0.4, 1.0);
            let colour = quad[0].color;
            let channel = |shift: u32| ((((colour >> shift) & 255) as f32 * shade) as u32) << shift;
            quads.push((
                p.iter().map(|v| v.z).sum::<f32>() / 4.0,
                p.map(|v| v.to_array()),
                std::array::from_fn(|i| quad[i].uv),
                (colour & 0xFF00_0000) | channel(0) | channel(8) | channel(16),
            ));
        }
        // Far to near all the same, for the texels that let what is behind show through.
        quads.sort_by(|a, b| a.0.total_cmp(&b.0));
        let start = batch.vertices.len();
        for (_, corners, uv, colour) in quads {
            batch.quad_3d(corners, uv, colour);
        }
        // Tested for depth and cut to the frame, as an item's model in a slot.
        batch.depth_since(start, clip);
    }

    /// A key pressed in play on a server: whether the party took it ([`Party::key`]).
    pub(in super::super::super) fn pokemon_party_key(&mut self, key: KeyCode) -> bool {
        // The summary's key first (`SummaryBinding`): it opens a screen of its own.
        self.pokemon_summary_open_key(key)
            || self
                .remote
                .as_mut()
                .is_some_and(|r| r.pokemon.party.key(key))
    }

    /// The wheel turned in play: whether the party took it ([`Party::wheel`]).
    pub(in super::super::super) fn pokemon_party_wheel(&mut self, steps: f32) -> bool {
        self.remote
            .as_mut()
            .is_some_and(|r| r.pokemon.party.wheel(steps))
    }

    /// A key let go, whatever is open: the throwing key sends its request when the player is in
    /// play and may throw (`PartySendBinding.onRelease`).
    pub(in super::super::super) fn pokemon_party_key_released(&mut self, key: KeyCode) {
        let Some(slot) = self
            .remote
            .as_mut()
            .and_then(|r| r.pokemon.party.released(key))
        else {
            return;
        };
        if self.screen != Screen::Playing || self.spectator() {
            return;
        }
        // In a battle the key brings its screen back (`toggleBattleScreen`).
        if self
            .remote
            .as_ref()
            .is_some_and(|r| r.pokemon.fight.fighting())
        {
            self.open_pokemon_battle_screen();
            return;
        }
        // `checkForTargetInteractions`: with nothing alive in the way, or one's own Pokémon, the
        // selected Pokémon is thrown or called back; a wild Pokémon there is challenged. A player there
        // has interactions in Cobblemon, which are not here yet: nothing is sent.
        let own = |id| {
            self.remote
                .as_ref()
                .is_some_and(|r| r.pokemon.party.sent_out(id))
        };
        match self.pokemon_party_target() {
            Some(id) if !own(id) => self.pokemon_battle_challenge(id, usize::from(slot)),
            _ => {
                if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
                    log::info!(
                        "Horizon: asked the server to send out or call back party slot {slot}"
                    );
                }
                self.pokemon_send(party::SEND_OUT_POKEMON, party::send_out(slot));
            }
        }
    }

    /// A message for the Cobblemon server, once the player is in the game.
    pub(super) fn pokemon_send(&self, channel: &str, data: Vec<u8>) {
        let joined = self
            .remote
            .as_ref()
            .is_some_and(|r| r.joined && r.reconfiguring.is_none());
        if joined {
            self.send(Serverbound::CustomPayload(CustomPayload {
                channel: channel.into(),
                data,
            }));
        }
    }

    /// The Pokémon in slot `slot` of the player's party.
    pub(super) fn pokemon_party_member(&self, slot: usize) -> Option<&PartyPokemon> {
        self.remote
            .as_ref()?
            .pokemon
            .party
            .storage
            .party()
            .get(slot)?
            .as_ref()
    }

    /// `traceFirstEntityCollision` as the throwing key asks it: the nearest mob the player's view
    /// goes through within Cobblemon's 64 blocks, unless a block in the player's reach stands before
    /// it. Other players are not looked for yet.
    pub(super) fn pokemon_party_target(&self) -> Option<i32> {
        let (eye, view) = self.aim()?;
        self.mobs
            .iter()
            .filter(|m| m.alive())
            .filter_map(|m| Some((ray_box(eye, view, TARGET_REACH, m.bounds())?, m.id as i32)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .filter(|&(distance, _)| self.before_block(distance))
            .map(|(_, id)| id)
    }

    /// The party as `horizon-ctl status` tells it, for a test to read: each slot as its tile was last
    /// drawn (species, name, level, bars, whether its portrait has its model), and the selected slot.
    /// Null without a party.
    pub(in super::super::super) fn pokemon_party_status(&self) -> serde_json::Value {
        let Some(party) = self.remote.as_ref().map(|r| &r.pokemon.party) else {
            return serde_json::Value::Null;
        };
        let slots: Vec<serde_json::Value> = party
            .storage
            .party()
            .iter()
            .map(|slot| {
                let Some(member) = slot else {
                    return serde_json::Value::Null;
                };
                // What the overlay read of the species the last time it drew, without reading more.
                let key = (member.species.clone(), member.form.clone());
                let tile = Tile::of(member, party.facts.get(&key).copied().flatten());
                serde_json::json!({
                    "species": tile.species,
                    "name": tile.name.plain(),
                    "level": tile.level,
                    "health": tile.health,
                    "max_health": tile.max_health,
                    "experience": tile.experience,
                    "status": tile.status,
                    "out": tile.out.map(|icon| icon.rsplit('_').next().unwrap_or(icon)),
                    "portrait": party.portraits.contains_key(&tile.uuid),
                })
            })
            .collect();
        if slots.is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::json!({
            "selected": party.storage.selected_slot(),
            "hidden": party.hidden,
            "slots": slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobblelegacy_cobblemon::party::{SET_PARTY_POKEMON, SET_PARTY_REFERENCE};

    /// A party of six slots with one Pokémon in the first, as a server's messages make it.
    fn with_one(pokemon: &str) -> Party {
        let store = 7u128.to_be_bytes();
        let mut held = Party::default();
        let mut initialize = vec![0];
        initialize.extend_from_slice(&store);
        initialize.push(6);
        held.read(party::INITIALIZE_PARTY, &initialize, 100, None);
        let (tag, _) = horizon_world::nbt::parse_snbt(pokemon).unwrap();
        let mut set = store.to_vec();
        set.extend_from_slice(&[0, 0]);
        let limits = horizon_world::nbt::Limits::default();
        set.extend_from_slice(&horizon_world::nbt::write_network(&tag, limits).unwrap());
        held.read(SET_PARTY_POKEMON, &set, 100, None);
        held.read(SET_PARTY_REFERENCE, &store, 100, None);
        assert_eq!(held.storage.selected_slot(), Some(0));
        held
    }

    const PIKACHU: &str = r#"{UUID:[I;1,2,3,4],Species:"cobblemon:pikachu",FormId:"normal",Level:12,
        Experience:1900,Health:16,Gender:"FEMALE",Shiny:0b,CaughtBall:"cobblemon:great_ball",
        ForcedAspects:["female"],IVs:{Base:{"cobblemon:hp":14},HyperTrained:{}},EVs:{"cobblemon:hp":0},
        Status:{StatusName:"cobblemon:paralysis",StatusTimer:10},Nickname:{text:"Sparky"}}"#;
    /// Pikachu's: base HP 35, medium fast.
    const FACTS: Facts = Facts {
        base_health: 35,
        experience: Some(ExperienceGroup::MediumFast),
    };

    /// A second Pokémon, in the third slot.
    fn with_two() -> Party {
        let mut held = with_one(PIKACHU);
        let (tag, _) =
            horizon_world::nbt::parse_snbt(&PIKACHU.replace("[I;1,2,3,4]", "[I;5,6,7,8]")).unwrap();
        let mut set = 7u128.to_be_bytes().to_vec();
        set.extend_from_slice(&[0, 2]);
        let limits = horizon_world::nbt::Limits::default();
        set.extend_from_slice(&horizon_world::nbt::write_network(&tag, limits).unwrap());
        held.read(SET_PARTY_POKEMON, &set, 100, None);
        held
    }

    #[test]
    fn the_throwing_key_asks_for_the_selected_slot_when_it_is_let_go() {
        let mut held = with_two();
        // Pressed, it is taken and nothing is asked yet; let go, it asks for the selected slot.
        assert!(held.key(KeyCode::KeyR));
        assert_eq!(held.released(KeyCode::KeyR), Some(0));
        // Let go without a press of its own (it went down behind a screen), or another key: nothing.
        assert_eq!(held.released(KeyCode::KeyR), None);
        assert!(held.key(KeyCode::KeyR));
        assert_eq!(held.released(KeyCode::KeyQ), None);
        assert_eq!(held.released(KeyCode::KeyR), Some(0));
        // The arrows move the selection past the empty slot between the two, and round.
        assert!(held.key(KeyCode::ArrowDown));
        assert_eq!(held.storage.selected_slot(), Some(2));
        assert!(held.key(KeyCode::KeyR));
        assert_eq!(held.released(KeyCode::KeyR), Some(2));
        assert!(held.key(KeyCode::ArrowDown));
        assert_eq!(held.storage.selected_slot(), Some(0));
        assert!(held.key(KeyCode::ArrowUp));
        assert_eq!(held.storage.selected_slot(), Some(2));
        // Any other key is not the party's.
        assert!(!held.key(KeyCode::KeyQ));
    }

    #[test]
    fn the_wheel_moves_the_selection_while_the_throwing_key_is_held_and_nothing_is_thrown() {
        let mut held = with_two();
        // Not held: the wheel is the hotbar's.
        assert!(!held.wheel(-1.0));
        assert_eq!(held.storage.selected_slot(), Some(0));
        assert!(held.key(KeyCode::KeyR));
        // Down a step: the next Pokémon. The party takes the wheel, and letting go throws nothing.
        assert!(held.wheel(-1.0));
        assert_eq!(held.storage.selected_slot(), Some(2));
        assert_eq!(held.released(KeyCode::KeyR), None);
        // Up a step: the one before. Half steps add up, and the wheel is taken meanwhile.
        assert!(held.key(KeyCode::KeyR));
        assert!(held.wheel(0.5));
        assert_eq!(held.storage.selected_slot(), Some(2));
        assert!(held.wheel(0.5));
        assert_eq!(held.storage.selected_slot(), Some(0));
        assert_eq!(held.released(KeyCode::KeyR), None);
        // Held and let go with the wheel still, or short of a step: thrown.
        assert!(held.key(KeyCode::KeyR));
        assert!(held.wheel(0.25));
        assert_eq!(held.released(KeyCode::KeyR), Some(0));
    }

    #[test]
    fn the_keys_are_the_party_s_only_where_a_server_named_one() {
        let mut none = Party::default();
        for key in [
            KeyCode::KeyR,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
            KeyCode::KeyO,
        ] {
            assert!(!none.key(key));
        }
        assert_eq!(none.released(KeyCode::KeyR), None);
        assert!(!none.wheel(1.0));
        // O hides the overlay and shows it again.
        let mut held = with_one(PIKACHU);
        assert!(held.key(KeyCode::KeyO) && held.hidden);
        assert!(held.key(KeyCode::KeyO) && !held.hidden);
        // An entity is the party's own when the server said one of its Pokémon is out as it.
        assert!(!held.sent_out(41));
        let mut update = 7u128.to_be_bytes().to_vec();
        update.extend_from_slice(&0x0000_0001_0000_0002_0000_0003_0000_0004u128.to_be_bytes());
        update.extend_from_slice(&[8]);
        update.extend_from_slice(b"sent-out");
        update.extend_from_slice(&41i32.to_be_bytes());
        update.extend_from_slice(&[19]);
        update.extend_from_slice(b"minecraft:overworld");
        held.read(party::STATE_UPDATE, &update, 100, None);
        assert!(held.sent_out(41) && !held.sent_out(42));
        let pokemon = held.storage.party()[0].as_ref().unwrap();
        assert_eq!(Tile::of(pokemon, None).out, Some(sprite::RELEASED));
    }

    #[test]
    fn a_tile_says_what_the_pokemon_s_own_overlay_would() {
        let held = with_one(PIKACHU);
        let pokemon = held.storage.party()[0].as_ref().unwrap();
        let tile = Tile::of(pokemon, Some(FACTS));
        assert_eq!(tile.name.plain(), "Sparky");
        assert_eq!(
            (tile.level, tile.health, tile.max_health),
            (12, 16, Some(32))
        );
        assert_eq!(tile.health_ratio(), 0.5);
        // Level 12 starts at 1728 and ends at 2197: 172 of 469.
        assert!((tile.experience - 172.0 / 469.0).abs() < 1e-6);
        assert_eq!(tile.ball, "cobblemon:gui/ball/great_ball");
        assert_eq!(tile.status, Some("par"));
        assert_eq!(tile.gender, Gender::Female);
        assert!(!tile.fainted());
        // Without the species' file: a full bar while it stands, no experience.
        let unknown = Tile::of(pokemon, None);
        assert_eq!((unknown.max_health, unknown.health_ratio()), (None, 1.0));
        assert_eq!(unknown.experience, 0.0);
    }

    #[test]
    fn a_pokemon_without_a_nickname_goes_by_its_species() {
        let held = with_one(&PIKACHU.replace(r#",Nickname:{text:"Sparky"}"#, ""));
        let pokemon = held.storage.party()[0].as_ref().unwrap();
        let named = Tile::of(pokemon, None).name;
        assert_eq!(named, species_name("cobblemon:pikachu"));
        // An empty nickname is none.
        let held = with_one(&PIKACHU.replace("Sparky", ""));
        let pokemon = held.storage.party()[0].as_ref().unwrap();
        assert_eq!(Tile::of(pokemon, None).name, named);
    }

    #[test]
    fn the_tiles_are_centred_down_the_left_edge() {
        // `PartyOverlay.render` on a 480 by 270 screen (1920 by 1080 at GUI scale 4): six slots of 30
        // start 10 above where they would be centred, 34 apart.
        assert_eq!(slot_top(270, 6, 0), 135 - 90 - 10);
        assert_eq!(slot_top(270, 6, 5), 35 + 5 * 34);
        // An odd height rounds as integer halves do.
        assert_eq!(slot_top(271, 6, 0), 35);
    }

    #[test]
    fn the_health_bar_goes_from_green_through_yellow_to_red() {
        // Red, green, blue in the batch's byte order (`rgba`).
        assert_eq!(health_colour(1.0), rgba(0, 204, 68, 255));
        assert_eq!(health_colour(0.5), rgba(204, 204, 68, 255));
        // Above a fifth the red rises past what a colour holds before its dimming: full by 0.375.
        assert_eq!(health_colour(0.35), rgba(255, 142, 68, 255));
        assert_eq!(health_colour(0.2), rgba(204, 0, 68, 255));
        assert_eq!(health_colour(0.0), rgba(204, 0, 68, 255));
    }

    #[test]
    fn experience_fills_the_bar_within_the_level() {
        use ExperienceGroup::MediumFast;
        assert_eq!(experience_ratio(MediumFast, 12, 1728), 0.0);
        assert_eq!(experience_ratio(MediumFast, 12, 2197), 1.0);
        // Level 1 spans from none to level 2's 8.
        assert_eq!(experience_ratio(MediumFast, 1, 4), 0.5);
        // Experience below the level's start (a level set by hand) is an empty bar, not a wrap.
        assert_eq!(experience_ratio(MediumFast, 12, 5), 0.0);
    }
}
