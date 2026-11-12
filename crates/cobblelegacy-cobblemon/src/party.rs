// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The player's party, as a Cobblemon 1.8.1 server keeps its client told of it.
//!
//! The server never sends "the party": it names a store (`InitializePartyPacket`), puts Pokémon in its
//! slots one message each (`SetPartyPokemonPacket`, the whole Pokémon as one NBT compound,
//! `Pokemon.CLIENT_CODEC`), says which store is the player's own (`SetPartyReferencePacket`), then sends
//! what changes: a Pokémon moved, swapped or taken out, and one message per field of a Pokémon that
//! changed (`PokemonUpdatePacket`: the store's id, the Pokémon's id, the value). [`Storage`] is the
//! client's side of it, `ClientStorageManager`.
//!
//! Two numbers the party overlay shows are never sent: the most health a Pokémon can have, and the
//! experience its level starts and ends at. Cobblemon's client works them out from the species' base
//! stats and experience group ([`max_health`], [`ExperienceGroup`]).
//!
//! The other way, the client asks for one thing so far: [`send_out`], what the key that throws the
//! selected Pokémon sends. The server then sends the Pokémon out or calls it back, whichever it is not,
//! and says so with the Pokémon's new [`State`].
//!
//! Not read yet: the PC's stores (`cobblemon:initialize_pc`, `set_pc_box`...), a Pokémon's moves,
//! ability, nature and evolutions (kept as sent in [`PartyPokemon::tag`]), and the updates of the fields
//! the overlay does not show.
use crate::wire::{Gender, nullable, string};
use horizon_protocol::{Error, Result, buf::Reader};
use horizon_world::nbt::Tag;
use std::collections::HashMap;

/// `InitializePartyPacket.ID`.
pub const INITIALIZE_PARTY: &str = "cobblemon:initialize_party";
/// `SetPartyPokemonPacket.ID`.
pub const SET_PARTY_POKEMON: &str = "cobblemon:set_party_pokemon";
/// `SetPartyReferencePacket.ID`.
pub const SET_PARTY_REFERENCE: &str = "cobblemon:set_party_reference";
/// `RemoveClientPokemonPacket.ID`.
pub const REMOVE_CLIENT_POKEMON: &str = "cobblemon:remove_client_pokemon";
/// `SwapClientPokemonPacket.ID`.
pub const SWAP_CLIENT_POKEMON: &str = "cobblemon:swap_client_pokemon";
/// `MoveClientPartyPokemonPacket.ID`.
pub const MOVE_CLIENT_PARTY_POKEMON: &str = "cobblemon:move_client_party_pokemon";
/// `HealthUpdatePacket.ID`.
pub const HEALTH_UPDATE: &str = "cobblemon:health_update";
/// `ExperienceUpdatePacket.ID`.
pub const EXPERIENCE_UPDATE: &str = "cobblemon:experience_update";
/// `StatusUpdatePacket.ID`.
pub const STATUS_UPDATE: &str = "cobblemon:status_update";
/// `NicknameUpdatePacket.ID`.
pub const NICKNAME_UPDATE: &str = "cobblemon:nickname_update";
/// `SpeciesUpdatePacket.ID`.
pub const SPECIES_UPDATE: &str = "cobblemon:species_update";
/// `AspectsUpdatePacket.ID`.
pub const ASPECTS_UPDATE: &str = "cobblemon:aspects_update";
/// `CaughtBallUpdatePacket.ID`.
pub const CAUGHT_BALL_UPDATE: &str = "cobblemon:caught_ball_update";
/// `GenderUpdatePacket.ID`.
pub const GENDER_UPDATE: &str = "cobblemon:gender_update";
/// `ShinyUpdatePacket.ID`.
pub const SHINY_UPDATE: &str = "cobblemon:shiny_update";
/// `PokemonStateUpdatePacket.ID`.
pub const STATE_UPDATE: &str = "cobblemon:state_update";

/// `SendOutPokemonPacket.ID`, which the client sends.
pub const SEND_OUT_POKEMON: &str = "cobblemon:send_out_pokemon";

/// `SendOutPokemonPacket`: the payload that asks the server to send out the Pokémon in `slot` of the
/// player's party, or to call it back when it is out.
pub fn send_out(slot: u8) -> Vec<u8> {
    vec![slot]
}

/// `PokemonState`: where a Pokémon of the party is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// In its ball.
    #[default]
    Inactive,
    /// Out in the level, as the entity `entity`.
    SentOut { entity: i32, dimension: String },
    /// On its trainer's shoulder.
    Shouldered { left: bool },
}

impl State {
    /// `PokemonState.fromBuffer`: the state's name, then its own fields. A name Cobblemon does not
    /// know is its `InactivePokemonState`.
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(match string(r)?.as_str() {
            "sent-out" => Self::SentOut {
                entity: r.i32()?,
                dimension: string(r)?,
            },
            "shouldered" => {
                let left = r.bool()?;
                // The player's id, the state's and the Pokémon's.
                for _ in 0..3 {
                    r.uuid()?;
                }
                Self::Shouldered { left }
            }
            _ => Self::Inactive,
        })
    }

    /// Out of its ball, in the level or on a shoulder (`ActivePokemonState`).
    pub fn active(&self) -> bool {
        *self != Self::Inactive
    }
}

/// Whether [`Storage::read`] reads `channel`.
pub fn reads(channel: &str) -> bool {
    matches!(
        channel,
        INITIALIZE_PARTY
            | SET_PARTY_POKEMON
            | SET_PARTY_REFERENCE
            | REMOVE_CLIENT_POKEMON
            | SWAP_CLIENT_POKEMON
            | MOVE_CLIENT_PARTY_POKEMON
            | HEALTH_UPDATE
            | EXPERIENCE_UPDATE
            | STATUS_UPDATE
            | NICKNAME_UPDATE
            | SPECIES_UPDATE
            | ASPECTS_UPDATE
            | CAUGHT_BALL_UPDATE
            | GENDER_UPDATE
            | SHINY_UPDATE
            | STATE_UPDATE
    )
}

/// A species' `experienceGroup`: the experience each level starts at (`ExperienceGroups.kt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExperienceGroup {
    Erratic,
    Fast,
    MediumFast,
    MediumSlow,
    Slow,
    Fluctuating,
}

impl ExperienceGroup {
    /// `ExperienceGroups.findByName`: by name, whatever its case.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "erratic" => Self::Erratic,
            "fast" => Self::Fast,
            "medium_fast" => Self::MediumFast,
            "medium_slow" => Self::MediumSlow,
            "slow" => Self::Slow,
            "fluctuating" => Self::Fluctuating,
            _ => return None,
        })
    }

    /// `getExperience`: the experience `level` starts at. Integer math as Cobblemon's, each division
    /// rounding down where it stands.
    pub fn experience(self, level: u32) -> u32 {
        let l = i64::from(level);
        let cube = l * l * l;
        let value = match self {
            // The one curve that does not say level 1 is 0, and is below it there.
            Self::MediumSlow => cube * 6 / 5 - 15 * l * l + 100 * l - 140,
            _ if level <= 1 => 0,
            Self::Erratic => match level {
                ..50 => cube * (100 - l) / 50,
                50..68 => cube * (150 - l) / 100,
                68..98 => cube * (1911 - 10 * l) / 3 / 500,
                _ => cube * (160 - l) / 100,
            },
            Self::Fast => 4 * cube / 5,
            Self::MediumFast => cube,
            Self::Slow => 5 * cube / 4,
            Self::Fluctuating => match level {
                ..15 => cube * ((l + 1) / 3 + 24) / 50,
                15..36 => cube * (l + 14) / 50,
                _ => cube * (l / 2 + 32) / 50,
            },
        };
        u32::try_from(value.max(0)).unwrap_or(u32::MAX)
    }

    /// `CachedLevelThresholds.getLevel`: the last level `experience` has reached, up to its limit of
    /// a thousand levels.
    pub fn level(self, experience: u32) -> u32 {
        (1..1000)
            .find(|&level| experience < self.experience(level))
            .map_or(1, |level| level - 1)
    }
}

/// `CobblemonStatProvider.getStatForPokemon` for HP: the most health a Pokémon of `species` can have,
/// from the species' (or the form's) base stat and the Pokémon's own numbers.
pub fn max_health(species: &str, base: u32, iv: u32, ev: u32, level: u32) -> u32 {
    // `Pokemon.SHEDINJA`: one, whatever its numbers.
    if species == "cobblemon:shedinja" {
        return 1;
    }
    (2 * base + iv + ev / 4 + 100) * level / 100 + 10
}

/// A Pokémon as `Pokemon.CLIENT_CODEC` writes it: what the party shows of it, and the compound itself.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyPokemon {
    pub uuid: u128,
    /// `cobblemon:charizard`.
    pub species: String,
    /// The form's Showdown id, `normal` for a species' own.
    pub form: String,
    /// The nickname's text component, when the Pokémon has one.
    pub nickname: Option<Tag>,
    pub level: u32,
    pub experience: u32,
    /// `currentHealth`; 0 is fainted.
    pub health: u32,
    pub gender: Gender,
    pub shiny: bool,
    /// `cobblemon:poke_ball`.
    pub caught_ball: String,
    /// The persistent status' id (`cobblemon:paralysis`), when it has one.
    pub status: Option<String>,
    /// What picks the model and the texture: the server sends them worked out (`ForcedAspects`).
    pub aspects: Vec<String>,
    /// The HP IV that counts (`IVs.getEffectiveBattleIV`: the hyper-trained one over the base one).
    pub health_iv: u32,
    pub health_ev: u32,
    pub scale_modifier: f32,
    /// The held item's id, when it holds one.
    pub held_item: Option<String>,
    /// Where it is. A Pokémon comes in its ball or on a shoulder (`ShoulderedState.CODEC`); the server
    /// says when it is sent out.
    pub state: State,
    /// Everything the server sent, for what is not picked out above.
    pub tag: Tag,
}

fn text(tag: &Tag, key: &str) -> Result<String> {
    match tag.get(key) {
        Some(Tag::String(value)) => Ok(value.to_string_lossy()),
        _ => Err(Error::new(format!("a Pokémon without the string {key}"))),
    }
}

fn int(tag: &Tag, key: &str) -> Result<u32> {
    match tag.get(key) {
        Some(&Tag::Int(value)) => u32::try_from(value)
            .map_err(|_| Error::new(format!("a Pokémon whose {key} is {value}"))),
        _ => Err(Error::new(format!("a Pokémon without the number {key}"))),
    }
}

fn flag(tag: &Tag, key: &str) -> bool {
    matches!(tag.get(key), Some(&Tag::Byte(value)) if value != 0)
}

/// `Gender.CODEC`: the constant's name.
fn gender(name: &str) -> Result<Gender> {
    Ok(match name {
        "MALE" => Gender::Male,
        "FEMALE" => Gender::Female,
        "GENDERLESS" => Gender::Genderless,
        other => return Err(Error::new(format!("a Pokémon of gender {other}"))),
    })
}

impl PartyPokemon {
    /// A Pokémon from the compound `Pokemon.CLIENT_CODEC` writes.
    pub fn from_tag(tag: Tag) -> Result<Self> {
        // `UUIDUtil.LENIENT_CODEC` writes four ints, the most significant first.
        let uuid = match tag.get("UUID") {
            Some(Tag::IntArray(parts)) if parts.len() == 4 => parts
                .iter()
                .fold(0u128, |uuid, &part| (uuid << 32) | u128::from(part as u32)),
            _ => return Err(Error::new("a Pokémon without a UUID")),
        };
        let health_stat = |of: Option<&Tag>| match of.and_then(|stats| stats.get("cobblemon:hp")) {
            Some(&Tag::Int(value)) => u32::try_from(value).ok(),
            _ => None,
        };
        let ivs = tag.get("IVs");
        let health_iv = health_stat(ivs.and_then(|ivs| ivs.get("HyperTrained")))
            .or_else(|| health_stat(ivs.and_then(|ivs| ivs.get("Base"))))
            .unwrap_or(0);
        let aspects = match tag.get("ForcedAspects") {
            Some(Tag::List(list)) => list
                .iter()
                .filter_map(|aspect| match aspect {
                    Tag::String(aspect) => Some(aspect.to_string_lossy()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            uuid,
            species: text(&tag, "Species")?,
            form: text(&tag, "FormId")?,
            nickname: tag.get("Nickname").cloned(),
            level: int(&tag, "Level")?,
            experience: int(&tag, "Experience")?,
            health: int(&tag, "Health")?,
            gender: gender(&text(&tag, "Gender")?)?,
            shiny: flag(&tag, "Shiny"),
            caught_ball: text(&tag, "CaughtBall")?,
            status: tag
                .get("Status")
                .and_then(|status| text(status, "StatusName").ok()),
            aspects,
            health_iv,
            health_ev: health_stat(tag.get("EVs")).unwrap_or(0),
            scale_modifier: match tag.get("ScaleModifier") {
                Some(&Tag::Float(bits)) => f32::from_bits(bits),
                _ => 1.0,
            },
            held_item: tag.get("HeldItem").and_then(|item| text(item, "id").ok()),
            state: match tag.get("State") {
                Some(state) => State::Shouldered {
                    left: flag(state, "StateShoulder"),
                },
                None => State::Inactive,
            },
            tag,
        })
    }

    /// `Pokemon.isFainted`.
    pub fn fainted(&self) -> bool {
        self.health == 0
    }

    /// The status' Showdown name, which names its icon (`status_par.png`).
    pub fn status_icon(&self) -> Option<&'static str> {
        Some(match self.status.as_deref()? {
            "cobblemon:burn" => "brn",
            "cobblemon:frozen" => "frz",
            "cobblemon:paralysis" => "par",
            "cobblemon:poison" => "psn",
            "cobblemon:poisonbadly" => "tox",
            "cobblemon:sleep" => "slp",
            _ => return None,
        })
    }
}

/// `ClientStorageManager`: the party stores the server named, which one is the player's, and the slot
/// the player has selected.
#[derive(Debug)]
pub struct Storage {
    stores: HashMap<u128, Vec<Option<PartyPokemon>>>,
    /// The store the overlay shows.
    mine: Option<u128>,
    /// `selectedSlot`, with -1 as none; and the Pokémon that was there, which the selection follows.
    selected_slot: i32,
    selected: Option<u128>,
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    pub fn new() -> Self {
        Self {
            stores: HashMap::new(),
            mine: None,
            selected_slot: -1,
            selected: None,
        }
    }

    /// The player's own party, slot by slot; empty until the server names it.
    pub fn party(&self) -> &[Option<PartyPokemon>] {
        self.mine
            .and_then(|id| self.stores.get(&id))
            .map_or(&[], Vec::as_slice)
    }

    /// The slot the player has selected, when the party has a Pokémon.
    pub fn selected_slot(&self) -> Option<usize> {
        usize::try_from(self.selected_slot).ok()
    }

    /// `shiftSelected`: the next Pokémon of the party, or the previous one, past the empty slots.
    pub fn shift_selected(&mut self, forward: bool) {
        let party = self.party();
        let count = party.len() as i32;
        let mut slot = self.selected_slot;
        // No Pokémon: slot 0, with none selected. Else at most one turn of the party and its two ends.
        let mut next = (0, None);
        if party.iter().any(Option::is_some) {
            for _ in 0..=count + 2 {
                slot += if forward { 1 } else { -1 };
                if slot >= count {
                    slot = -1;
                } else if slot < 0 {
                    slot = count;
                } else if let Some(pokemon) = &party[slot as usize] {
                    next = (slot, Some(pokemon.uuid));
                    break;
                }
            }
        }
        (self.selected_slot, self.selected) = next;
    }

    /// `checkSelectedPokemon`: the selection stays on its Pokémon wherever it went; once that one is
    /// gone it takes the Pokémon now in its slot, and the party's first when the slot is empty.
    fn check_selected(&mut self) {
        let party = self.party();
        let at = |uuid: u128| {
            party
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|p| p.uuid == uuid))
        };
        let here = usize::try_from(self.selected_slot)
            .ok()
            .and_then(|slot| party.get(slot)?.as_ref())
            .map(|p| p.uuid);
        let (slot, selected) = match self.selected.and_then(|uuid| Some((at(uuid)?, uuid))) {
            Some((slot, uuid)) => (slot as i32, Some(uuid)),
            None => match here {
                Some(uuid) => (self.selected_slot, Some(uuid)),
                None => match party.iter().flatten().next() {
                    Some(first) => (
                        at(first.uuid).map_or(-1, |slot| slot as i32),
                        Some(first.uuid),
                    ),
                    None => (-1, None),
                },
            },
        };
        (self.selected_slot, self.selected) = (slot, selected);
    }

    fn find(&mut self, store: u128, pokemon: u128) -> Option<&mut PartyPokemon> {
        self.stores
            .get_mut(&store)?
            .iter_mut()
            .flatten()
            .find(|p| p.uuid == pokemon)
    }

    fn position(&self, store: u128, pokemon: u128) -> Option<usize> {
        self.stores
            .get(&store)?
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|p| p.uuid == pokemon))
    }

    /// A message on one of the channels [`reads`] names. Whether it changed a party: one about a store
    /// that is no party (the PC's) or a Pokémon that is not there changes nothing, as in Cobblemon.
    pub fn read(&mut self, channel: &str, payload: &[u8]) -> Result<bool> {
        let r = &mut Reader::new(payload);
        let changed = match channel {
            INITIALIZE_PARTY => {
                let mine = r.bool()?;
                let store = r.uuid()?;
                let slots = usize::from(r.u8()?);
                self.stores.insert(store, vec![None; slots]);
                if mine {
                    self.mine = Some(store);
                }
                true
            }
            SET_PARTY_POKEMON => {
                let store = r.uuid()?;
                // `writePartyPosition`: the slot in two bytes.
                let slot = usize::from(r.u16()?);
                let pokemon = PartyPokemon::from_tag(r.nbt()?)?;
                match self.stores.get_mut(&store).and_then(|s| s.get_mut(slot)) {
                    Some(place) => {
                        *place = Some(pokemon);
                        true
                    }
                    None => false,
                }
            }
            SET_PARTY_REFERENCE => {
                let store = r.uuid()?;
                if !self.stores.contains_key(&store) {
                    return Err(Error::new("the party is a store the server never named"));
                }
                self.mine = Some(store);
                true
            }
            REMOVE_CLIENT_POKEMON => {
                let party = r.bool()?;
                let (store, pokemon) = (r.uuid()?, r.uuid()?);
                match (self.position(store, pokemon), self.stores.get_mut(&store)) {
                    (Some(slot), Some(slots)) if party => {
                        slots[slot] = None;
                        true
                    }
                    _ => false,
                }
            }
            SWAP_CLIENT_POKEMON => {
                let party = r.bool()?;
                let (store, first, second) = (r.uuid()?, r.uuid()?, r.uuid()?);
                let places = (self.position(store, first), self.position(store, second));
                match (places, self.stores.get_mut(&store)) {
                    ((Some(a), Some(b)), Some(slots)) if party => {
                        slots.swap(a, b);
                        true
                    }
                    _ => false,
                }
            }
            MOVE_CLIENT_PARTY_POKEMON => {
                let (store, pokemon) = (r.uuid()?, r.uuid()?);
                let to = usize::from(r.u16()?);
                match (self.position(store, pokemon), self.stores.get_mut(&store)) {
                    (Some(from), Some(slots)) => {
                        if to >= slots.len() {
                            return Err(Error::new(format!("a Pokémon moved to slot {to}")));
                        }
                        let moved = slots[from].take();
                        slots[to] = moved;
                        true
                    }
                    _ => false,
                }
            }
            _ => {
                // `PokemonUpdatePacket`: the store, the Pokémon, then the field's value.
                let (store, pokemon) = (r.uuid()?, r.uuid()?);
                let Some(p) = self.find(store, pokemon) else {
                    return Ok(false);
                };
                match channel {
                    HEALTH_UPDATE => p.health = u32::from(r.u16()?),
                    // `setExperienceAndUpdateLevel` needs the species' experience group, which the
                    // caller has: see [`PartyPokemon::level_from`].
                    EXPERIENCE_UPDATE => {
                        p.experience = u32::try_from(r.i32()?)
                            .map_err(|_| Error::new("a Pokémon with experience below 0"))?;
                    }
                    STATUS_UPDATE => p.status = nullable(r, string)?,
                    NICKNAME_UPDATE => p.nickname = nullable(r, Reader::nbt)?,
                    SPECIES_UPDATE => p.species = string(r)?,
                    ASPECTS_UPDATE => {
                        let count = r.count(4096)?;
                        p.aspects = (0..count).map(|_| string(r)).collect::<Result<_>>()?;
                    }
                    CAUGHT_BALL_UPDATE => p.caught_ball = string(r)?,
                    GENDER_UPDATE => {
                        p.gender = match r.i32()? {
                            0 => Gender::Male,
                            1 => Gender::Female,
                            2 => Gender::Genderless,
                            other => return Err(Error::new(format!("gender ordinal {other}"))),
                        };
                    }
                    SHINY_UPDATE => p.shiny = r.bool()?,
                    STATE_UPDATE => p.state = State::read(r)?,
                    other => return Err(Error::new(format!("{other} is not read here"))),
                }
                true
            }
        };
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{channel}: {} bytes left unread",
                r.remaining()
            )));
        }
        if changed {
            self.check_selected();
        }
        Ok(changed)
    }

    /// The party's Pokémon, to bring their level in line with their experience after
    /// [`EXPERIENCE_UPDATE`] (`setExperienceAndUpdateLevel`).
    pub fn party_mut(&mut self) -> impl Iterator<Item = &mut PartyPokemon> {
        self.mine
            .and_then(|id| self.stores.get_mut(&id))
            .into_iter()
            .flatten()
            .flatten()
    }
}

impl PartyPokemon {
    /// `setExperienceAndUpdateLevel`: the level its experience has reached in `group`, no higher than
    /// the server's `max_level`.
    pub fn level_from(&mut self, group: ExperienceGroup, max_level: u32) {
        let level = group.level(self.experience);
        if level != self.level && level <= max_level {
            self.level = level;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INITIALIZE: &[u8] = include_bytes!("../tests/fixtures/party_initialize.bin");
    const REFERENCE: &[u8] = include_bytes!("../tests/fixtures/party_reference.bin");
    const POKEMON: [&[u8]; 3] = [
        include_bytes!("../tests/fixtures/party_pokemon_1.bin"),
        include_bytes!("../tests/fixtures/party_pokemon_2.bin"),
        include_bytes!("../tests/fixtures/party_pokemon_3.bin"),
    ];

    /// What a server sent a player who logs in with three Pokémon (captured from Cobblemon 1.8.1).
    fn logged_in() -> Storage {
        let mut storage = Storage::new();
        assert!(storage.read(INITIALIZE_PARTY, INITIALIZE).unwrap());
        for pokemon in POKEMON {
            assert!(storage.read(SET_PARTY_POKEMON, pokemon).unwrap());
        }
        assert!(storage.read(SET_PARTY_REFERENCE, REFERENCE).unwrap());
        storage
    }

    #[test]
    fn a_login_s_party_is_read_whole() {
        let storage = logged_in();
        let party = storage.party();
        assert_eq!(party.len(), 6);
        let seen: Vec<_> = party
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|p| (p.species.as_str(), p.level, p.health))
            })
            .collect();
        assert_eq!(
            seen,
            [
                Some(("cobblemon:pikachu", 13, 33)),
                Some(("cobblemon:charizard", 50, 140)),
                Some(("cobblemon:bulbasaur", 5, 20)),
                None,
                None,
                None,
            ]
        );
        let charizard = party[1].as_ref().unwrap();
        assert_eq!(charizard.uuid, 0x0aa2_3eda_48cb_433f_81d7_7823_a286_0ae6);
        assert_eq!(charizard.form, "normal");
        assert_eq!(charizard.gender, Gender::Male);
        assert!(charizard.shiny);
        assert_eq!(charizard.aspects, ["shiny", "male"]);
        assert_eq!(charizard.caught_ball, "cobblemon:poke_ball");
        assert_eq!((charizard.health_iv, charizard.health_ev), (4, 0));
        assert_eq!(charizard.experience, 117_360);
        assert!(charizard.status.is_none() && charizard.held_item.is_none());
        assert!(charizard.nickname.is_none() && !charizard.fainted());
        // The nickname is a component, here the one `pokeedit nickname=Bulbi` makes.
        let nickname = party[2].as_ref().unwrap().nickname.as_ref().unwrap();
        assert!(
            matches!(nickname.get("translate"), Some(Tag::String(s)) if s.to_string_lossy() == "Bulbi")
        );
        // The first Pokémon is the one selected.
        assert_eq!(storage.selected_slot(), Some(0));
    }

    #[test]
    fn full_health_is_what_the_species_and_the_pokemon_s_numbers_give() {
        let storage = logged_in();
        // Base HP of Pikachu, Charizard and Bulbasaur; all three were just healed.
        for (slot, base) in [(0, 35), (1, 78), (2, 45)] {
            let p = storage.party()[slot].as_ref().unwrap();
            assert_eq!(
                max_health(&p.species, base, p.health_iv, p.health_ev, p.level),
                p.health,
                "{}",
                p.species
            );
        }
        assert_eq!(max_health("cobblemon:shedinja", 1, 31, 252, 100), 1);
    }

    #[test]
    fn a_level_is_where_its_experience_is_in_the_species_group() {
        use ExperienceGroup::*;
        // Charizard is medium slow, Pikachu medium fast: the experience the server gave them at the
        // level it gave them.
        assert_eq!(MediumSlow.experience(50), 117_360);
        assert_eq!(MediumFast.experience(13), 2197);
        assert_eq!(MediumSlow.level(117_360), 50);
        assert_eq!(MediumSlow.level(117_359), 49);
        assert_eq!(MediumFast.level(2197), 13);
        // Bulbasaur at level 5, medium slow: 135.
        assert_eq!(MediumSlow.experience(5), 135);
        assert_eq!(MediumSlow.experience(1), 0);
        // The published tables' totals at level 100.
        let at_100 =
            [Erratic, Fast, MediumFast, MediumSlow, Slow, Fluctuating].map(|g| g.experience(100));
        assert_eq!(
            at_100,
            [600_000, 800_000, 1_000_000, 1_059_860, 1_250_000, 1_640_000]
        );
        for group in [Erratic, Fast, MediumFast, MediumSlow, Slow, Fluctuating] {
            assert_eq!(group.experience(1), 0);
            assert_eq!(group.level(0), 1);
            assert_eq!(group.level(group.experience(37)), 37);
        }
        assert_eq!(ExperienceGroup::named("Medium_Slow"), Some(MediumSlow));
        assert_eq!(ExperienceGroup::named("none"), None);
    }

    #[test]
    fn a_field_s_update_changes_that_pokemon() {
        // Captured as the first Pokémon was edited to level 13 and the third nicknamed, then all healed.
        let mut storage = logged_in();
        let pikachu = storage.party()[0].as_ref().unwrap().uuid;
        for p in storage.party_mut() {
            (p.health, p.experience, p.level, p.nickname) = (1, 0, 12, None);
            p.status = Some("cobblemon:paralysis".into());
        }
        assert_eq!(
            storage.party()[0].as_ref().unwrap().status_icon(),
            Some("par")
        );
        let update =
            |storage: &mut Storage, channel, bytes: &[u8]| storage.read(channel, bytes).unwrap();
        assert!(update(
            &mut storage,
            HEALTH_UPDATE,
            include_bytes!("../tests/fixtures/party_health_update.bin")
        ));
        assert!(update(
            &mut storage,
            EXPERIENCE_UPDATE,
            include_bytes!("../tests/fixtures/party_experience_update.bin")
        ));
        assert!(update(
            &mut storage,
            STATUS_UPDATE,
            include_bytes!("../tests/fixtures/party_status_update.bin")
        ));
        assert!(update(
            &mut storage,
            NICKNAME_UPDATE,
            include_bytes!("../tests/fixtures/party_nickname_update.bin")
        ));
        for p in storage.party_mut() {
            p.level_from(ExperienceGroup::MediumFast, 100);
        }
        let party = storage.party();
        let first = party[0].as_ref().unwrap();
        assert_eq!(first.uuid, pikachu);
        assert_eq!(
            (first.health, first.experience, first.level),
            (33, 2197, 13)
        );
        assert!(first.status.is_none());
        // The others kept what they had, but for the nickname, which was the third's.
        assert_eq!(party[1].as_ref().unwrap().health, 1);
        assert!(party[1].as_ref().unwrap().nickname.is_none());
        assert!(party[2].as_ref().unwrap().nickname.is_some());
    }

    #[test]
    fn pokemon_move_swap_and_leave_and_the_selection_follows_its_own() {
        let mut storage = logged_in();
        let ids: Vec<u128> = storage.party().iter().flatten().map(|p| p.uuid).collect();
        let store = Reader::new(REFERENCE).uuid().unwrap();
        let message = |flag: Option<bool>, uuids: &[u128], slot: Option<u16>| {
            let mut bytes = Vec::new();
            if let Some(flag) = flag {
                bytes.push(u8::from(flag));
            }
            for uuid in [store].iter().chain(uuids) {
                bytes.extend_from_slice(&uuid.to_be_bytes());
            }
            if let Some(slot) = slot {
                bytes.extend_from_slice(&slot.to_be_bytes());
            }
            bytes
        };
        let order = |storage: &Storage| -> Vec<Option<usize>> {
            storage
                .party()
                .iter()
                .map(|slot| {
                    slot.as_ref()
                        .and_then(|p| ids.iter().position(|&id| id == p.uuid))
                })
                .collect()
        };
        // The selection is on the first Pokémon, and stays on it as it changes places.
        assert!(
            storage
                .read(
                    SWAP_CLIENT_POKEMON,
                    &message(Some(true), &[ids[0], ids[2]], None)
                )
                .unwrap()
        );
        assert_eq!(
            order(&storage),
            [Some(2), Some(1), Some(0), None, None, None]
        );
        assert_eq!(storage.selected_slot(), Some(2));
        assert!(
            storage
                .read(
                    MOVE_CLIENT_PARTY_POKEMON,
                    &message(None, &[ids[0]], Some(5))
                )
                .unwrap()
        );
        assert_eq!(
            order(&storage),
            [Some(2), Some(1), None, None, None, Some(0)]
        );
        assert_eq!(storage.selected_slot(), Some(5));
        // Next past the end is the first one; back from it, the last.
        storage.shift_selected(true);
        assert_eq!(storage.selected_slot(), Some(0));
        storage.shift_selected(false);
        assert_eq!(storage.selected_slot(), Some(5));
        // Once it leaves, the selection falls on the party's first.
        assert!(
            storage
                .read(REMOVE_CLIENT_POKEMON, &message(Some(true), &[ids[0]], None))
                .unwrap()
        );
        assert_eq!(order(&storage), [Some(2), Some(1), None, None, None, None]);
        assert_eq!(storage.selected_slot(), Some(0));
        // The same message about the PC's store, or a Pokémon that is not there, changes nothing.
        assert!(
            !storage
                .read(
                    REMOVE_CLIENT_POKEMON,
                    &message(Some(false), &[ids[1]], None)
                )
                .unwrap()
        );
        assert!(
            !storage
                .read(REMOVE_CLIENT_POKEMON, &message(Some(true), &[ids[0]], None))
                .unwrap()
        );
        assert_eq!(order(&storage), [Some(2), Some(1), None, None, None, None]);
    }

    #[test]
    fn a_message_cut_short_or_too_long_is_an_error() {
        let mut storage = logged_in();
        assert!(storage.read(SET_PARTY_POKEMON, &POKEMON[0][..200]).is_err());
        let mut long = include_bytes!("../tests/fixtures/party_health_update.bin").to_vec();
        long.push(0);
        assert!(storage.read(HEALTH_UPDATE, &long).is_err());
        assert!(Storage::new().read(SET_PARTY_REFERENCE, REFERENCE).is_err());
    }
}
