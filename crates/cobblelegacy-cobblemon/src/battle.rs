// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A battle, as a Cobblemon 1.8.1 server tells it to a client and takes the client's choices.
//!
//! The server runs the battle (Showdown) and sends what happens one message at a time: who fights
//! (`BattleInitializePacket`), the player's team, each Pokémon that comes in, each line of text, each
//! change of health or status, and before every decision the request it will want answered
//! (`BattleQueueRequestPacket`: the moves that can be chosen, whether a switch is forced), then the
//! signal that it now waits for the answer (`BattleMakeChoicePacket`). [`Battles`] is the client's side
//! of it, `ClientBattle` and the handlers of `client/net/battle`.
//!
//! The client says three things: [`challenge`] (the throwing key on a wild Pokémon), and
//! [`select_actions`] with one [`Response`] for each of its active Pokémon.
//!
//! A Pokémon's health comes two ways: its own trainer's side is told the points it has left, the other
//! side a share of its full health ([`BattlePokemon::flat`]).
//!
//! Not read yet: challenges between players and teams (`battle_challenge_notification`,
//! `team_*`), and what a capture in battle shows (`battle_capture_*` are read past).
use crate::party::PartyPokemon;
use crate::wire::{nullable, string};
use horizon_protocol::{
    Error, Result,
    buf::{Reader, Writer},
};
use horizon_world::nbt::Tag;

/// `BattleInitializePacket.ID`.
pub const INITIALIZE: &str = "cobblemon:battle_initialize";
/// `BattleEndPacket.ID`.
pub const END: &str = "cobblemon:battle_end";
/// `BattleQueueRequestPacket.ID`.
pub const QUEUE_REQUEST: &str = "cobblemon:battle_queue_request";
/// `BattleMakeChoicePacket.ID`.
pub const MAKE_CHOICE: &str = "cobblemon:battle_make_choice";
/// `BattleMadeInvalidChoicePacket.ID`.
pub const MADE_INVALID_CHOICE: &str = "cobblemon:battle_made_invalid_choice";
/// `BattleApplyPassResponsePacket.ID`.
pub const APPLY_PASS_RESPONSE: &str = "cobblemon:battle_apply_pass_response";
/// `BattleMessagePacket.ID`.
pub const MESSAGE: &str = "cobblemon:battle_message";
/// `BattleHealthChangePacket.ID`.
pub const HEALTH_CHANGE: &str = "cobblemon:battle_health_change";
/// `BattleFaintPacket.ID`.
pub const FAINT: &str = "cobblemon:battle_faint";
/// `BattlePersistentStatusPacket.ID`.
pub const PERSISTENT_STATUS: &str = "cobblemon:battle_persistent_status";
/// `BattleSetTeamPokemonPacket.ID`.
pub const SET_TEAM: &str = "cobblemon:battle_set_team";
/// `BattleUpdateTeamPokemonPacket.ID`.
pub const UPDATE_TEAM: &str = "cobblemon:battle_update_team";
/// `BattleSwitchPokemonPacket.ID`.
pub const SWITCH_POKEMON: &str = "cobblemon:battle_switch_pokemon";
/// `BattleReplacePokemonPacket.ID`.
pub const REPLACE_POKEMON: &str = "cobblemon:battle_replace_pokemon";
/// `BattleTransformPokemonPacket.ID`.
pub const TRANSFORM_POKEMON: &str = "cobblemon:battle_transform_pokemon";
/// `BattleSwapPokemonPacket.ID`.
pub const SWAP_POKEMON: &str = "cobblemon:battle_swap_pokemon";
/// `BattleMusicPacket.ID`.
pub const MUSIC: &str = "cobblemon:battle_music";
/// `BattleCaptureStartPacket.ID`, `BattleCaptureShakePacket.ID`, `BattleCaptureEndPacket.ID`.
pub const CAPTURE_START: &str = "cobblemon:battle_capture_start";
pub const CAPTURE_SHAKE: &str = "cobblemon:battle_capture_shake";
pub const CAPTURE_END: &str = "cobblemon:battle_capture_end";

/// `BattleChallengePacket.ID` and `BattleSelectActionsPacket.ID`, which the client sends.
pub const CHALLENGE: &str = "cobblemon:battle_challenge";
pub const SELECT_ACTIONS: &str = "cobblemon:battle_select_actions";

/// Whether [`Battles::read`] reads `channel`.
pub fn reads(channel: &str) -> bool {
    matches!(
        channel,
        INITIALIZE
            | END
            | QUEUE_REQUEST
            | MAKE_CHOICE
            | MADE_INVALID_CHOICE
            | APPLY_PASS_RESPONSE
            | MESSAGE
            | HEALTH_CHANGE
            | FAINT
            | PERSISTENT_STATUS
            | SET_TEAM
            | UPDATE_TEAM
            | SWITCH_POKEMON
            | REPLACE_POKEMON
            | TRANSFORM_POKEMON
            | SWAP_POKEMON
            | MUSIC
            | CAPTURE_START
            | CAPTURE_SHAKE
            | CAPTURE_END
    )
}

/// `ActorType`, by ordinal: who commands one team of a battle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorType {
    Wild,
    Player,
    Npc,
}

/// A Pokémon in the fight, as the side that looks at it is told (`ActiveBattlePokemonDTO`).
#[derive(Clone, Debug, PartialEq)]
pub struct BattlePokemon {
    pub uuid: u128,
    /// Its name, a text component.
    pub name: Tag,
    /// `cobblemon:snorlax`, from the properties.
    pub species: String,
    /// 0 when the properties do not say.
    pub level: u32,
    /// The properties as sent: `cobblemon:snorlax form=normal level=2 shiny=false gender=MALE`.
    pub properties: String,
    /// What picks the model and the texture.
    pub aspects: Vec<String>,
    /// The persistent status' id (`cobblemon:paralysis`).
    pub status: Option<String>,
    /// The health it has: points when [`Self::flat`], else a share of its full health, 0 to 1.
    pub health: f32,
    /// Its full health in points, the opponent's too.
    pub max_health: f32,
    /// Whether [`Self::health`] is in points: for the Pokémon of one's own side.
    pub flat: bool,
    /// It has fainted and waits to be replaced (`BattleFaintPacket`).
    pub fainted: bool,
}

impl BattlePokemon {
    /// `ActiveBattlePokemonDTO`; `flat` is what the reader is told apart from it, which Cobblemon's
    /// client goes by.
    fn read(r: &mut Reader, flat: Option<bool>) -> Result<Self> {
        let uuid = r.uuid()?;
        let name = r.nbt()?;
        let properties = string(r)?;
        let aspects = (0..r.count(4096)?)
            .map(|_| string(r))
            .collect::<Result<_>>()?;
        let status = nullable(r, string)?;
        let health = r.f32()?;
        let max_health = r.f32()?;
        let said_flat = r.bool()?;
        // Stat stages, which a 1.8.1 server never fills: a stat's ordinal and its stages.
        for _ in 0..r.u8()? {
            r.u8()?;
            r.i8()?;
        }
        let property = |key: &str| {
            properties
                .split(' ')
                .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        };
        Ok(Self {
            uuid,
            name,
            species: properties.split(' ').next().unwrap_or_default().to_owned(),
            level: property("level").and_then(|l| l.parse().ok()).unwrap_or(0),
            aspects,
            status,
            health,
            max_health,
            flat: flat.unwrap_or(said_flat),
            fainted: false,
            properties,
        })
    }

    /// How full its health bar is, 0 to 1 (`BattleOverlay`).
    pub fn health_ratio(&self) -> f32 {
        let ratio = if self.flat {
            if self.max_health > 0.0 {
                self.health / self.max_health
            } else {
                0.0
            }
        } else {
            self.health
        };
        ratio.clamp(0.0, 1.0)
    }
}

/// One team of a battle and who commands it (`ClientBattleActor`).
#[derive(Clone, Debug, PartialEq)]
pub struct Actor {
    /// The player's id, or the wild Pokémon's.
    pub uuid: u128,
    pub name: Tag,
    /// `p1`, `p2`: what the positions of its Pokémon start with.
    pub showdown_id: String,
    pub kind: ActorType,
    /// Its active slots, one in a single battle; empty until a Pokémon comes in.
    pub active: Vec<Option<BattlePokemon>>,
}

/// `MoveTarget`, by ordinal: what a move can be aimed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTarget {
    Any,
    All,
    AllAdjacent,
    AllAdjacentFoes,
    Myself,
    Normal,
    RandomNormal,
    Allies,
    AllySide,
    AllyTeam,
    AdjacentAlly,
    AdjacentAllyOrSelf,
    AdjacentFoe,
    FoeSide,
    Scripted,
}

impl MoveTarget {
    const ALL: [Self; 15] = [
        Self::Any,
        Self::All,
        Self::AllAdjacent,
        Self::AllAdjacentFoes,
        Self::Myself,
        Self::Normal,
        Self::RandomNormal,
        Self::Allies,
        Self::AllySide,
        Self::AllyTeam,
        Self::AdjacentAlly,
        Self::AdjacentAllyOrSelf,
        Self::AdjacentFoe,
        Self::FoeSide,
        Self::Scripted,
    ];

    /// `writeEnumConstant`: the ordinal in four bytes, in a request's moves as in the moves' registry.
    pub(crate) fn read(r: &mut Reader) -> Result<Self> {
        let ordinal = r.i32()?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .ok_or_else(|| Error::new(format!("move target ordinal {ordinal}")))
    }

    /// In a single battle, the position a move with this target is aimed at, which the answer must
    /// name: the opponent's for a move that picks one foe, one's own for one that may pick oneself,
    /// none for every other (`MoveTarget.targetList`).
    pub fn in_singles<'a>(self, own: &'a str, foe: &'a str) -> Option<&'a str> {
        match self {
            Self::Normal | Self::Any | Self::AdjacentFoe => Some(foe),
            Self::AdjacentAllyOrSelf => Some(own),
            _ => None,
        }
    }
}

/// A move the request offers (`InBattleMove`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Showdown's id (`electroball`), which the answer names.
    pub id: String,
    /// The name Showdown shows.
    pub name: String,
    pub pp: u8,
    pub max_pp: u8,
    pub target: MoveTarget,
    pub disabled: bool,
}

impl Move {
    /// `mustBeUsed`: a move the Pokémon is locked into, which comes without its own numbers.
    pub fn must_be_used(&self) -> bool {
        self.max_pp == 100 && self.pp == 100 && self.target == MoveTarget::Myself
    }

    /// `canBeUsed`.
    pub fn usable(&self) -> bool {
        self.must_be_used() || (self.pp > 0 && !self.disabled)
    }
}

/// What one active Pokémon may do (`ShowdownMoveset`), without the gimmicks, which are read past.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moveset {
    pub moves: Vec<Move>,
    /// It cannot be switched out.
    pub trapped: bool,
}

/// `InBattleGimmickMove`, read past: a move's name, target and whether it is disabled.
fn skip_gimmick(r: &mut Reader) -> Result<()> {
    string(r)?;
    r.i32()?;
    r.bool()?;
    Ok(())
}

impl Moveset {
    fn read(r: &mut Reader) -> Result<Self> {
        let moves = (0..r.u8()?)
            .map(|_| {
                Ok(Move {
                    id: string(r)?,
                    name: string(r)?,
                    pp: r.u8()?,
                    max_pp: r.u8()?,
                    target: MoveTarget::read(r)?,
                    disabled: r.bool()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let trapped = r.bool()?;
        // Mega evolution and ultra burst.
        r.bool()?;
        r.bool()?;
        // Z-moves, then after the dynamax flag the max moves: each list, when there, has an entry
        // a move, written without a count.
        for list in 0..2 {
            if list == 1 {
                r.bool()?;
            }
            if r.bool()? {
                for _ in 0..moves.len() {
                    if r.bool()? {
                        skip_gimmick(r)?;
                    }
                }
            }
        }
        // The type it can terastallize to.
        nullable(r, string)?;
        Ok(Self { moves, trapped })
    }
}

/// A Pokémon of the player's team as Showdown lists it in a request (`ShowdownPokemon`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamMember {
    /// `Pikachu, <uuid>, L13, M`.
    pub details: String,
    /// `33/33`, `12/33 par`, `0 fnt`.
    pub condition: String,
    pub active: bool,
}

impl TeamMember {
    fn read(r: &mut Reader) -> Result<Self> {
        // `ident`.
        string(r)?;
        let details = string(r)?;
        let condition = string(r)?;
        let active = r.bool()?;
        // Reviving and commanding.
        r.bool()?;
        r.bool()?;
        for _ in 0..r.u8()? {
            string(r)?;
        }
        // Its base ability, its ball and its ability.
        for _ in 0..3 {
            string(r)?;
        }
        // Its base types, then its types.
        for _ in 0..2 {
            for _ in 0..r.u8()? {
                string(r)?;
            }
        }
        Ok(Self {
            details,
            condition,
            active,
        })
    }

    /// The Pokémon's id, the second field of the details (`ShowdownPokemon.uuid`).
    pub fn uuid(&self) -> Option<u128> {
        let text: String = self
            .details
            .split(',')
            .nth(1)?
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect();
        (text.len() == 32)
            .then(|| u128::from_str_radix(&text, 16).ok())
            .flatten()
    }

    /// Whether it has fainted (`0 fnt`).
    pub fn fainted(&self) -> bool {
        self.condition.ends_with(" fnt")
    }
}

/// What the server will want decided next (`ShowdownActionRequest`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// Nothing to decide: the other side has a Pokémon to replace.
    pub wait: bool,
    /// What each active Pokémon may do; none while a switch is forced.
    pub active: Vec<Moveset>,
    /// For each active slot, whether its Pokémon has to be replaced.
    pub force_switch: Vec<bool>,
    pub no_cancel: bool,
    /// The player's team as Showdown has it, the active Pokémon first.
    pub team: Vec<TeamMember>,
}

impl Request {
    fn read(r: &mut Reader) -> Result<Self> {
        let wait = r.bool()?;
        let active = (0..r.u8()?)
            .map(|_| Moveset::read(r))
            .collect::<Result<_>>()?;
        let force_switch = (0..r.u8()?).map(|_| r.bool()).collect::<Result<_>>()?;
        let no_cancel = r.bool()?;
        let team = if r.bool()? {
            // The actor's id, then its Showdown id.
            r.uuid()?;
            string(r)?;
            (0..r.u8()?)
                .map(|_| TeamMember::read(r))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            wait,
            active,
            force_switch,
            no_cancel,
            team,
        })
    }

    /// How many answers the request wants: one a slot (`ShowdownActionRequest.iterate`).
    pub fn slots(&self) -> usize {
        self.active.len().max(self.force_switch.len())
    }

    /// Whether slot `slot`'s Pokémon has to be replaced.
    pub fn forced_switch(&self, slot: usize) -> bool {
        self.force_switch.get(slot).copied().unwrap_or(false)
    }
}

/// The battle the player is in (`ClientBattle`).
#[derive(Clone, Debug, PartialEq)]
pub struct Battle {
    pub id: u128,
    /// The battle type's name: `singles`, `doubles`...
    pub kind: String,
    /// The two sides as the server orders them; in a wild battle the player's is the first.
    pub sides: [Vec<Actor>; 2],
    /// The side the player is on and their actor there. None for a spectator.
    pub mine: Option<(usize, usize)>,
    /// The player's own team in the battle (`BattleSetTeamPokemonPacket`).
    pub team: Vec<PartyPokemon>,
    /// What the server will want decided next.
    pub request: Option<Request>,
    /// The server waits for the request's answer (`mustChoose`).
    pub must_choose: bool,
    /// A capture or an item is under way: the answer it waits for is [`Response::ForcePass`].
    pub pass_expected: bool,
    /// Every line the server sent, text components in the order they came.
    pub messages: Vec<Tag>,
    /// The music the server asked for, a sound's id.
    pub music: Option<String>,
}

impl Battle {
    /// The Pokémon at a position (`p2a`: the actor's Showdown id, then the slot's letter).
    pub fn at_mut(&mut self, pnx: &str) -> Option<&mut BattlePokemon> {
        let (actor, slot) = pnx.split_at_checked(2)?;
        let slot = usize::from(slot.bytes().next()?.checked_sub(b'a')?);
        self.sides
            .iter_mut()
            .flatten()
            .find(|a| a.showdown_id == actor)?
            .active
            .get_mut(slot)?
            .as_mut()
    }

    /// The player's own actor.
    pub fn own(&self) -> Option<&Actor> {
        let (side, actor) = self.mine?;
        self.sides[side].get(actor)
    }

    /// The actors the player fights against, or the second side's for a spectator.
    pub fn opponents(&self) -> &[Actor] {
        &self.sides[self.mine.map_or(1, |(side, _)| 1 - side)]
    }

    /// The position of the player's Pokémon in slot `slot`, and of the one it faces.
    pub fn positions(&self, slot: usize) -> Option<(String, String)> {
        let letter = char::from(b'a' + u8::try_from(slot).ok()?);
        let own = format!("{}{letter}", self.own()?.showdown_id);
        let foe = format!("{}{letter}", self.opponents().first()?.showdown_id);
        Some((own, foe))
    }

    /// The answer that uses move `index` of slot `slot` in a single battle, aimed as the move wants;
    /// none when the request does not offer the move or it cannot be used.
    pub fn move_response(&self, slot: usize, index: usize) -> Option<Response> {
        let chosen = self.request.as_ref()?.active.get(slot)?.moves.get(index)?;
        if !chosen.usable() {
            return None;
        }
        let (own, foe) = self.positions(slot)?;
        Some(Response::Move {
            id: chosen.id.clone(),
            target: chosen.target.in_singles(&own, &foe).map(str::to_owned),
        })
    }

    /// The Pokémon of the team that can come in: not fainted, not already fighting.
    pub fn can_switch_to(&self, pokemon: &PartyPokemon) -> bool {
        let fighting = self.own().is_some_and(|a| {
            a.active
                .iter()
                .flatten()
                .any(|p| p.uuid == pokemon.uuid && !p.fainted)
        });
        !pokemon.fainted() && !fighting
    }
}

/// What the client answers for one of its active Pokémon (`ShowdownActionResponse`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// Send this Pokémon of the team in.
    Switch(u128),
    /// Use the move of this id, at this position when the move picks one.
    Move {
        id: String,
        target: Option<String>,
    },
    /// Let Showdown choose.
    Default,
    /// The answer the server wants while a capture or an item it started is under way.
    ForcePass,
    Pass,
    Forfeit,
    /// What the Run button sends: the server answers how to flee, and asks again.
    FleeAttempt,
}

/// `BattleSelectActionsPacket`: the answers to the request of battle `battle`, one a slot.
pub fn select_actions(battle: u128, responses: &[Response]) -> Vec<u8> {
    let mut w = Writer::new();
    w.uuid(battle);
    w.u8(responses.len() as u8);
    for response in responses {
        match response {
            Response::Switch(pokemon) => {
                w.u8(0);
                w.uuid(*pokemon);
            }
            Response::Move { id, target } => {
                w.u8(1);
                w.string(id);
                w.bool(target.is_some());
                if let Some(target) = target {
                    w.string(target);
                }
                // No gimmick.
                w.bool(false);
            }
            Response::Default => w.u8(2),
            Response::ForcePass => w.u8(3),
            Response::Pass => w.u8(4),
            Response::Forfeit => w.u8(7),
            Response::FleeAttempt => w.u8(8),
        }
    }
    w.data
}

/// `BattleChallengePacket`: asks for a single battle against the entity `entity` (a wild Pokémon),
/// led by the Pokémon `lead` of the party. The server starts it only if the player's view goes to
/// that Pokémon within 12 blocks, and says nothing when it does not.
pub fn challenge(entity: i32, lead: u128) -> Vec<u8> {
    let mut w = Writer::new();
    w.i32(entity);
    w.uuid(lead);
    // `BattleFormat.GEN_9_SINGLES`: the mod, the battle type with its name as a component and its
    // actors a side and slots an actor, the rules, and no level adjustment.
    w.string("cobblemon");
    w.string("singles");
    w.nbt(&Tag::compound([(
        "translate",
        Tag::String("cobblemon.battle.types.singles".into()),
    )]));
    w.u8(1);
    w.u8(1);
    w.u8(3);
    for rule in ["Obtainable", "+Past", "+Unobtainable"] {
        w.string(rule);
    }
    w.i32(-1);
    w.data
}

/// What a message changed, for whoever shows the battle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing the battle's screen shows.
    None,
    /// A battle started.
    Started,
    /// The battle is over.
    Ended,
    /// The server now waits for the player's choice.
    MustChoose,
    /// Something else of the battle's state.
    State,
}

/// The client's side of the battles a server runs: one at a time (`CobblemonClient.battle`).
#[derive(Debug, Default)]
pub struct Battles {
    current: Option<Battle>,
    /// A request that came before its battle: the server sends both at once, in either order.
    early: Option<Request>,
}

impl Battles {
    /// The battle the player is in.
    pub fn current(&self) -> Option<&Battle> {
        self.current.as_ref()
    }

    /// The answers were sent: the server no longer waits (`ClientBattle.mustChoose`).
    pub fn answered(&mut self) {
        if let Some(battle) = &mut self.current {
            battle.must_choose = false;
            battle.pass_expected = false;
        }
    }

    /// The connection is gone, and the battle with it.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// A message on one of the channels [`reads`] names, for the player of id `me`.
    pub fn read(&mut self, channel: &str, payload: &[u8], me: u128) -> Result<Change> {
        let r = &mut Reader::new(payload);
        let change = match channel {
            INITIALIZE => {
                let id = r.uuid()?;
                // `BattleFormat`: the mod, the type (name, displayed name, actors a side, slots an
                // actor), the rules, the level adjustment.
                string(r)?;
                let kind = string(r)?;
                r.nbt()?;
                r.u8()?;
                r.u8()?;
                for _ in 0..r.u8()? {
                    string(r)?;
                }
                r.i32()?;
                let mut side = || -> Result<Vec<Actor>> {
                    (0..r.u8()?)
                        .map(|_| {
                            let uuid = r.uuid()?;
                            let name = r.nbt()?;
                            let showdown_id = string(r)?;
                            let active = (0..r.u8()?)
                                .map(|_| nullable(r, |r| BattlePokemon::read(r, None)))
                                .collect::<Result<_>>()?;
                            let kind = match r.u8()? {
                                0 => ActorType::Wild,
                                1 => ActorType::Player,
                                2 => ActorType::Npc,
                                other => {
                                    return Err(Error::new(format!("actor type {other}")));
                                }
                            };
                            Ok(Actor {
                                uuid,
                                name,
                                showdown_id,
                                kind,
                                active,
                            })
                        })
                        .collect()
                };
                let mut sides = [side()?, side()?];
                let mine = sides
                    .iter()
                    .enumerate()
                    .find_map(|(s, actors)| Some((s, actors.iter().position(|a| a.uuid == me)?)));
                // `BattleInitializeHandler`: the Pokémon of the player's side are told in points,
                // the others in shares, whatever each one's own flag says.
                for (s, actors) in sides.iter_mut().enumerate() {
                    let flat = mine.is_some_and(|(side, _)| side == s);
                    for pokemon in actors
                        .iter_mut()
                        .flat_map(|a| a.active.iter_mut().flatten())
                    {
                        pokemon.flat = flat;
                    }
                }
                self.current = Some(Battle {
                    id,
                    kind,
                    sides,
                    mine,
                    team: Vec::new(),
                    request: self.early.take(),
                    must_choose: false,
                    pass_expected: false,
                    messages: Vec::new(),
                    music: None,
                });
                Change::Started
            }
            QUEUE_REQUEST => {
                let request = Request::read(r)?;
                match &mut self.current {
                    Some(battle) => battle.request = Some(request),
                    None => self.early = Some(request),
                }
                Change::State
            }
            END => {
                self.clear();
                Change::Ended
            }
            _ => {
                let Some(battle) = &mut self.current else {
                    // A battle's message without its battle (it ended, or was never told): nothing
                    // to change, as in Cobblemon's handlers.
                    return Ok(Change::None);
                };
                match channel {
                    MAKE_CHOICE | MADE_INVALID_CHOICE => {
                        battle.must_choose = true;
                        Change::MustChoose
                    }
                    APPLY_PASS_RESPONSE => {
                        battle.pass_expected = true;
                        battle.must_choose = true;
                        Change::MustChoose
                    }
                    MESSAGE => {
                        for _ in 0..r.count(4096)? {
                            battle.messages.push(r.nbt()?);
                        }
                        Change::State
                    }
                    HEALTH_CHANGE => {
                        let pnx = string(r)?;
                        let health = r.f32()?;
                        let max = nullable(r, Reader::f32)?;
                        if let Some(pokemon) = battle.at_mut(&pnx) {
                            pokemon.health = health;
                            if let Some(max) = max {
                                pokemon.max_health = max;
                            }
                        }
                        Change::State
                    }
                    FAINT => {
                        let pnx = string(r)?;
                        if let Some(pokemon) = battle.at_mut(&pnx) {
                            pokemon.fainted = true;
                        }
                        Change::State
                    }
                    PERSISTENT_STATUS => {
                        let pnx = string(r)?;
                        let status = nullable(r, string)?;
                        if let Some(pokemon) = battle.at_mut(&pnx) {
                            pokemon.status = status;
                        }
                        Change::State
                    }
                    SET_TEAM => {
                        battle.team = (0..r.count(64)?)
                            .map(|_| PartyPokemon::from_tag(r.nbt()?))
                            .collect::<Result<_>>()?;
                        Change::State
                    }
                    UPDATE_TEAM => {
                        let pokemon = PartyPokemon::from_tag(r.nbt()?)?;
                        if let Some(place) = battle.team.iter_mut().find(|p| p.uuid == pokemon.uuid)
                        {
                            *place = pokemon;
                        }
                        Change::State
                    }
                    SWITCH_POKEMON | REPLACE_POKEMON | TRANSFORM_POKEMON => {
                        let pnx = string(r)?;
                        let mut pokemon = BattlePokemon::read(r, None)?;
                        let ally = r.bool()?;
                        // `isAlly` says how the health is told, but for a transformation, which
                        // goes by the Pokémon's own flag.
                        if channel != TRANSFORM_POKEMON {
                            pokemon.flat = ally;
                        }
                        let (actor, slot) = pnx
                            .split_at_checked(2)
                            .ok_or_else(|| Error::new(format!("a Pokémon at {pnx}")))?;
                        let slot = slot
                            .bytes()
                            .next()
                            .and_then(|letter| letter.checked_sub(b'a'))
                            .map(usize::from);
                        let place = battle
                            .sides
                            .iter_mut()
                            .flatten()
                            .find(|a| a.showdown_id == actor)
                            .zip(slot)
                            .and_then(|(a, slot)| a.active.get_mut(slot));
                        if let Some(place) = place {
                            *place = Some(pokemon);
                        }
                        Change::State
                    }
                    SWAP_POKEMON => {
                        string(r)?;
                        Change::None
                    }
                    MUSIC => {
                        battle.music = nullable(r, string)?;
                        // Volume, pitch, and whether to start it over.
                        r.f32()?;
                        r.f32()?;
                        r.bool()?;
                        Change::State
                    }
                    CAPTURE_START => {
                        string(r)?;
                        for _ in 0..r.count(4096)? {
                            string(r)?;
                        }
                        string(r)?;
                        Change::None
                    }
                    CAPTURE_SHAKE | CAPTURE_END => {
                        string(r)?;
                        r.bool()?;
                        Change::None
                    }
                    other => return Err(Error::new(format!("{other} is not read here"))),
                }
            }
        };
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{channel}: {} bytes left unread",
                r.remaining()
            )));
        }
        Ok(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::party;

    /// The player the two battles were captured for.
    const ME: u128 = 0xc63d_c98f_3301_33bd_8ec0_c1a8_d3c6_f512;

    /// A captured battle: every message the server sent on Cobblemon's channels from the challenge
    /// on, in order, as (channel, payload).
    fn captured(name: &str) -> Vec<(String, Vec<u8>)> {
        let folder = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
            .into_iter()
            .map(|path| {
                // `flow-007-battle_queue_request.bin`.
                let stem = path.file_stem().unwrap().to_str().unwrap();
                let channel = format!("cobblemon:{}", &stem["flow-000-".len()..]);
                (channel, std::fs::read(&path).unwrap())
            })
            .collect()
    }

    fn text(component: &Tag) -> String {
        match component.get("translate") {
            Some(Tag::String(key)) => key.to_string_lossy(),
            _ => format!("{component:?}"),
        }
    }

    #[test]
    fn a_short_battle_is_followed_from_the_challenge_to_the_win() {
        let mut battles = Battles::default();
        let mut steps = captured("battle1").into_iter();
        let mut next = |battles: &mut Battles, upto: &str| {
            for (channel, payload) in steps.by_ref() {
                if reads(&channel) {
                    battles.read(&channel, &payload, ME).unwrap();
                }
                if channel == upto {
                    return;
                }
            }
            panic!("no {upto} left in the capture");
        };
        next(&mut battles, INITIALIZE);
        let battle = battles.current().unwrap();
        assert_eq!(battle.kind, "singles");
        assert_eq!(battle.mine, Some((0, 0)));
        let own = battle.own().unwrap();
        assert_eq!(
            (own.showdown_id.as_str(), own.kind),
            ("p1", ActorType::Player)
        );
        // The player's Pokémon is not out yet; the wild one is, told as a share of its health.
        assert_eq!(own.active, [None]);
        let foe = &battle.opponents()[0];
        assert_eq!(
            (foe.showdown_id.as_str(), foe.kind),
            ("p2", ActorType::Wild)
        );
        let snorlax = foe.active[0].as_ref().unwrap();
        assert_eq!(snorlax.species, "cobblemon:snorlax");
        assert_eq!(
            (snorlax.level, snorlax.health, snorlax.max_health),
            (2, 1.0, 18.0)
        );
        assert!(!snorlax.flat && snorlax.aspects == ["male"] && snorlax.status.is_none());
        assert_eq!(text(&snorlax.name), "cobblemon.species.snorlax.name");

        next(&mut battles, QUEUE_REQUEST);
        let battle = battles.current().unwrap();
        assert_eq!(
            battle.music.as_deref(),
            Some("cobblemon:battle.pvw.default")
        );
        // The team is the party, and the first Pokémon came in, told in points.
        let team: Vec<_> = battle.team.iter().map(|p| p.species.as_str()).collect();
        assert_eq!(
            team,
            [
                "cobblemon:pikachu",
                "cobblemon:charizard",
                "cobblemon:bulbasaur"
            ]
        );
        let pikachu = battle.own().unwrap().active[0].as_ref().unwrap();
        assert_eq!(
            (pikachu.health, pikachu.max_health, pikachu.flat),
            (33.0, 33.0, true)
        );
        assert_eq!(
            (pikachu.species.as_str(), pikachu.level),
            ("cobblemon:pikachu", 13)
        );
        assert_eq!(pikachu.uuid, battle.team[0].uuid);
        assert_eq!(text(&battle.messages[0]), "cobblemon.battle.switch.self");
        // The request offers its moves; nothing is asked yet.
        let request = battle.request.as_ref().unwrap();
        assert!(!request.wait && !battle.must_choose);
        assert_eq!((request.slots(), request.forced_switch(0)), (1, false));
        assert!(
            request.active[0]
                .moves
                .iter()
                .any(|m| m.id == "electroball" && m.usable())
        );
        assert_eq!(request.team.len(), 3);
        assert_eq!(request.team[0].uuid(), Some(battle.team[0].uuid));
        assert!(request.team[0].active && !request.team[1].active && !request.team[0].fainted());

        next(&mut battles, MAKE_CHOICE);
        let battle = battles.current().unwrap();
        assert!(battle.must_choose);
        // Electro Ball picks one foe: the answer names the wild Pokémon's position.
        let index = request_move(battle, "electroball");
        let response = battle.move_response(0, index).unwrap();
        assert_eq!(
            response,
            Response::Move {
                id: "electroball".into(),
                target: Some("p2a".into())
            }
        );
        // What the capture sent then, and the server took.
        let sent = select_actions(battle.id, &[response]);
        let mut expected = battle.id.to_be_bytes().to_vec();
        expected.extend_from_slice(b"\x01\x01\x0belectroball\x01\x03p2a\x00");
        assert_eq!(sent, expected);
        battles.answered();
        assert!(!battles.current().unwrap().must_choose);

        // The hit takes all of the wild Pokémon's health, then it faints.
        next(&mut battles, FAINT);
        let battle = battles.current().unwrap();
        let snorlax = battle.opponents()[0].active[0].as_ref().unwrap();
        assert_eq!(
            (snorlax.health, snorlax.health_ratio(), snorlax.fainted),
            (0.0, 0.0, true)
        );
        next(&mut battles, END);
        assert!(battles.current().is_none());
    }

    fn request_move(battle: &Battle, id: &str) -> usize {
        battle.request.as_ref().unwrap().active[0]
            .moves
            .iter()
            .position(|m| m.id == id)
            .unwrap()
    }

    #[test]
    fn a_long_battle_goes_through_a_faint_and_a_forced_switch() {
        let mut battles = Battles::default();
        let mut own = party::Storage::new();
        let mut forced = 0;
        let mut lines = 0;
        let mut least = 1.0f32;
        for (channel, payload) in captured("battle2") {
            if reads(&channel) {
                battles
                    .read(&channel, &payload, ME)
                    .unwrap_or_else(|e| panic!("{channel}: {e}"));
            } else if party::reads(&channel) {
                // The party's own messages about the same Pokémon come in between.
                own.read(&channel, &payload)
                    .unwrap_or_else(|e| panic!("{channel}: {e}"));
            }
            let Some(battle) = battles.current() else {
                continue;
            };
            lines = battle.messages.len();
            if let Some(foe) = battle.opponents()[0].active[0].as_ref() {
                least = least.min(foe.health_ratio());
            }
            // Asked to choose while its Pokémon has fainted: the request forces a switch, offers no
            // move, and the team says who can come in.
            if channel == MAKE_CHOICE && battle.request.as_ref().is_some_and(|r| r.forced_switch(0))
            {
                forced += 1;
                let request = battle.request.as_ref().unwrap();
                assert!(request.active.is_empty() && request.slots() == 1);
                assert!(battle.own().unwrap().active[0].as_ref().unwrap().fainted);
                assert!(battle.move_response(0, 0).is_none());
                let able: Vec<_> = battle
                    .team
                    .iter()
                    .filter(|p| battle.can_switch_to(p))
                    .map(|p| p.species.as_str())
                    .collect();
                assert_eq!(able, ["cobblemon:charizard", "cobblemon:bulbasaur"]);
                assert!(request.team[0].fainted());
            }
        }
        assert_eq!(forced, 1);
        // The capture stops mid-battle: Charizard is in, hurt, and the wild Pokémon was hurt too.
        let battle = battles.current().unwrap();
        let charizard = battle.own().unwrap().active[0].as_ref().unwrap();
        assert_eq!(charizard.species, "cobblemon:charizard");
        assert!(charizard.flat && !charizard.fainted);
        assert!(charizard.health > 0.0 && charizard.health < charizard.max_health);
        assert!(least < 1.0 && least > 0.0, "{least}");
        assert!(lines > 20, "{lines}");
        assert!(battle.must_choose);
        // Its moves are offered again, with what is left of each.
        let request = battle.request.as_ref().unwrap();
        let fire_spin = &request.active[0].moves[request_move(battle, "firespin")];
        assert!(fire_spin.pp < fire_spin.max_pp && fire_spin.usable());
    }

    #[test]
    fn answers_are_written_as_the_server_reads_them() {
        let battle = 5u128;
        let all = [
            Response::Switch(9),
            Response::Move {
                id: "growl".into(),
                target: None,
            },
            Response::Default,
            Response::ForcePass,
            Response::Pass,
            Response::Forfeit,
            Response::FleeAttempt,
        ];
        let sent = select_actions(battle, &all);
        let mut expected = battle.to_be_bytes().to_vec();
        expected.push(7);
        expected.push(0);
        expected.extend_from_slice(&9u128.to_be_bytes());
        expected.extend_from_slice(b"\x01\x05growl\x00\x00");
        expected.extend_from_slice(&[2, 3, 4, 7, 8]);
        assert_eq!(sent, expected);
        // A move aimed at oneself, at a foe, at nobody.
        assert_eq!(
            MoveTarget::AdjacentAllyOrSelf.in_singles("p1a", "p2a"),
            Some("p1a")
        );
        assert_eq!(
            MoveTarget::AdjacentFoe.in_singles("p1a", "p2a"),
            Some("p2a")
        );
        assert_eq!(MoveTarget::AllAdjacentFoes.in_singles("p1a", "p2a"), None);
        assert_eq!(MoveTarget::Myself.in_singles("p1a", "p2a"), None);
    }

    #[test]
    fn a_challenge_is_what_the_server_took_in_the_capture() {
        // The capture's own challenge started `battle1`: the same bytes, for its entity and lead.
        let sent = challenge(897, 0x51d7_249d_d3d2_45ef_b95c_1987_6a96_5601);
        assert_eq!(&sent[..4], &897i32.to_be_bytes());
        assert_eq!(&sent[20..38], b"\x09cobblemon\x07singles");
        assert!(
            sent.ends_with(b"\x01\x01\x03\x0aObtainable\x05+Past\x0d+Unobtainable\xff\xff\xff\xff")
        );
        // The format is the one the server sends back in `battle_initialize`, after the battle's id.
        let (_, initialize) = &captured("battle1")[0];
        assert_eq!(&initialize[16..16 + sent.len() - 20], &sent[20..]);
    }

    #[test]
    fn a_request_before_its_battle_and_a_message_without_one_are_taken_in_stride() {
        let flow = captured("battle1");
        let of = |channel: &str| &flow.iter().find(|(c, _)| c == channel).unwrap().1;
        let mut battles = Battles::default();
        assert_eq!(
            battles.read(MESSAGE, of(MESSAGE), ME).unwrap(),
            Change::None
        );
        assert_eq!(
            battles.read(QUEUE_REQUEST, of(QUEUE_REQUEST), ME).unwrap(),
            Change::State
        );
        assert!(battles.current().is_none());
        assert_eq!(
            battles.read(INITIALIZE, of(INITIALIZE), ME).unwrap(),
            Change::Started
        );
        assert!(battles.current().unwrap().request.is_some());
        // Another player's battle: no side is one's own.
        let mut watching = Battles::default();
        watching.read(INITIALIZE, of(INITIALIZE), 1).unwrap();
        assert_eq!(watching.current().unwrap().mine, None);
        // Cut short, or with bytes left: an error.
        assert!(
            battles
                .read(HEALTH_CHANGE, &of(HEALTH_CHANGE)[..5], ME)
                .is_err()
        );
        let mut long = of(FAINT).clone();
        long.push(0);
        assert!(battles.read(FAINT, &long, ME).is_err());
    }
}
