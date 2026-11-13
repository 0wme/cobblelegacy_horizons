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
//! The other way, the client asks: [`send_out`], what the key that throws the selected Pokémon sends
//! (the server then sends the Pokémon out or calls it back, whichever it is not, and says so with the
//! Pokémon's new [`State`]); and what the summary screen does to a Pokémon, each a message the server
//! answers with the update of what changed: two moves changing places ([`request_move_swap`]), a move
//! given up for one learnt before ([`bench_move`]), a nickname ([`set_nickname`]), the six markings
//! ([`set_markings`]), the mark worn ([`set_active_mark`]), the held item shown or hidden
//! ([`set_item_hidden`]), and two slots of the party changing places ([`swap_party_pokemon`],
//! [`move_party_pokemon`]).
//!
//! Not read yet: the PC's stores (`cobblemon:initialize_pc`, `set_pc_box`...), a Pokémon's evolutions,
//! species features and ride boosts (kept as sent in [`PartyPokemon::tag`]), and the updates of a held
//! or cosmetic item (`cobblemon:held_item_update`: an item stack in the server's own numbering), of a
//! form, of the marks earned and of the fields no screen shows.
use crate::stats::{self, Ivs, Stat};
use crate::wire::{Gender, nullable, string};
use horizon_protocol::{
    Error, Result,
    buf::{Reader, Writer},
};
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
/// `MoveSetUpdatePacket.ID`.
pub const MOVESET_UPDATE: &str = "cobblemon:moveset_update";
/// `BenchedMovesUpdatePacket.ID`.
pub const BENCHED_MOVES_UPDATE: &str = "cobblemon:benched_moves_update";
/// `NatureUpdatePacket.ID`.
pub const NATURE_UPDATE: &str = "cobblemon:nature_update";
/// `AbilityUpdatePacket.ID`.
pub const ABILITY_UPDATE: &str = "cobblemon:ability_update";
/// `IVsUpdatePacket.ID`.
pub const IV_UPDATE: &str = "cobblemon:iv_update";
/// `EVsUpdatePacket.ID`.
pub const EV_UPDATE: &str = "cobblemon:ev_update";
/// `FriendshipUpdatePacket.ID`.
pub const FRIENDSHIP_UPDATE: &str = "cobblemon:friendship_update";
/// `FullnessUpdatePacket.ID`.
pub const FULLNESS_UPDATE: &str = "cobblemon:fullness_update";
/// `MarkingsUpdatePacket.ID`.
pub const MARKINGS_UPDATE: &str = "cobblemon:markings_update";
/// `ActiveMarkUpdatePacket.ID`.
pub const ACTIVE_MARK_UPDATE: &str = "cobblemon:active_mark_update";
/// `TeraTypeUpdatePacket.ID`.
pub const TERA_TYPE_UPDATE: &str = "cobblemon:tera_type_update";
/// `OriginalTrainerUpdatePacket.ID`.
pub const ORIGINAL_TRAINER_UPDATE: &str = "cobblemon:original_trainer_update";

/// `SendOutPokemonPacket.ID`, which the client sends.
pub const SEND_OUT_POKEMON: &str = "cobblemon:send_out_pokemon";

/// `SendOutPokemonPacket`: the payload that asks the server to send out the Pokémon in `slot` of the
/// player's party, or to call it back when it is out.
pub fn send_out(slot: u8) -> Vec<u8> {
    vec![slot]
}

/// `RequestMoveSwapPacket.ID`, which the client sends.
pub const REQUEST_MOVE_SWAP: &str = "cobblemon:request_move_swap";
/// `BenchMovePacket.ID`, which the client sends.
pub const BENCH_MOVE: &str = "cobblemon:bench_move";
/// `SetNicknamePacket.ID`, which the client sends.
pub const SET_NICKNAME: &str = "cobblemon:set_nickname";
/// `SetMarkingsPacket.ID`, which the client sends.
pub const SET_MARKINGS: &str = "cobblemon:set_markings";
/// `SetActiveMarkPacket.ID`, which the client sends.
pub const SET_ACTIVE_MARK: &str = "cobblemon:set_active_mark";
/// `SetItemHiddenPacket.ID`, which the client sends.
pub const SET_ITEM_HIDDEN: &str = "cobblemon:set_item_hidden";
/// `SwapPartyPokemonPacket.ID`, which the client sends.
pub const SWAP_PARTY_POKEMON: &str = "cobblemon:swap_party_pokemon";
/// `MovePartyPokemonPacket.ID`, which the client sends.
pub const MOVE_PARTY_POKEMON: &str = "cobblemon:move_party_pokemon";

/// `SetNicknameHandler.MAX_NAME_LENGTH`: the server refuses a longer nickname.
pub const MAX_NICKNAME: usize = 12;

/// `RequestMoveSwapPacket`: asks that the moves at `first` and `second` of the Pokémon in `slot` of
/// the party change places. The server answers with the move set ([`MOVESET_UPDATE`]).
pub fn request_move_swap(first: usize, second: usize, slot: usize) -> Vec<u8> {
    let mut w = Writer::new();
    for value in [first, second, slot] {
        w.i32(value as i32);
    }
    w.data
}

/// `BenchMovePacket`: asks that a Pokémon of the party (`party`, else of the PC) gives up the move
/// `old` for `new`, one it may take back. No `old` fills an empty place; no `new` forgets the move.
pub fn bench_move(party: bool, pokemon: u128, old: Option<&str>, new: Option<&str>) -> Vec<u8> {
    let mut w = Writer::new();
    w.bool(party);
    w.uuid(pokemon);
    for name in [old, new] {
        w.bool(name.is_some());
        if let Some(name) = name {
            w.string(name);
        }
    }
    w.data
}

/// `SetNicknamePacket`: names a Pokémon, or with no nickname gives it back its species' name.
pub fn set_nickname(pokemon: u128, party: bool, nickname: Option<&str>) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(pokemon);
    w.bool(party);
    w.bool(nickname.is_some());
    if let Some(nickname) = nickname {
        w.string(nickname);
    }
    w.data
}

/// `SetMarkingsPacket`: the state of each of a Pokémon's six markings (0 off, 1 and 2 its two
/// colours).
pub fn set_markings(pokemon: u128, markings: &[i32], party: bool) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(pokemon);
    w.varint(markings.len() as i32);
    for &state in markings {
        w.i32(state);
    }
    w.bool(party);
    w.data
}

/// `SetActiveMarkPacket`: the mark a Pokémon wears by its identifier, or none.
pub fn set_active_mark(pokemon: u128, mark: Option<&str>) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(pokemon);
    w.bool(mark.is_some());
    if let Some(mark) = mark {
        w.string(mark);
    }
    w.data
}

/// `SetItemHiddenPacket`: whether a Pokémon's held item shows on its model.
pub fn set_item_hidden(pokemon: u128, visible: bool) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(pokemon);
    w.bool(visible);
    w.data
}

/// `SwapPartyPokemonPacket`: two Pokémon of the party change places, each named with the slot it is
/// in (`writePartyPosition`: two bytes).
pub fn swap_party_pokemon(first: (u128, usize), second: (u128, usize)) -> Vec<u8> {
    let mut w = Writer::new();
    for (pokemon, slot) in [first, second] {
        w.uuid(pokemon);
        w.u16(slot as u16);
    }
    w.data
}

/// `MovePartyPokemonPacket`: a Pokémon of the party goes from slot `from` to the empty slot `to`.
pub fn move_party_pokemon(pokemon: u128, from: usize, to: usize) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(pokemon);
    w.u16(from as u16);
    w.u16(to as u16);
    w.data
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
            | MOVESET_UPDATE
            | BENCHED_MOVES_UPDATE
            | NATURE_UPDATE
            | ABILITY_UPDATE
            | IV_UPDATE
            | EV_UPDATE
            | FRIENDSHIP_UPDATE
            | FULLNESS_UPDATE
            | MARKINGS_UPDATE
            | ACTIVE_MARK_UPDATE
            | TERA_TYPE_UPDATE
            | ORIGINAL_TRAINER_UPDATE
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

/// A persistent status' Showdown name (`par`), which names its icons and its label, from its id
/// (`cobblemon:paralysis`).
pub fn status_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "cobblemon:burn" => "brn",
        "cobblemon:frozen" => "frz",
        "cobblemon:paralysis" => "par",
        "cobblemon:poison" => "psn",
        "cobblemon:poisonbadly" => "tox",
        "cobblemon:sleep" => "slp",
        _ => return None,
    })
}

/// `Move`: one of the moves a Pokémon knows now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// The move's Showdown id (`firespin`), which the server's registry knows it by
    /// ([`crate::moves::Moves`]).
    pub id: String,
    /// `currentPp`.
    pub pp: u32,
    /// How many PP Up it took, 0 to 3.
    pub raised_pp_stages: u32,
}

impl Move {
    /// `Move.maxPp`: the template's PP, and a fifth more for each PP Up.
    pub fn max_pp(&self, template_pp: u32) -> u32 {
        max_pp(template_pp, self.raised_pp_stages)
    }
}

/// `template.pp + raisedPpStages * template.pp / 5`.
pub fn max_pp(template_pp: u32, raised_pp_stages: u32) -> u32 {
    template_pp + raised_pp_stages * template_pp / 5
}

/// `BenchedMove`: a move a Pokémon knew and gave up, which it may take back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchedMove {
    pub id: String,
    pub raised_pp_stages: u32,
}

/// A Pokémon as `Pokemon.CLIENT_CODEC` writes it: what the party and the summary show of it, and the
/// compound itself.
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
    /// 0 to the server's `maxPokemonFriendship` (255).
    pub friendship: u32,
    /// `currentFullness`: how much it ate, 0 to what its species' weight allows.
    pub fullness: u32,
    pub ivs: Ivs,
    /// By `Stat as usize`.
    pub evs: stats::Values,
    /// The nature's id (`cobblemon:bold`).
    pub nature: String,
    /// The nature a mint gave it, which is the one that counts when it has one.
    pub minted_nature: Option<String>,
    /// The ability's name (`blaze`).
    pub ability: String,
    /// The moves it knows, at most four, in the order of its move set.
    pub moves: Vec<Move>,
    pub benched_moves: Vec<BenchedMove>,
    /// The Tera type's id (`cobblemon:flying`).
    pub tera_type: String,
    /// Who first had it: a player's UUID as text, or an NPC's name.
    pub original_trainer: Option<String>,
    /// That player's name, when the server knows it.
    pub original_trainer_name: Option<String>,
    /// Whether the held item shows on the model.
    pub held_item_visible: bool,
    /// The cosmetic item's id, when it wears one.
    pub cosmetic_item: Option<String>,
    /// The six markings, each 0 (off), 1 or 2 (its two colours).
    pub markings: Vec<i32>,
    /// The marks it earned, by identifier, and the one it wears.
    pub marks: Vec<String>,
    pub active_mark: Option<String>,
    pub alpha: bool,
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

/// The strings of the list at `key`; none without it.
fn texts(tag: &Tag, key: &str) -> Vec<String> {
    match tag.get(key) {
        Some(Tag::List(list)) => list
            .iter()
            .filter_map(|entry| match entry {
                Tag::String(entry) => Some(entry.to_string_lossy()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A number a codec wrote as an int, or as a byte (`BenchedMove.saveToNBT`); 0 without it.
fn small(tag: &Tag, key: &str) -> u32 {
    match tag.get(key) {
        Some(&Tag::Int(value)) => u32::try_from(value).unwrap_or(0),
        Some(&Tag::Byte(value)) => u32::try_from(value).unwrap_or(0),
        _ => 0,
    }
}

/// The compounds of the list at `key`, each with the string it has at `name`.
fn named<'a>(tag: &'a Tag, key: &str, name: &'a str) -> impl Iterator<Item = (String, &'a Tag)> {
    let list = match tag.get(key) {
        Some(Tag::List(list)) => list.as_slice(),
        _ => &[],
    };
    list.iter()
        .filter_map(move |entry| Some((text(entry, name).ok()?, entry)))
}

/// `Pokemon.markings` before any is set: six, all off.
const NO_MARKINGS: [i32; 6] = [0; 6];

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
        let ivs = Ivs::from_tag(tag.get("IVs"));
        let evs = stats::evs(tag.get("EVs"));
        let aspects = texts(&tag, "ForcedAspects");
        // `MoveSet.CODEC` and `BenchedMoves.CODEC`: a list each, the moves that are there.
        let moves = named(&tag, "MoveSet", "MoveName")
            .map(|(id, entry)| Move {
                id,
                pp: small(entry, "MovePP"),
                raised_pp_stages: small(entry, "RaisedPPStages"),
            })
            .collect();
        let benched_moves = named(&tag, "BenchedMoves", "MoveName")
            .map(|(id, entry)| BenchedMove {
                id,
                raised_pp_stages: small(entry, "RaisedPPStages"),
            })
            .collect();
        let markings = match tag.get("Markings") {
            Some(Tag::List(list)) => list
                .iter()
                .map(|state| match state {
                    &Tag::Int(state) => state,
                    _ => 0,
                })
                .collect(),
            _ => NO_MARKINGS.to_vec(),
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
            health_iv: ivs.effective(Stat::Hp),
            health_ev: evs[Stat::Hp as usize],
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
            friendship: small(&tag, "Friendship"),
            fullness: small(&tag, "Fullness"),
            ivs,
            evs,
            // Cobblemon's codec asks for a nature, an ability and a Tera type. A compound without
            // them still makes a Pokémon here, with nothing to show of them: the party and the
            // battle go by what is above.
            nature: text(&tag, "Nature").unwrap_or_default(),
            minted_nature: text(&tag, "MintedNature").ok(),
            ability: tag
                .get("Ability")
                .and_then(|ability| text(ability, "AbilityName").ok())
                .unwrap_or_default(),
            moves,
            benched_moves,
            tera_type: text(&tag, "TeraType").unwrap_or_default(),
            original_trainer: text(&tag, "PokemonOriginalTrainer").ok(),
            original_trainer_name: text(&tag, "PokemonOriginalTrainerName").ok(),
            // `Pokemon.heldItemVisible` is true until the server says otherwise.
            held_item_visible: !matches!(tag.get("HeldItemVisible"), Some(&Tag::Byte(0))),
            cosmetic_item: tag
                .get("CosmeticItem")
                .and_then(|item| text(item, "id").ok()),
            markings,
            marks: texts(&tag, "Marks"),
            active_mark: text(&tag, "ActiveMark").ok(),
            alpha: flag(&tag, "Alpha"),
            tag,
        })
    }

    /// `Pokemon.effectiveNature`: the minted nature over its own.
    pub fn effective_nature(&self) -> Option<&'static stats::Nature> {
        stats::nature(self.minted_nature.as_deref().unwrap_or(&self.nature))
    }

    /// `Pokemon.getStat`: what `stat` is with the form's `base` stats, at the level it has.
    pub fn stat(&self, stat: Stat, base: &stats::Values) -> u32 {
        stats::stat(
            stat,
            &self.species,
            base[stat as usize],
            self.ivs.effective(stat),
            self.evs[stat as usize],
            self.level,
            self.effective_nature(),
        )
    }

    /// `Pokemon.isFainted`.
    pub fn fainted(&self) -> bool {
        self.health == 0
    }

    /// The status' Showdown name, which names its icon (`status_par.png`).
    pub fn status_icon(&self) -> Option<&'static str> {
        status_name(self.status.as_deref()?)
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
                    // `MoveSet.saveToBuffer`: how many, then each move's name, PP and PP Ups.
                    MOVESET_UPDATE => {
                        let count = usize::from(r.u8()?);
                        p.moves = (0..count)
                            .map(|_| {
                                Ok(Move {
                                    id: string(r)?,
                                    pp: u32::from(r.u8()?),
                                    raised_pp_stages: u32::from(r.u8()?),
                                })
                            })
                            .collect::<Result<_>>()?;
                    }
                    // `BenchedMoves.saveToBuffer`: the count in two bytes.
                    BENCHED_MOVES_UPDATE => {
                        let count = usize::try_from(r.i16()?)
                            .map_err(|_| Error::new("benched moves below none"))?;
                        p.benched_moves = (0..count)
                            .map(|_| {
                                Ok(BenchedMove {
                                    id: string(r)?,
                                    raised_pp_stages: u32::from(r.u8()?),
                                })
                            })
                            .collect::<Result<_>>()?;
                    }
                    // `NatureUpdatePacket.applyToPokemon`: the nature, or the mint's; no nature takes
                    // a mint away, and is nothing for the Pokémon's own.
                    NATURE_UPDATE => {
                        let nature = nullable(r, string)?;
                        match (r.bool()?, nature) {
                            (true, minted) => p.minted_nature = minted,
                            (false, Some(nature)) => p.nature = nature,
                            (false, None) => {}
                        }
                    }
                    ABILITY_UPDATE => p.ability = string(r)?,
                    // `IVs.STREAM_CODEC` and `EVs.STREAM_CODEC`: the codec's compound.
                    IV_UPDATE => {
                        p.ivs = Ivs::from_tag(Some(&r.nbt()?));
                        p.health_iv = p.ivs.effective(Stat::Hp);
                    }
                    EV_UPDATE => {
                        p.evs = stats::evs(Some(&r.nbt()?));
                        p.health_ev = p.evs[Stat::Hp as usize];
                    }
                    FRIENDSHIP_UPDATE => p.friendship = u32::from(r.u8()?),
                    FULLNESS_UPDATE => p.fullness = u32::from(r.u8()?),
                    MARKINGS_UPDATE => {
                        let count = r.count(64)?;
                        p.markings = (0..count).map(|_| r.i32()).collect::<Result<_>>()?;
                    }
                    ACTIVE_MARK_UPDATE => p.active_mark = nullable(r, string)?,
                    TERA_TYPE_UPDATE => p.tera_type = string(r)?,
                    ORIGINAL_TRAINER_UPDATE => p.original_trainer_name = nullable(r, string)?,
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

    #[test]
    fn what_the_summary_shows_is_read_from_a_login_s_party() {
        let storage = logged_in();
        let party = storage.party();
        let charizard = party[1].as_ref().unwrap();
        assert_eq!(charizard.nature, "cobblemon:bold");
        assert_eq!(charizard.minted_nature, None);
        assert_eq!(charizard.ability, "blaze");
        let moves: Vec<_> = charizard
            .moves
            .iter()
            .map(|m| (m.id.as_str(), m.pp, m.raised_pp_stages))
            .collect();
        assert_eq!(
            moves,
            [
                ("firespin", 15, 0),
                ("scaryface", 10, 0),
                ("flamethrower", 15, 0),
                ("slash", 20, 0)
            ]
        );
        assert!(charizard.benched_moves.is_empty());
        // HP, attack, defence, special attack, special defence, speed.
        assert_eq!(charizard.ivs.base, [4, 6, 14, 26, 14, 8]);
        assert_eq!(charizard.ivs.hyper_trained, [None; 6]);
        assert_eq!(charizard.evs, [0; 6]);
        assert_eq!((charizard.friendship, charizard.fullness), (50, 0));
        assert_eq!(charizard.tera_type, "cobblemon:flying");
        assert_eq!(
            charizard.original_trainer.as_deref(),
            Some("c63dc98f-3301-33bd-8ec0-c1a8d3c6f512")
        );
        assert_eq!(
            charizard.original_trainer_name.as_deref(),
            Some("HorizonProbe")
        );
        assert!(charizard.held_item_visible && charizard.cosmetic_item.is_none());
        assert_eq!(charizard.markings, [0; 6]);
        assert!(charizard.marks.is_empty() && charizard.active_mark.is_none());
        assert!(!charizard.alpha);
        // Its stats from its species' base stats: bold raises defence and lowers attack. Its health
        // is the one the server gave it, just healed.
        let stats = Stat::ALL.map(|stat| charizard.stat(stat, &[78, 84, 78, 109, 85, 100]));
        assert_eq!(stats, [140, 82, 99, 127, 97, 109]);
        assert_eq!(stats[0], charizard.health);
        // Pikachu at level 13, brave: attack up, speed down.
        let pikachu = party[0].as_ref().unwrap();
        assert_eq!(
            (pikachu.nature.as_str(), pikachu.ability.as_str()),
            ("cobblemon:brave", "static")
        );
        let stats = Stat::ALL.map(|stat| pikachu.stat(stat, &[35, 55, 40, 50, 50, 90]));
        assert_eq!(stats, [33, 22, 17, 21, 19, 27]);
        assert_eq!(stats[0], pikachu.health);
        // Bulbasaur knows three moves: its move set has no fourth.
        let bulbasaur = party[2].as_ref().unwrap();
        let moves: Vec<_> = bulbasaur.moves.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(moves, ["vinewhip", "growl", "tackle"]);
        assert_eq!(bulbasaur.tera_type, "cobblemon:grass");
    }

    #[test]
    fn what_a_battle_changed_of_a_pokemon_reaches_it() {
        // Captured after a battle the first Pokémon won with its first move, and during one the
        // second fought: the EVs the win gave, the moves with the PP they have left, the friendship
        // a faint cost.
        let mut storage = logged_in();
        let update =
            |storage: &mut Storage, channel, bytes: &[u8]| storage.read(channel, bytes).unwrap();
        assert!(update(
            &mut storage,
            EV_UPDATE,
            include_bytes!("../tests/fixtures/battle1/flow-025-ev_update.bin")
        ));
        assert!(update(
            &mut storage,
            MOVESET_UPDATE,
            include_bytes!("../tests/fixtures/battle1/flow-029-moveset_update.bin")
        ));
        assert!(update(
            &mut storage,
            FRIENDSHIP_UPDATE,
            include_bytes!("../tests/fixtures/battle2/flow-023-friendship_update.bin")
        ));
        assert!(update(
            &mut storage,
            MOVESET_UPDATE,
            include_bytes!("../tests/fixtures/battle2/flow-182-moveset_update.bin")
        ));
        let party = storage.party();
        let pikachu = party[0].as_ref().unwrap();
        assert_eq!(pikachu.evs, [2, 0, 0, 0, 0, 0]);
        assert_eq!(pikachu.health_ev, 2);
        assert_eq!(pikachu.friendship, 47);
        let left: Vec<_> = pikachu
            .moves
            .iter()
            .map(|m| (m.id.as_str(), m.pp))
            .collect();
        assert_eq!(
            left,
            [
                ("electroball", 9),
                ("doubleteam", 15),
                ("thunderwave", 20),
                ("tailwhip", 30)
            ]
        );
        let charizard = party[1].as_ref().unwrap();
        assert_eq!(
            (charizard.moves[0].id.as_str(), charizard.moves[0].pp),
            ("firespin", 11)
        );
        assert_eq!(charizard.moves.len(), 4);
        // The third was in neither battle.
        assert_eq!(party[2].as_ref().unwrap().friendship, 50);
    }

    /// A `PokemonUpdatePacket` about the logged-in party's second Pokémon, with `value` after the two ids.
    fn about_charizard(value: &[u8]) -> Vec<u8> {
        let mut bytes = REFERENCE.to_vec();
        bytes.extend_from_slice(&0x0aa2_3eda_48cb_433f_81d7_7823_a286_0ae6u128.to_be_bytes());
        bytes.extend_from_slice(value);
        bytes
    }

    #[test]
    fn the_other_fields_of_the_summary_follow_their_updates() {
        let mut storage = logged_in();
        let mut update = |channel, value: &[u8]| {
            assert!(
                storage.read(channel, &about_charizard(value)).unwrap(),
                "{channel}"
            );
        };
        // `MoveSet.saveToBuffer` after the first two moves changed places and one took a PP Up.
        let mut moves = vec![2];
        for (name, pp, raised) in [("scaryface", 12u8, 1u8), ("firespin", 15, 0)] {
            moves.push(name.len() as u8);
            moves.extend_from_slice(name.as_bytes());
            moves.extend_from_slice(&[pp, raised]);
        }
        update(MOVESET_UPDATE, &moves);
        // `BenchedMoves.saveToBuffer`: a count in two bytes, then a name and its PP Ups.
        let mut benched = vec![0, 1, 5];
        benched.extend_from_slice(b"slash");
        benched.push(2);
        update(BENCHED_MOVES_UPDATE, &benched);
        // A nature of its own, then a mint's.
        let nature = |id: &str, minted: bool| {
            let mut bytes = vec![1, id.len() as u8];
            bytes.extend_from_slice(id.as_bytes());
            bytes.push(u8::from(minted));
            bytes
        };
        update(NATURE_UPDATE, &nature("cobblemon:timid", false));
        update(NATURE_UPDATE, &nature("cobblemon:modest", true));
        update(ABILITY_UPDATE, b"\x0asolarpower");
        update(FULLNESS_UPDATE, &[3]);
        update(FRIENDSHIP_UPDATE, &[255]);
        update(
            MARKINGS_UPDATE,
            &[
                6, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        );
        update(ACTIVE_MARK_UPDATE, b"\x01\x17cobblemon:mark_gourmand");
        update(TERA_TYPE_UPDATE, b"\x0fcobblemon:water");
        update(ORIGINAL_TRAINER_UPDATE, b"\x01\x05Sacha");
        let limits = horizon_world::nbt::Limits::default();
        let nbt = |snbt: &str| {
            let (tag, _) = horizon_world::nbt::parse_snbt(snbt).unwrap();
            horizon_world::nbt::write_network(&tag, limits).unwrap()
        };
        update(
            IV_UPDATE,
            &nbt(
                r#"{Base:{"cobblemon:hp":4,"cobblemon:speed":8},HyperTrained:{"cobblemon:hp":31}}"#,
            ),
        );
        update(
            EV_UPDATE,
            &nbt(r#"{"cobblemon:hp":252,"cobblemon:speed":4}"#),
        );
        let charizard = storage.party()[1].as_ref().unwrap();
        assert_eq!(charizard.moves.len(), 2);
        assert_eq!(
            (charizard.moves[0].id.as_str(), charizard.moves[0].pp),
            ("scaryface", 12)
        );
        // Scary Face has 10 PP: 12 with one PP Up, 16 with three.
        assert_eq!(charizard.moves[0].max_pp(10), 12);
        assert_eq!(max_pp(10, 3), 16);
        assert_eq!(max_pp(35, 3), 56);
        assert_eq!(
            charizard.benched_moves,
            [BenchedMove {
                id: "slash".into(),
                raised_pp_stages: 2
            }]
        );
        assert_eq!(charizard.nature, "cobblemon:timid");
        assert_eq!(charizard.minted_nature.as_deref(), Some("cobblemon:modest"));
        // The mint's nature is the one that counts.
        assert_eq!(charizard.effective_nature().unwrap().name, "modest");
        assert_eq!(charizard.ability, "solarpower");
        assert_eq!((charizard.fullness, charizard.friendship), (3, 255));
        assert_eq!(charizard.markings, [1, 2, 0, 0, 0, 0]);
        assert_eq!(
            charizard.active_mark.as_deref(),
            Some("cobblemon:mark_gourmand")
        );
        assert_eq!(charizard.tera_type, "cobblemon:water");
        assert_eq!(charizard.original_trainer_name.as_deref(), Some("Sacha"));
        assert_eq!(charizard.ivs.base, [4, 0, 0, 0, 0, 8]);
        assert_eq!(charizard.ivs.effective(Stat::Hp), 31);
        assert_eq!((charizard.health_iv, charizard.health_ev), (31, 252));
        assert_eq!(charizard.evs, [252, 0, 0, 0, 0, 4]);
        // Full health with them: 78 base, 31 and 252 at level 50.
        assert_eq!(charizard.stat(Stat::Hp, &[78, 84, 78, 109, 85, 100]), 185);
        // Taking the mint away leaves its own nature; no nature for its own changes nothing.
        assert!(
            storage
                .read(NATURE_UPDATE, &about_charizard(&[0, 1]))
                .unwrap()
        );
        assert!(
            storage
                .read(NATURE_UPDATE, &about_charizard(&[0, 0]))
                .unwrap()
        );
        let charizard = storage.party()[1].as_ref().unwrap();
        assert_eq!(charizard.minted_nature, None);
        assert_eq!(charizard.effective_nature().unwrap().name, "timid");
        // No mark worn, and a name the server forgot.
        assert!(
            storage
                .read(ACTIVE_MARK_UPDATE, &about_charizard(&[0]))
                .unwrap()
        );
        assert!(
            storage
                .read(ORIGINAL_TRAINER_UPDATE, &about_charizard(&[0]))
                .unwrap()
        );
        let charizard = storage.party()[1].as_ref().unwrap();
        assert!(charizard.active_mark.is_none() && charizard.original_trainer_name.is_none());
        // Cut short, each is an error; about a Pokémon that is not there, nothing.
        assert!(
            storage
                .read(MOVESET_UPDATE, &about_charizard(&[2, 3]))
                .is_err()
        );
        assert!(
            storage
                .read(BENCHED_MOVES_UPDATE, &about_charizard(&[255, 255]))
                .is_err()
        );
        let mut elsewhere = REFERENCE.to_vec();
        elsewhere.extend_from_slice(&[9; 16]);
        elsewhere.push(40);
        assert!(!storage.read(FRIENDSHIP_UPDATE, &elsewhere).unwrap());
    }

    #[test]
    fn what_the_summary_asks_the_server_is_written_as_cobblemon_reads_it() {
        let pokemon = 0x0aa2_3eda_48cb_433f_81d7_7823_a286_0ae6u128;
        let id = pokemon.to_be_bytes();
        let other = [7u8; 16];
        // `RequestMoveSwapPacket.decode`: three ints.
        assert_eq!(
            request_move_swap(0, 3, 5),
            [0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 5]
        );
        // `BenchMovePacket.decode`: the store's kind, the Pokémon, then each move if there is one.
        let mut expected = vec![1];
        expected.extend_from_slice(&id);
        expected.extend_from_slice(b"\x01\x05slash\x01\x07inferno");
        assert_eq!(
            bench_move(true, pokemon, Some("slash"), Some("inferno")),
            expected
        );
        let mut expected = vec![0];
        expected.extend_from_slice(&id);
        expected.extend_from_slice(b"\x00\x01\x07inferno");
        assert_eq!(bench_move(false, pokemon, None, Some("inferno")), expected);
        let mut expected = vec![1];
        expected.extend_from_slice(&id);
        expected.extend_from_slice(b"\x01\x05slash\x00");
        assert_eq!(bench_move(true, pokemon, Some("slash"), None), expected);
        // `SetNicknamePacket.decode`: the Pokémon, the store's kind, a nullable string.
        let mut expected = id.to_vec();
        expected.extend_from_slice("\u{1}\u{1}\u{6}Dracau".as_bytes());
        assert_eq!(set_nickname(pokemon, true, Some("Dracau")), expected);
        let mut expected = id.to_vec();
        expected.extend_from_slice(&[0, 0]);
        assert_eq!(set_nickname(pokemon, false, None), expected);
        // A name's length is in bytes, as `writeString` counts it.
        assert_eq!(
            set_nickname(pokemon, true, Some("Évoli"))[17..20],
            [1, 6, 0xC3]
        );
        // `SetMarkingsPacket.decode`: a list of ints, then the store's kind.
        let mut expected = id.to_vec();
        expected.extend_from_slice(&[3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 1]);
        assert_eq!(set_markings(pokemon, &[1, 0, 2], true), expected);
        // `SetActiveMarkPacket.decode`: a nullable identifier.
        let mut expected = id.to_vec();
        expected.extend_from_slice(b"\x01\x17cobblemon:mark_gourmand");
        assert_eq!(
            set_active_mark(pokemon, Some("cobblemon:mark_gourmand")),
            expected
        );
        assert_eq!(set_active_mark(pokemon, None)[16..], [0]);
        // `SetItemHiddenPacket.decode`: whether the item shows.
        assert_eq!(set_item_hidden(pokemon, false)[16..], [0]);
        assert_eq!(set_item_hidden(pokemon, true)[..16], id);
        // `SwapPartyPokemonPacket.decode` and `MovePartyPokemonPacket.decode`: a slot is two bytes.
        let mut expected = id.to_vec();
        expected.extend_from_slice(&[0, 1]);
        expected.extend_from_slice(&other);
        expected.extend_from_slice(&[0, 4]);
        assert_eq!(
            swap_party_pokemon((pokemon, 1), (u128::from_be_bytes(other), 4)),
            expected
        );
        let mut expected = id.to_vec();
        expected.extend_from_slice(&[0, 1, 0, 5]);
        assert_eq!(move_party_pokemon(pokemon, 1, 5), expected);
    }

    #[test]
    fn what_the_server_edits_of_a_pokemon_reaches_it() {
        // Captured as the server's console edited a Charmeleon one property at a time
        // (`pokeeditother`): each message's value, behind the ids of the logged-in party's own
        // Charizard in place of that Charmeleon's.
        let mut storage = logged_in();
        let mut update = |channel, captured: &[u8]| {
            assert!(
                storage
                    .read(channel, &about_charizard(&captured[32..]))
                    .unwrap(),
                "{channel}"
            );
        };
        update(
            NATURE_UPDATE,
            include_bytes!("../tests/fixtures/summary1/nature_update-1.bin"),
        );
        update(
            IV_UPDATE,
            include_bytes!("../tests/fixtures/summary1/iv_update.bin"),
        );
        update(
            EV_UPDATE,
            include_bytes!("../tests/fixtures/summary1/ev_update.bin"),
        );
        update(
            FRIENDSHIP_UPDATE,
            include_bytes!("../tests/fixtures/summary1/friendship_update.bin"),
        );
        update(
            FULLNESS_UPDATE,
            include_bytes!("../tests/fixtures/summary1/fullness_update.bin"),
        );
        update(
            ABILITY_UPDATE,
            include_bytes!("../tests/fixtures/summary1/ability_update.bin"),
        );
        update(
            TERA_TYPE_UPDATE,
            include_bytes!("../tests/fixtures/summary1/tera_type_update.bin"),
        );
        let charizard = storage.party()[1].as_ref().unwrap();
        // `nature=timid`, `hp_iv=31 speed_iv=0`, `attack_ev=100 speed_ev=252`, `friendship=200`,
        // `fullness=3`, `ability=solarpower`, `tera_type=water`.
        assert_eq!(charizard.nature, "cobblemon:timid");
        assert_eq!(charizard.minted_nature, None);
        assert_eq!(charizard.ivs.base, [31, 28, 31, 13, 21, 0]);
        assert_eq!(charizard.ivs.hyper_trained, [None; 6]);
        assert_eq!(charizard.evs, [0, 100, 0, 0, 0, 252]);
        assert_eq!((charizard.health_iv, charizard.health_ev), (31, 0));
        assert_eq!((charizard.friendship, charizard.fullness), (200, 3));
        assert_eq!(charizard.ability, "solarpower");
        assert_eq!(charizard.tera_type, "cobblemon:water");
        // Its stats with them: timid raises speed and lowers attack.
        let stats = Stat::ALL.map(|stat| charizard.stat(stat, &[78, 84, 78, 109, 85, 100]));
        assert_eq!(stats, [153, 103, 98, 120, 100, 149]);
        // `nature=modest`, the last edit.
        let modest = include_bytes!("../tests/fixtures/summary1/nature_update-2.bin");
        assert!(
            storage
                .read(NATURE_UPDATE, &about_charizard(&modest[32..]))
                .unwrap()
        );
        let charizard = storage.party()[1].as_ref().unwrap();
        assert_eq!(charizard.effective_nature().unwrap().name, "modest");
        // As captured, they are about a Pokémon this party does not hold: nothing changes.
        assert!(!storage.read(NATURE_UPDATE, modest).unwrap());
    }
}
