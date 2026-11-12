// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The PC, where a Cobblemon 1.8.1 server keeps the Pokémon a party has no room for.
//!
//! As a player joins the server names their PC (`InitializePCPacket`: its id and how many boxes it has),
//! then sends each box whole (`SetPCBoxPacket`: its name, its wallpaper, and the Pokémon of its thirty
//! slots, each the compound `Pokemon.S2C_CODEC` writes). From then on it sends what changes, as it does
//! for a party: a Pokémon put in a slot, moved, swapped with another or taken out, a box renamed or given
//! another wallpaper. Two of those messages are the party's as well, told apart by their first byte
//! (`RemoveClientPokemonPacket`, `SwapClientPokemonPacket`). [`Storage`] is the client's side of it, the
//! `pcStores` of `ClientStorageManager`.
//!
//! The server also opens the PC's screen (`OpenPCPacket`, after a right click on a PC block or `/pc`)
//! and closes it (`ClosePCPacket`). While it is open the server holds a link between the player and the
//! PC, and refuses what the client asks without one; the client ends it when the screen closes
//! ([`Request::Unlink`]).
//!
//! The other way, everything the screen does is a [`Request`]: the server checks it against what it
//! holds, and answers with the messages above, never with a word of its own. What a click on a slot
//! asks for is [`slot_click`], `StorageWidget.onStorageSlotClicked`.
//!
//! Not read yet: the updates of a field of a Pokémon that is in the PC (`PokemonUpdatePacket`, read for
//! the party's alone), and a pasture's own messages, which open this screen in another mode.
use crate::party::{PartyPokemon, REMOVE_CLIENT_POKEMON, SWAP_CLIENT_POKEMON};
use crate::wire::{nullable, string};
use horizon_protocol::{
    Error, Result,
    buf::{Reader, Writer},
};
use std::collections::HashMap;

/// `InitializePCPacket.ID`.
pub const INITIALIZE_PC: &str = "cobblemon:initialize_pc";
/// `SetPCBoxPacket.ID`.
pub const SET_PC_BOX: &str = "cobblemon:set_pc_box";
/// `SetPCPokemonPacket.ID`.
pub const SET_PC_POKEMON: &str = "cobblemon:set_pc_pokemon";
/// `MoveClientPCPokemonPacket.ID`.
pub const MOVE_CLIENT_PC_POKEMON: &str = "cobblemon:move_client_pc_pokemon";
/// `OpenPCPacket.ID`.
pub const OPEN_PC: &str = "cobblemon:open_pc";
/// `ClosePCPacket.ID`.
pub const CLOSE_PC: &str = "cobblemon:close_pc";
/// `RenamePCBoxPacket.ID`.
pub const RENAME_PC_BOX: &str = "cobblemon:rename_pc_box";
/// `ChangePCBoxWallpaperPacket.ID`.
pub const CHANGE_PC_BOX_WALLPAPER: &str = "cobblemon:change_pc_box_wallpaper";
/// `RequestPCBoxWallpapersPacket.ID`.
pub const REQUEST_PC_BOX_WALLPAPERS: &str = "cobblemon:request_pc_box_wallpapers";
/// `SetPCBoxWallpapersPacket.ID`.
pub const SET_PC_BOX_WALLPAPERS: &str = "cobblemon:set_pc_box_wallpapers";
/// `UnlockPCBoxWallpaperPacket.ID`.
pub const UNLOCK_PC_BOX_WALLPAPER: &str = "cobblemon:unlock_pc_box_wallpaper";

/// `MovePCPokemonPacket.ID`, which the client sends, as all those below.
pub const MOVE_PC_POKEMON: &str = "cobblemon:move_pc_pokemon";
/// `SwapPCPokemonPacket.ID`.
pub const SWAP_PC_POKEMON: &str = "cobblemon:swap_pc_pokemon";
/// `MovePCPokemonToPartyPacket.ID`.
pub const MOVE_PC_POKEMON_TO_PARTY: &str = "cobblemon:move_pc_pokemon_to_party";
/// `MovePartyPokemonToPCPacket.ID`.
pub const MOVE_PARTY_POKEMON_TO_PC: &str = "cobblemon:move_party_pokemon_to_pc";
/// `SwapPCPartyPokemonPacket.ID`.
pub const SWAP_PC_PARTY_POKEMON: &str = "cobblemon:swap_pc_party_pokemon";
/// `MovePartyPokemonPacket.ID`.
pub const MOVE_PARTY_POKEMON: &str = "cobblemon:move_party_pokemon";
/// `SwapPartyPokemonPacket.ID`.
pub const SWAP_PARTY_POKEMON: &str = "cobblemon:swap_party_pokemon";
/// `ReleasePCPokemonPacket.ID`.
pub const RELEASE_PC_POKEMON: &str = "cobblemon:release_pc_pokemon";
/// `ReleasePartyPokemonPacket.ID`.
pub const RELEASE_PARTY_POKEMON: &str = "cobblemon:release_party_pokemon";
/// `UnlinkPlayerFromPCPacket.ID`.
pub const UNLINK_PLAYER_FROM_PC: &str = "cobblemon:unlink_player_from_pc";
/// `RequestRenamePCBoxPacket.ID`.
pub const REQUEST_RENAME_PC_BOX: &str = "cobblemon:request_rename_pc_box";
/// `RequestChangePCBoxWallpaperPacket.ID`.
pub const REQUEST_CHANGE_PC_BOX_WALLPAPER: &str = "cobblemon:request_change_pc_box_wallpaper";
/// `SortPCBoxPacket.ID`.
pub const SORT_PC_BOX: &str = "cobblemon:sort_pc_box";
/// `PCBoxWallpapersPacket.ID`.
pub const PC_BOX_WALLPAPERS: &str = "cobblemon:pc_box_wallpapers";
/// `MarkPCBoxWallpapersSeenPacket.ID`.
pub const MARK_PC_BOX_WALLPAPERS_SEEN: &str = "cobblemon:mark_pc_box_wallpapers_seen";

/// `POKEMON_PER_BOX`: the slots of a box, six a row.
pub const POKEMON_PER_BOX: usize = 30;
/// `PCBoxWallpaperRepository.defaultWallpaper`: what a box wears until the server says.
pub const DEFAULT_WALLPAPER: &str =
    "cobblemon:textures/gui/pc/wallpaper/basic/wallpaper_basic_05.png";
/// The longest name `RequestRenamePCBoxHandler` takes for a box.
pub const BOX_NAME_LENGTH: usize = 19;

/// Whether [`Storage::read`] reads `channel` and no other reader does.
pub fn reads(channel: &str) -> bool {
    matches!(
        channel,
        INITIALIZE_PC
            | SET_PC_BOX
            | SET_PC_POKEMON
            | MOVE_CLIENT_PC_POKEMON
            | OPEN_PC
            | CLOSE_PC
            | RENAME_PC_BOX
            | CHANGE_PC_BOX_WALLPAPER
            | REQUEST_PC_BOX_WALLPAPERS
            | SET_PC_BOX_WALLPAPERS
            | UNLOCK_PC_BOX_WALLPAPER
    )
}

/// Whether `channel` is one a party and a PC share: [`Storage::read`] takes the messages about a PC,
/// `party::Storage::read` those about a party.
pub fn shares(channel: &str) -> bool {
    matches!(channel, REMOVE_CLIENT_POKEMON | SWAP_CLIENT_POKEMON)
}

/// `PCPosition`: a slot of a box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PcPosition {
    pub pc_box: u16,
    pub slot: u8,
}

impl PcPosition {
    pub fn new(pc_box: usize, slot: usize) -> Self {
        Self {
            pc_box: pc_box as u16,
            slot: slot as u8,
        }
    }

    /// `readPCPosition`: the box in two bytes, the slot in one.
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(Self {
            pc_box: r.u16()?,
            slot: r.u8()?,
        })
    }

    fn write(self, w: &mut Writer) {
        w.u16(self.pc_box);
        w.u8(self.slot);
    }
}

/// `StorePosition`: where a Pokémon the screen shows is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Pc(PcPosition),
    /// A slot of the player's party (`PartyPosition`).
    Party(u16),
}

/// `ClientBox`: a box as the client holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct PcBox {
    /// The name its owner gave it. Without one the screen calls it by its number.
    pub name: Option<String>,
    /// The texture behind its slots (`cobblemon:textures/gui/pc/wallpaper/...png`).
    pub wallpaper: String,
    /// Its [`POKEMON_PER_BOX`] slots.
    pub slots: Vec<Option<PartyPokemon>>,
}

impl Default for PcBox {
    fn default() -> Self {
        Self {
            name: None,
            wallpaper: DEFAULT_WALLPAPER.to_owned(),
            slots: vec![None; POKEMON_PER_BOX],
        }
    }
}

impl PcBox {
    /// `indexOfFirst { it == null }`: the first slot with no Pokémon.
    pub fn first_free(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }
}

/// `ClientPC`: a PC's boxes.
#[derive(Clone, Debug, PartialEq)]
pub struct Pc {
    pub uuid: u128,
    pub boxes: Vec<PcBox>,
    /// `hasOverflowed`: the server holds Pokémon of this PC that fit in none of its boxes.
    pub overflowed: bool,
}

impl Pc {
    /// `ClientPC.get`.
    pub fn get(&self, at: PcPosition) -> Option<&PartyPokemon> {
        self.boxes
            .get(usize::from(at.pc_box))?
            .slots
            .get(usize::from(at.slot))?
            .as_ref()
    }

    fn slot_mut(&mut self, at: PcPosition) -> Option<&mut Option<PartyPokemon>> {
        self.boxes
            .get_mut(usize::from(at.pc_box))?
            .slots
            .get_mut(usize::from(at.slot))
    }

    /// `ClientPC.getPosition`, by the Pokémon's id.
    pub fn position(&self, pokemon: u128) -> Option<PcPosition> {
        self.boxes.iter().enumerate().find_map(|(number, pc_box)| {
            let slot = pc_box
                .slots
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|p| p.uuid == pokemon))?;
            Some(PcPosition::new(number, slot))
        })
    }

    /// How many Pokémon it holds.
    pub fn count(&self) -> usize {
        self.boxes
            .iter()
            .map(|b| b.slots.iter().flatten().count())
            .sum()
    }
}

/// What a message changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Nothing the client holds: a message about a party, a store the server never named, or a
    /// Pokémon that is not there, as in Cobblemon.
    Nothing,
    /// What a PC holds.
    Stores,
    /// `OpenPCHandler`: the screen opens on the PC `store`, on the box `open_on` when the server says
    /// which (the one last looked at otherwise).
    Open {
        store: u128,
        open_on: Option<usize>,
        /// The wallpapers unlocked since the PC was last looked at.
        unseen_wallpapers: Vec<String>,
    },
    /// `ClosePCHandler`: the screen closes if it shows the PC `store`. The server sends none when it
    /// refuses a request for want of a link: the screen it comes for is then whichever is open.
    Close { store: Option<u128> },
    /// `RequestPCBoxWallpapersHandler`: the server asks which wallpapers the client has
    /// ([`Request::Wallpapers`]).
    WallpapersAsked,
}

/// The PCs the server named (`ClientStorageManager.pcStores`), and the wallpapers it lets the player
/// give a box.
#[derive(Debug, Default)]
pub struct Storage {
    stores: HashMap<u128, Pc>,
    /// `PCBoxWallpaperRepository.availableWallpapers`, once the server said. Until then every wallpaper
    /// the client has is one.
    wallpapers: Option<Vec<String>>,
}

/// `readList { readResourceLocation() }`: a count, then that many ids.
fn ids(r: &mut Reader) -> Result<Vec<String>> {
    let count = r.count(4096)?;
    (0..count).map(|_| string(r)).collect()
}

/// A message of the server, read whole before it changes anything.
enum Message {
    Initialize {
        store: u128,
        boxes: usize,
        overflowed: bool,
    },
    SetBox {
        store: u128,
        number: usize,
        held: PcBox,
    },
    SetPokemon {
        store: u128,
        at: PcPosition,
        pokemon: Box<PartyPokemon>,
    },
    Move {
        store: u128,
        pokemon: u128,
        to: PcPosition,
    },
    Remove {
        party: bool,
        store: u128,
        pokemon: u128,
    },
    Swap {
        party: bool,
        store: u128,
        first: u128,
        second: u128,
    },
    Open {
        store: u128,
        open_on: Option<i32>,
        unseen_wallpapers: Vec<String>,
    },
    Close {
        store: Option<u128>,
    },
    Rename {
        store: u128,
        number: usize,
        name: String,
    },
    Wallpaper {
        store: u128,
        number: usize,
        wallpaper: String,
    },
    WallpapersAsked,
    Wallpapers(Vec<String>),
    Unlocked(String),
}

impl Message {
    fn read(channel: &str, r: &mut Reader) -> Result<Self> {
        Ok(match channel {
            INITIALIZE_PC => Self::Initialize {
                store: r.uuid()?,
                boxes: usize::from(r.u16()?),
                overflowed: r.bool()?,
            },
            SET_PC_BOX => {
                let store = r.uuid()?;
                let number = usize::from(r.u16()?);
                let name = string(r)?;
                let wallpaper = string(r)?;
                let mut held = PcBox {
                    name: (!name.trim().is_empty()).then_some(name),
                    wallpaper,
                    ..PcBox::default()
                };
                // `writeMapK`: a count in one byte, then for each Pokémon its slot and its compound,
                // the compound's length before it.
                for _ in 0..r.u8()? {
                    let slot = usize::from(r.u8()?);
                    let length = usize::try_from(r.i32()?)
                        .map_err(|_| Error::new("a Pokémon of a box with a length below 0"))?;
                    let inner = &mut Reader::new(r.bytes(length)?);
                    let pokemon = PartyPokemon::from_tag(inner.nbt()?)?;
                    if inner.remaining() != 0 {
                        return Err(Error::new(format!(
                            "{channel}: {} bytes left unread of the Pokémon in slot {slot}",
                            inner.remaining()
                        )));
                    }
                    *held.slots.get_mut(slot).ok_or_else(|| {
                        Error::new(format!("a box with a Pokémon in slot {slot}"))
                    })? = Some(pokemon);
                }
                Self::SetBox {
                    store,
                    number,
                    held,
                }
            }
            SET_PC_POKEMON => Self::SetPokemon {
                store: r.uuid()?,
                at: PcPosition::read(r)?,
                pokemon: Box::new(PartyPokemon::from_tag(r.nbt()?)?),
            },
            MOVE_CLIENT_PC_POKEMON => Self::Move {
                store: r.uuid()?,
                pokemon: r.uuid()?,
                to: PcPosition::read(r)?,
            },
            REMOVE_CLIENT_POKEMON => Self::Remove {
                party: r.bool()?,
                store: r.uuid()?,
                pokemon: r.uuid()?,
            },
            SWAP_CLIENT_POKEMON => Self::Swap {
                party: r.bool()?,
                store: r.uuid()?,
                first: r.uuid()?,
                second: r.uuid()?,
            },
            OPEN_PC => Self::Open {
                store: r.uuid()?,
                open_on: nullable(r, Reader::i32)?,
                unseen_wallpapers: ids(r)?,
            },
            CLOSE_PC => Self::Close {
                store: nullable(r, Reader::uuid)?,
            },
            RENAME_PC_BOX => Self::Rename {
                store: r.uuid()?,
                number: usize::from(r.u16()?),
                name: string(r)?,
            },
            CHANGE_PC_BOX_WALLPAPER => Self::Wallpaper {
                store: r.uuid()?,
                number: usize::from(r.u16()?),
                wallpaper: string(r)?,
            },
            REQUEST_PC_BOX_WALLPAPERS => Self::WallpapersAsked,
            SET_PC_BOX_WALLPAPERS => Self::Wallpapers(ids(r)?),
            UNLOCK_PC_BOX_WALLPAPER => Self::Unlocked(string(r)?),
            other => return Err(Error::new(format!("{other} is not read here"))),
        })
    }
}

impl Storage {
    pub fn new() -> Self {
        Self::default()
    }

    /// The PC `store`, once the server named it.
    pub fn pc(&self, store: u128) -> Option<&Pc> {
        self.stores.get(&store)
    }

    /// Every PC the server named. A player has one; a server's mod may name others.
    pub fn pcs(&self) -> impl Iterator<Item = &Pc> {
        self.stores.values()
    }

    /// The wallpapers the server lets the player give a box; `None` until it said.
    pub fn wallpapers(&self) -> Option<&[String]> {
        self.wallpapers.as_deref()
    }

    /// A message on one of the channels [`reads`] or [`shares`] names. One that does not read to its
    /// last byte changes nothing.
    pub fn read(&mut self, channel: &str, payload: &[u8]) -> Result<Change> {
        let r = &mut Reader::new(payload);
        let message = Message::read(channel, r)?;
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{channel}: {} bytes left unread",
                r.remaining()
            )));
        }
        self.apply(message)
    }

    fn apply(&mut self, message: Message) -> Result<Change> {
        /// A slot's content, when the PC has that slot.
        fn put(pc: &mut Pc, at: PcPosition, pokemon: Option<PartyPokemon>) {
            if let Some(place) = pc.slot_mut(at) {
                *place = pokemon;
            }
        }
        Ok(match message {
            Message::Initialize {
                store,
                boxes,
                overflowed,
            } => {
                self.stores.insert(
                    store,
                    Pc {
                        uuid: store,
                        boxes: vec![PcBox::default(); boxes],
                        overflowed,
                    },
                );
                Change::Stores
            }
            Message::SetBox {
                store,
                number,
                held,
            } => match self.stores.get_mut(&store) {
                Some(pc) => {
                    // `SetPCBoxHandler`: a box past the last makes the PC that much longer.
                    if pc.boxes.len() <= number {
                        pc.boxes.resize_with(number + 1, PcBox::default);
                    }
                    pc.boxes[number] = held;
                    Change::Stores
                }
                None => Change::Nothing,
            },
            Message::SetPokemon { store, at, pokemon } => {
                let place = self.stores.get_mut(&store).and_then(|pc| pc.slot_mut(at));
                match place {
                    Some(place) => {
                        *place = Some(*pokemon);
                        Change::Stores
                    }
                    None => Change::Nothing,
                }
            }
            Message::Move { store, pokemon, to } => {
                let found = self
                    .stores
                    .get_mut(&store)
                    .and_then(|pc| Some((pc.position(pokemon)?, pc)));
                match found {
                    Some((from, pc)) => {
                        // `ClientStorage.move` would empty its slot and put it nowhere.
                        if pc.slot_mut(to).is_none() {
                            return Err(Error::new(format!("a Pokémon moved to {to:?}")));
                        }
                        let moved = pc.slot_mut(from).and_then(Option::take);
                        put(pc, to, moved);
                        Change::Stores
                    }
                    None => Change::Nothing,
                }
            }
            Message::Remove {
                party,
                store,
                pokemon,
            } => {
                let found = self
                    .stores
                    .get_mut(&store)
                    .filter(|_| !party)
                    .and_then(|pc| Some((pc.position(pokemon)?, pc)));
                match found {
                    Some((at, pc)) => {
                        put(pc, at, None);
                        Change::Stores
                    }
                    None => Change::Nothing,
                }
            }
            Message::Swap {
                party,
                store,
                first,
                second,
            } => {
                let found = self
                    .stores
                    .get_mut(&store)
                    .filter(|_| !party)
                    .and_then(|pc| Some((pc.position(first)?, pc.position(second)?, pc)));
                match found {
                    Some((a, b, pc)) => {
                        let one = pc.slot_mut(a).and_then(Option::take);
                        let other = pc.slot_mut(b).and_then(Option::take);
                        put(pc, a, other);
                        put(pc, b, one);
                        Change::Stores
                    }
                    None => Change::Nothing,
                }
            }
            // `OpenPCHandler`: a PC the server never named opens nothing.
            Message::Open {
                store,
                open_on,
                unseen_wallpapers,
            } if self.stores.contains_key(&store) => Change::Open {
                store,
                open_on: open_on.and_then(|number| usize::try_from(number).ok()),
                unseen_wallpapers,
            },
            Message::Open { .. } => Change::Nothing,
            Message::Close { store } => Change::Close { store },
            Message::Rename {
                store,
                number,
                name,
            } => match self.box_mut(store, number) {
                Some(held) => {
                    held.name = (!name.trim().is_empty()).then_some(name);
                    Change::Stores
                }
                None => Change::Nothing,
            },
            Message::Wallpaper {
                store,
                number,
                wallpaper,
            } => match self.box_mut(store, number) {
                Some(held) => {
                    held.wallpaper = wallpaper;
                    Change::Stores
                }
                None => Change::Nothing,
            },
            Message::WallpapersAsked => Change::WallpapersAsked,
            Message::Wallpapers(wallpapers) => {
                self.wallpapers = Some(wallpapers);
                Change::Stores
            }
            Message::Unlocked(texture) => {
                let wallpapers = self.wallpapers.get_or_insert_with(Vec::new);
                if !wallpapers.contains(&texture) {
                    wallpapers.push(texture);
                }
                Change::Stores
            }
        })
    }

    fn box_mut(&mut self, store: u128, number: usize) -> Option<&mut PcBox> {
        self.stores.get_mut(&store)?.boxes.get_mut(number)
    }
}

/// `PokemonSortMode`: what the screen's sorting buttons order a box by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Level,
    Type,
    PokedexNumber,
    Gender,
}

impl SortMode {
    pub const ALL: [Self; 5] = [
        Self::Name,
        Self::Level,
        Self::Type,
        Self::PokedexNumber,
        Self::Gender,
    ];

    /// The constant's name in lower case, which names its button's texture and its tooltip.
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Level => "level",
            Self::Type => "type",
            Self::PokedexNumber => "pokedex_number",
            Self::Gender => "gender",
        }
    }
}

/// What the PC's screen asks the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `MovePCPokemonPacket`: to an empty slot of the PC.
    MovePc {
        pokemon: u128,
        from: PcPosition,
        to: PcPosition,
    },
    /// `SwapPCPokemonPacket`: two Pokémon of the PC change places.
    SwapPc {
        first: (u128, PcPosition),
        second: (u128, PcPosition),
    },
    /// `MovePCPokemonToPartyPacket`; with no slot, or one that is taken, the party's first free one.
    PcToParty {
        pokemon: u128,
        from: PcPosition,
        to: Option<u16>,
    },
    /// `MovePartyPokemonToPCPacket`; with no position, or one that is taken, the PC's first free one.
    PartyToPc {
        pokemon: u128,
        from: u16,
        to: Option<PcPosition>,
    },
    /// `SwapPCPartyPokemonPacket`: a Pokémon of the party and one of the PC change places.
    SwapPcParty {
        party: (u128, u16),
        pc: (u128, PcPosition),
    },
    /// `MovePartyPokemonPacket`: to an empty slot of the party.
    MoveParty { pokemon: u128, from: u16, to: u16 },
    /// `SwapPartyPokemonPacket`.
    SwapParty {
        first: (u128, u16),
        second: (u128, u16),
    },
    /// `ReleasePCPokemonPacket`: the Pokémon is let go for good.
    ReleasePc { pokemon: u128, at: PcPosition },
    /// `ReleasePartyPokemonPacket`.
    ReleaseParty { pokemon: u128, slot: u16 },
    /// `UnlinkPlayerFromPCPacket`: the screen closed.
    Unlink,
    /// `RequestRenamePCBoxPacket`; an empty name gives the box its number back.
    RenameBox {
        store: u128,
        pc_box: u16,
        name: String,
    },
    /// `RequestChangePCBoxWallpaperPacket`; `alt` is the wallpaper's other look, when it is the one
    /// asked for.
    ChangeWallpaper {
        store: u128,
        pc_box: u16,
        wallpaper: String,
        alt: Option<String>,
    },
    /// `SortPCBoxPacket`.
    SortBox {
        store: u128,
        pc_box: u16,
        mode: SortMode,
        descending: bool,
    },
    /// `PCBoxWallpapersPacket`: the wallpapers the client has, in answer to
    /// [`Change::WallpapersAsked`].
    Wallpapers(Vec<String>),
    /// `MarkPCBoxWallpapersSeenPacket`.
    WallpapersSeen(Vec<String>),
}

impl Request {
    /// The channel the request goes on.
    pub fn channel(&self) -> &'static str {
        match self {
            Self::MovePc { .. } => MOVE_PC_POKEMON,
            Self::SwapPc { .. } => SWAP_PC_POKEMON,
            Self::PcToParty { .. } => MOVE_PC_POKEMON_TO_PARTY,
            Self::PartyToPc { .. } => MOVE_PARTY_POKEMON_TO_PC,
            Self::SwapPcParty { .. } => SWAP_PC_PARTY_POKEMON,
            Self::MoveParty { .. } => MOVE_PARTY_POKEMON,
            Self::SwapParty { .. } => SWAP_PARTY_POKEMON,
            Self::ReleasePc { .. } => RELEASE_PC_POKEMON,
            Self::ReleaseParty { .. } => RELEASE_PARTY_POKEMON,
            Self::Unlink => UNLINK_PLAYER_FROM_PC,
            Self::RenameBox { .. } => REQUEST_RENAME_PC_BOX,
            Self::ChangeWallpaper { .. } => REQUEST_CHANGE_PC_BOX_WALLPAPER,
            Self::SortBox { .. } => SORT_PC_BOX,
            Self::Wallpapers(_) => PC_BOX_WALLPAPERS,
            Self::WallpapersSeen(_) => MARK_PC_BOX_WALLPAPERS_SEEN,
        }
    }

    /// The request's payload, as its packet's `encode` writes it.
    pub fn payload(&self) -> Vec<u8> {
        let mut w = Writer::new();
        // `writeNullable`: whether there is a value, then the value.
        fn optional<T>(w: &mut Writer, value: Option<T>, write: impl FnOnce(&mut Writer, T)) {
            w.bool(value.is_some());
            if let Some(value) = value {
                write(w, value);
            }
        }
        match self {
            Self::MovePc { pokemon, from, to } => {
                w.uuid(*pokemon);
                from.write(&mut w);
                to.write(&mut w);
            }
            Self::SwapPc { first, second } => {
                for (pokemon, at) in [first, second] {
                    w.uuid(*pokemon);
                    at.write(&mut w);
                }
            }
            Self::PcToParty { pokemon, from, to } => {
                w.uuid(*pokemon);
                from.write(&mut w);
                optional(&mut w, *to, Writer::u16);
            }
            Self::PartyToPc { pokemon, from, to } => {
                w.uuid(*pokemon);
                w.u16(*from);
                optional(&mut w, *to, |w, to| to.write(w));
            }
            Self::SwapPcParty { party, pc } => {
                w.uuid(party.0);
                w.u16(party.1);
                w.uuid(pc.0);
                pc.1.write(&mut w);
            }
            Self::MoveParty { pokemon, from, to } => {
                w.uuid(*pokemon);
                w.u16(*from);
                w.u16(*to);
            }
            Self::SwapParty { first, second } => {
                for (pokemon, slot) in [first, second] {
                    w.uuid(*pokemon);
                    w.u16(*slot);
                }
            }
            Self::ReleasePc { pokemon, at } => {
                w.uuid(*pokemon);
                at.write(&mut w);
            }
            Self::ReleaseParty { pokemon, slot } => {
                w.uuid(*pokemon);
                w.u16(*slot);
            }
            Self::Unlink => {}
            Self::RenameBox {
                store,
                pc_box,
                name,
            } => {
                w.uuid(*store);
                w.u16(*pc_box);
                w.string(name);
            }
            Self::ChangeWallpaper {
                store,
                pc_box,
                wallpaper,
                alt,
            } => {
                w.uuid(*store);
                w.u16(*pc_box);
                w.string(wallpaper);
                optional(&mut w, alt.as_deref(), Writer::string);
            }
            Self::SortBox {
                store,
                pc_box,
                mode,
                descending,
            } => {
                w.uuid(*store);
                w.u16(*pc_box);
                w.u8(*mode as u8);
                w.bool(*descending);
            }
            Self::Wallpapers(wallpapers) | Self::WallpapersSeen(wallpapers) => {
                w.varint(wallpapers.len() as i32);
                for wallpaper in wallpapers {
                    w.string(wallpaper);
                }
            }
        }
        w.data
    }
}

/// What the screen holds around a click on a slot, which decides what the click may ask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Around {
    /// How many Pokémon the party has.
    pub party_count: usize,
    /// The party's first slot with no Pokémon.
    pub party_free: Option<u16>,
    /// The first slot with no Pokémon of the box the screen shows.
    pub box_free: Option<PcPosition>,
    /// `ServerSettings.preventCompletePartyDeposit`: the party's last Pokémon stays in it.
    pub keep_one_in_party: bool,
}

/// What a click on a slot does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    /// Nothing: an empty slot with no Pokémon in hand, or a move the server would refuse.
    Nothing,
    /// The Pokémon of the slot is taken in hand.
    Grab,
    /// The Pokémon in hand goes back where it was.
    PutBack,
    /// The server is asked; `dropped` when it was for the Pokémon in hand, which then leaves it.
    Ask { request: Request, dropped: bool },
}

/// `StorageWidget.onStorageSlotClicked`: a click on the slot `clicked`, which holds the Pokémon
/// `there`, with the Pokémon `held` in hand (taken from the place it names) and Shift down or not.
pub fn slot_click(
    held: Option<(Place, u128)>,
    clicked: Place,
    there: Option<u128>,
    shift: bool,
    around: Around,
) -> Click {
    let Some((from, pokemon)) = held else {
        let Some(there) = there else {
            return Click::Nothing;
        };
        // Shift sends it straight to the other side's first free slot, when there is one.
        if shift {
            let request = match clicked {
                Place::Pc(at) => around.party_free.map(|slot| Request::PcToParty {
                    pokemon: there,
                    from: at,
                    to: Some(slot),
                }),
                Place::Party(_) if around.keep_one_in_party && around.party_count == 1 => {
                    return Click::Nothing;
                }
                Place::Party(slot) => around.box_free.map(|at| Request::PartyToPc {
                    pokemon: there,
                    from: slot,
                    to: Some(at),
                }),
            };
            if let Some(request) = request {
                return Click::Ask {
                    request,
                    dropped: false,
                };
            }
        }
        return Click::Grab;
    };
    if from == clicked {
        return Click::PutBack;
    }
    let request = match (from, clicked) {
        (Place::Pc(from), Place::Pc(to)) => match there {
            Some(there) => Request::SwapPc {
                first: (there, to),
                second: (pokemon, from),
            },
            None => Request::MovePc { pokemon, from, to },
        },
        (Place::Pc(from), Place::Party(to)) => match there {
            Some(there) => Request::SwapPcParty {
                party: (there, to),
                pc: (pokemon, from),
            },
            None => Request::PcToParty {
                pokemon,
                from,
                to: Some(to),
            },
        },
        (Place::Party(from), Place::Pc(to)) => match there {
            Some(there) => Request::SwapPcParty {
                party: (pokemon, from),
                pc: (there, to),
            },
            // The party's last Pokémon is not put away.
            None if around.keep_one_in_party && around.party_count == 1 => return Click::Nothing,
            None => Request::PartyToPc {
                pokemon,
                from,
                to: Some(to),
            },
        },
        (Place::Party(from), Place::Party(to)) => match there {
            Some(there) => Request::SwapParty {
                first: (there, to),
                second: (pokemon, from),
            },
            None => Request::MoveParty { pokemon, from, to },
        },
    };
    Click::Ask {
        request,
        dropped: true,
    }
}

/// `StorageWidget.canDeleteSelected`: whether the Pokémon in hand, taken from `from`, may be released.
/// The party's last one may not.
pub fn can_release(from: Place, party_count: usize) -> bool {
    !(matches!(from, Place::Party(_)) && party_count <= 1)
}

/// The request that releases the Pokémon `pokemon` in hand, taken from `from`.
pub fn release(from: Place, pokemon: u128) -> Request {
    match from {
        Place::Pc(at) => Request::ReleasePc { pokemon, at },
        Place::Party(slot) => Request::ReleaseParty { pokemon, slot },
    }
}

/// `StorageWidget.box`'s setter: the box `step` boxes after `current` of `count`, round both ends.
pub fn box_after(current: usize, step: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let value = current as i64 + i64::from(step);
    // One past the end is the first box, whatever the step; before the first, the last.
    if value < 0 {
        count - 1
    } else if (value as usize) < count {
        value as usize
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_world::nbt::{Limits, parse_snbt, write_network};

    const STORE: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;

    /// A Pokémon's compound, as short as `PartyPokemon::from_tag` takes one.
    fn compound(uuid: u32, species: &str, level: u32) -> Vec<u8> {
        let snbt = format!(
            r#"{{UUID:[I;0,0,0,{uuid}],Species:"cobblemon:{species}",FormId:"normal",Level:{level},
            Experience:0,Health:10,Gender:"MALE",CaughtBall:"cobblemon:poke_ball"}}"#
        );
        let (tag, _) = parse_snbt(&snbt).unwrap();
        write_network(&tag, Limits::default()).unwrap()
    }

    fn initialize(boxes: u16) -> Vec<u8> {
        let mut w = Writer::new();
        w.uuid(STORE);
        w.u16(boxes);
        w.bool(false);
        w.data
    }

    fn set_box(number: u16, name: &str, pokemon: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut w = Writer::new();
        w.uuid(STORE);
        w.u16(number);
        w.string(name);
        w.string(DEFAULT_WALLPAPER);
        w.u8(pokemon.len() as u8);
        for (slot, compound) in pokemon {
            w.u8(*slot);
            w.i32(compound.len() as i32);
            w.bytes(compound);
        }
        w.data
    }

    /// A PC of three boxes: a Pikachu and an Eevee in the first, a Mew in the last.
    fn pc() -> Storage {
        let mut storage = Storage::new();
        assert_eq!(
            storage.read(INITIALIZE_PC, &initialize(3)).unwrap(),
            Change::Stores
        );
        let first = set_box(
            0,
            "",
            &[(0, compound(1, "pikachu", 5)), (7, compound(2, "eevee", 9))],
        );
        assert_eq!(storage.read(SET_PC_BOX, &first).unwrap(), Change::Stores);
        let last = set_box(2, "Legends", &[(29, compound(3, "mew", 50))]);
        assert_eq!(storage.read(SET_PC_BOX, &last).unwrap(), Change::Stores);
        storage
    }

    /// Where the Pokémon are: (box, slot, the last part of the id).
    fn held(storage: &Storage) -> Vec<(usize, usize, u128)> {
        let pc = storage.pc(STORE).unwrap();
        let mut held = Vec::new();
        for (number, pc_box) in pc.boxes.iter().enumerate() {
            for (slot, pokemon) in pc_box.slots.iter().enumerate() {
                if let Some(pokemon) = pokemon {
                    held.push((number, slot, pokemon.uuid));
                }
            }
        }
        held
    }

    fn about(party: Option<bool>, uuids: &[u128], to: Option<PcPosition>) -> Vec<u8> {
        let mut w = Writer::new();
        if let Some(party) = party {
            w.bool(party);
        }
        w.uuid(STORE);
        for uuid in uuids {
            w.uuid(*uuid);
        }
        if let Some(to) = to {
            to.write(&mut w);
        }
        w.data
    }

    #[test]
    fn a_pc_is_its_boxes_each_with_its_name_and_its_pokemon() {
        let storage = pc();
        let pc = storage.pc(STORE).unwrap();
        assert_eq!((pc.boxes.len(), pc.count(), pc.overflowed), (3, 3, false));
        assert_eq!(held(&storage), [(0, 0, 1), (0, 7, 2), (2, 29, 3)]);
        assert_eq!(pc.boxes[0].name, None);
        assert_eq!(pc.boxes[2].name.as_deref(), Some("Legends"));
        assert_eq!(pc.boxes[1].wallpaper, DEFAULT_WALLPAPER);
        assert_eq!(pc.boxes[0].first_free(), Some(1));
        let eevee = pc.get(PcPosition::new(0, 7)).unwrap();
        assert_eq!(
            (eevee.species.as_str(), eevee.level),
            ("cobblemon:eevee", 9)
        );
        assert_eq!(pc.position(3), Some(PcPosition::new(2, 29)));
        assert!(pc.get(PcPosition::new(9, 0)).is_none() && pc.position(4).is_none());
        // A box sent again replaces what the client held of it.
        let mut storage = storage;
        let again = set_box(0, "Starters", &[(3, compound(1, "pikachu", 6))]);
        assert_eq!(storage.read(SET_PC_BOX, &again).unwrap(), Change::Stores);
        assert_eq!(held(&storage), [(0, 3, 1), (2, 29, 3)]);
        // A box past the last makes the PC longer; a PC the server never named takes none.
        let fifth = set_box(4, "", &[]);
        assert_eq!(storage.read(SET_PC_BOX, &fifth).unwrap(), Change::Stores);
        assert_eq!(storage.pc(STORE).unwrap().boxes.len(), 5);
        let mut other = Storage::new();
        assert_eq!(other.read(SET_PC_BOX, &fifth).unwrap(), Change::Nothing);
    }

    #[test]
    fn pokemon_are_set_moved_swapped_and_taken_out() {
        let mut storage = pc();
        // A Pokémon put in a slot, over whatever was there.
        let mut set = about(None, &[], Some(PcPosition::new(1, 4)));
        set.extend_from_slice(&compound(4, "ditto", 30));
        assert_eq!(storage.read(SET_PC_POKEMON, &set).unwrap(), Change::Stores);
        assert_eq!(
            held(&storage),
            [(0, 0, 1), (0, 7, 2), (1, 4, 4), (2, 29, 3)]
        );
        // Moved to an empty slot of another box.
        let moved = about(None, &[1], Some(PcPosition::new(2, 0)));
        assert_eq!(
            storage.read(MOVE_CLIENT_PC_POKEMON, &moved).unwrap(),
            Change::Stores
        );
        assert_eq!(
            held(&storage),
            [(0, 7, 2), (1, 4, 4), (2, 0, 1), (2, 29, 3)]
        );
        // Two that change places.
        let swapped = about(Some(false), &[2, 3], None);
        assert_eq!(
            storage.read(SWAP_CLIENT_POKEMON, &swapped).unwrap(),
            Change::Stores
        );
        assert_eq!(
            held(&storage),
            [(0, 7, 3), (1, 4, 4), (2, 0, 1), (2, 29, 2)]
        );
        // Taken out.
        let removed = about(Some(false), &[4], None);
        assert_eq!(
            storage.read(REMOVE_CLIENT_POKEMON, &removed).unwrap(),
            Change::Stores
        );
        assert_eq!(held(&storage), [(0, 7, 3), (2, 0, 1), (2, 29, 2)]);
        // The same messages about a party, a Pokémon that is not there or a slot there is not
        // change nothing.
        for (channel, payload) in [
            (REMOVE_CLIENT_POKEMON, about(Some(true), &[1], None)),
            (REMOVE_CLIENT_POKEMON, about(Some(false), &[4], None)),
            (SWAP_CLIENT_POKEMON, about(Some(true), &[1, 2], None)),
            (SWAP_CLIENT_POKEMON, about(Some(false), &[1, 9], None)),
            (
                MOVE_CLIENT_PC_POKEMON,
                about(None, &[9], Some(PcPosition::new(0, 0))),
            ),
        ] {
            assert_eq!(storage.read(channel, &payload).unwrap(), Change::Nothing);
        }
        let mut set = about(None, &[], Some(PcPosition::new(0, 30)));
        set.extend_from_slice(&compound(5, "abra", 3));
        assert_eq!(storage.read(SET_PC_POKEMON, &set).unwrap(), Change::Nothing);
        assert_eq!(held(&storage), [(0, 7, 3), (2, 0, 1), (2, 29, 2)]);
        // A move to a slot there is not would lose the Pokémon: an error, and it stays.
        let lost = about(None, &[1], Some(PcPosition::new(3, 0)));
        assert!(storage.read(MOVE_CLIENT_PC_POKEMON, &lost).is_err());
        assert_eq!(held(&storage), [(0, 7, 3), (2, 0, 1), (2, 29, 2)]);
    }

    #[test]
    fn the_server_opens_the_screen_closes_it_and_dresses_the_boxes() {
        let mut storage = pc();
        let open = |open_on: Option<i32>, unseen: &[&str]| {
            let mut w = Writer::new();
            w.uuid(STORE);
            w.bool(open_on.is_some());
            if let Some(number) = open_on {
                w.i32(number);
            }
            w.varint(unseen.len() as i32);
            for id in unseen {
                w.string(id);
            }
            w.data
        };
        assert_eq!(
            storage.read(OPEN_PC, &open(None, &[])).unwrap(),
            Change::Open {
                store: STORE,
                open_on: None,
                unseen_wallpapers: Vec::new()
            }
        );
        assert_eq!(
            storage.read(OPEN_PC, &open(Some(2), &["a:b.png"])).unwrap(),
            Change::Open {
                store: STORE,
                open_on: Some(2),
                unseen_wallpapers: vec!["a:b.png".into()]
            }
        );
        // A PC the server never named opens nothing.
        let mut unknown = open(None, &[]);
        unknown[0] ^= 1;
        assert_eq!(storage.read(OPEN_PC, &unknown).unwrap(), Change::Nothing);
        let mut close = vec![1];
        close.extend_from_slice(&STORE.to_be_bytes());
        assert_eq!(
            storage.read(CLOSE_PC, &close).unwrap(),
            Change::Close { store: Some(STORE) }
        );
        assert_eq!(
            storage.read(CLOSE_PC, &[0]).unwrap(),
            Change::Close { store: None }
        );
        // A box's name and wallpaper; a blank name is none.
        let named = |number: u16, value: &str| {
            let mut w = Writer::new();
            w.uuid(STORE);
            w.u16(number);
            w.string(value);
            w.data
        };
        assert_eq!(
            storage.read(RENAME_PC_BOX, &named(1, "Shinies")).unwrap(),
            Change::Stores
        );
        assert_eq!(
            storage
                .read(CHANGE_PC_BOX_WALLPAPER, &named(1, "a:forest.png"))
                .unwrap(),
            Change::Stores
        );
        let second = &storage.pc(STORE).unwrap().boxes[1];
        assert_eq!(
            (second.name.as_deref(), second.wallpaper.as_str()),
            (Some("Shinies"), "a:forest.png")
        );
        storage.read(RENAME_PC_BOX, &named(1, " ")).unwrap();
        assert_eq!(storage.pc(STORE).unwrap().boxes[1].name, None);
        assert_eq!(
            storage.read(RENAME_PC_BOX, &named(7, "None")).unwrap(),
            Change::Nothing
        );
        // The wallpapers a box may wear: none said, then the server's list, then one more.
        assert!(storage.wallpapers().is_none());
        assert_eq!(
            storage.read(REQUEST_PC_BOX_WALLPAPERS, &[]).unwrap(),
            Change::WallpapersAsked
        );
        let mut list = Writer::new();
        list.varint(1);
        list.string("a:b.png");
        storage.read(SET_PC_BOX_WALLPAPERS, &list.data).unwrap();
        let mut one = Writer::new();
        one.string("a:c.png");
        storage.read(UNLOCK_PC_BOX_WALLPAPER, &one.data).unwrap();
        storage.read(UNLOCK_PC_BOX_WALLPAPER, &one.data).unwrap();
        assert_eq!(storage.wallpapers().unwrap(), ["a:b.png", "a:c.png"]);
    }

    #[test]
    fn a_message_cut_short_or_too_long_is_an_error() {
        let mut storage = pc();
        let whole = set_box(1, "", &[(0, compound(8, "abra", 3))]);
        assert!(storage.read(SET_PC_BOX, &whole[..whole.len() - 1]).is_err());
        let mut long = whole.clone();
        long.push(0);
        assert!(storage.read(SET_PC_BOX, &long).is_err());
        // A Pokémon's length that says more than its compound takes.
        let mut padded = compound(8, "abra", 3);
        padded.push(0);
        assert!(
            storage
                .read(SET_PC_BOX, &set_box(1, "", &[(0, padded)]))
                .is_err()
        );
        assert!(
            storage
                .read(SET_PC_BOX, &set_box(1, "", &[(30, compound(8, "abra", 3))]))
                .is_err()
        );
        assert!(storage.read(INITIALIZE_PC, &initialize(3)[..18]).is_err());
        assert!(storage.read("cobblemon:initialize_party", &[]).is_err());
        // None of it touched what the PC held.
        assert_eq!(held(&storage), [(0, 0, 1), (0, 7, 2), (2, 29, 3)]);
    }

    #[test]
    fn a_request_is_written_as_its_packet_is() {
        let at = PcPosition::new(2, 29);
        let to = PcPosition::new(0x0102, 3);
        let id = |n: u8| -> Vec<u8> { u128::from(n).to_be_bytes().to_vec() };
        let bytes = |parts: &[&[u8]]| -> Vec<u8> { parts.concat() };
        let cases: Vec<(Request, &str, Vec<u8>)> = vec![
            (
                Request::MovePc {
                    pokemon: 7,
                    from: at,
                    to,
                },
                "cobblemon:move_pc_pokemon",
                bytes(&[&id(7), &[0, 2, 29], &[1, 2, 3]]),
            ),
            (
                Request::SwapPc {
                    first: (7, at),
                    second: (8, to),
                },
                "cobblemon:swap_pc_pokemon",
                bytes(&[&id(7), &[0, 2, 29], &id(8), &[1, 2, 3]]),
            ),
            (
                Request::PcToParty {
                    pokemon: 7,
                    from: at,
                    to: Some(5),
                },
                "cobblemon:move_pc_pokemon_to_party",
                bytes(&[&id(7), &[0, 2, 29], &[1, 0, 5]]),
            ),
            (
                Request::PcToParty {
                    pokemon: 7,
                    from: at,
                    to: None,
                },
                "cobblemon:move_pc_pokemon_to_party",
                bytes(&[&id(7), &[0, 2, 29], &[0]]),
            ),
            (
                Request::PartyToPc {
                    pokemon: 7,
                    from: 4,
                    to: Some(to),
                },
                "cobblemon:move_party_pokemon_to_pc",
                bytes(&[&id(7), &[0, 4], &[1, 1, 2, 3]]),
            ),
            (
                Request::SwapPcParty {
                    party: (7, 4),
                    pc: (8, at),
                },
                "cobblemon:swap_pc_party_pokemon",
                bytes(&[&id(7), &[0, 4], &id(8), &[0, 2, 29]]),
            ),
            (
                Request::MoveParty {
                    pokemon: 7,
                    from: 0,
                    to: 5,
                },
                "cobblemon:move_party_pokemon",
                bytes(&[&id(7), &[0, 0], &[0, 5]]),
            ),
            (
                Request::SwapParty {
                    first: (7, 0),
                    second: (8, 5),
                },
                "cobblemon:swap_party_pokemon",
                bytes(&[&id(7), &[0, 0], &id(8), &[0, 5]]),
            ),
            (
                Request::ReleasePc { pokemon: 7, at },
                "cobblemon:release_pc_pokemon",
                bytes(&[&id(7), &[0, 2, 29]]),
            ),
            (
                Request::ReleaseParty {
                    pokemon: 7,
                    slot: 3,
                },
                "cobblemon:release_party_pokemon",
                bytes(&[&id(7), &[0, 3]]),
            ),
            (
                Request::Unlink,
                "cobblemon:unlink_player_from_pc",
                Vec::new(),
            ),
            (
                Request::RenameBox {
                    store: 9,
                    pc_box: 2,
                    name: "Été".into(),
                },
                "cobblemon:request_rename_pc_box",
                bytes(&[&id(9), &[0, 2], &[5], "Été".as_bytes()]),
            ),
            (
                Request::ChangeWallpaper {
                    store: 9,
                    pc_box: 2,
                    wallpaper: "a:b".into(),
                    alt: Some("a:c".into()),
                },
                "cobblemon:request_change_pc_box_wallpaper",
                bytes(&[&id(9), &[0, 2], &[3], b"a:b", &[1, 3], b"a:c"]),
            ),
            (
                Request::SortBox {
                    store: 9,
                    pc_box: 2,
                    mode: SortMode::PokedexNumber,
                    descending: true,
                },
                "cobblemon:sort_pc_box",
                bytes(&[&id(9), &[0, 2], &[3, 1]]),
            ),
            (
                Request::Wallpapers(vec!["a:b".into(), "a:c".into()]),
                "cobblemon:pc_box_wallpapers",
                bytes(&[&[2, 3], b"a:b", &[3], b"a:c"]),
            ),
            (
                Request::WallpapersSeen(Vec::new()),
                "cobblemon:mark_pc_box_wallpapers_seen",
                vec![0],
            ),
        ];
        for (request, channel, payload) in cases {
            assert_eq!(request.channel(), channel, "{request:?}");
            assert_eq!(request.payload(), payload, "{request:?}");
        }
        assert_eq!(
            SortMode::ALL.map(SortMode::name),
            ["name", "level", "type", "pokedex_number", "gender"]
        );
        assert_eq!(SortMode::ALL.map(|mode| mode as u8), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_click_takes_a_pokemon_in_hand_then_puts_it_where_the_next_one_lands() {
        let around = Around {
            party_count: 2,
            party_free: Some(2),
            box_free: Some(PcPosition::new(1, 0)),
            keep_one_in_party: true,
        };
        let (a, b) = (PcPosition::new(1, 4), PcPosition::new(1, 9));
        let ask = |request| Click::Ask {
            request,
            dropped: true,
        };
        // Nothing in hand: a Pokémon is taken, an empty slot does nothing.
        assert_eq!(
            slot_click(None, Place::Pc(a), Some(7), false, around),
            Click::Grab
        );
        assert_eq!(
            slot_click(None, Place::Pc(a), None, false, around),
            Click::Nothing
        );
        assert_eq!(
            slot_click(None, Place::Party(0), Some(7), false, around),
            Click::Grab
        );
        // In hand: its own slot puts it back, an empty one moves it, a taken one swaps.
        let held = Some((Place::Pc(a), 7));
        assert_eq!(
            slot_click(held, Place::Pc(a), Some(7), false, around),
            Click::PutBack
        );
        assert_eq!(
            slot_click(held, Place::Pc(b), None, false, around),
            ask(Request::MovePc {
                pokemon: 7,
                from: a,
                to: b
            })
        );
        assert_eq!(
            slot_click(held, Place::Pc(b), Some(8), false, around),
            ask(Request::SwapPc {
                first: (8, b),
                second: (7, a)
            })
        );
        assert_eq!(
            slot_click(held, Place::Party(3), None, false, around),
            ask(Request::PcToParty {
                pokemon: 7,
                from: a,
                to: Some(3)
            })
        );
        assert_eq!(
            slot_click(held, Place::Party(1), Some(8), false, around),
            ask(Request::SwapPcParty {
                party: (8, 1),
                pc: (7, a)
            })
        );
        // From the party.
        let held = Some((Place::Party(1), 7));
        assert_eq!(
            slot_click(held, Place::Pc(b), None, false, around),
            ask(Request::PartyToPc {
                pokemon: 7,
                from: 1,
                to: Some(b)
            })
        );
        assert_eq!(
            slot_click(held, Place::Pc(b), Some(8), false, around),
            ask(Request::SwapPcParty {
                party: (7, 1),
                pc: (8, b)
            })
        );
        assert_eq!(
            slot_click(held, Place::Party(4), None, false, around),
            ask(Request::MoveParty {
                pokemon: 7,
                from: 1,
                to: 4
            })
        );
        assert_eq!(
            slot_click(held, Place::Party(0), Some(8), false, around),
            ask(Request::SwapParty {
                first: (8, 0),
                second: (7, 1)
            })
        );
    }

    #[test]
    fn shift_sends_a_pokemon_across_and_the_party_keeps_its_last() {
        let around = Around {
            party_count: 2,
            party_free: Some(2),
            box_free: Some(PcPosition::new(1, 0)),
            keep_one_in_party: true,
        };
        let a = PcPosition::new(1, 4);
        let across = |request| Click::Ask {
            request,
            dropped: false,
        };
        assert_eq!(
            slot_click(None, Place::Pc(a), Some(7), true, around),
            across(Request::PcToParty {
                pokemon: 7,
                from: a,
                to: Some(2)
            })
        );
        assert_eq!(
            slot_click(None, Place::Party(1), Some(7), true, around),
            across(Request::PartyToPc {
                pokemon: 7,
                from: 1,
                to: Some(PcPosition::new(1, 0))
            })
        );
        // With no room on the other side the Pokémon is taken in hand as without Shift.
        let full = Around {
            party_free: None,
            box_free: None,
            ..around
        };
        assert_eq!(
            slot_click(None, Place::Pc(a), Some(7), true, full),
            Click::Grab
        );
        assert_eq!(
            slot_click(None, Place::Party(1), Some(7), true, full),
            Click::Grab
        );
        // The party's last Pokémon: not sent across, not put in an empty slot of the PC, but
        // swapped for one of the PC's all the same. A server that lets a party be emptied takes it.
        let last = Around {
            party_count: 1,
            ..around
        };
        let held = Some((Place::Party(0), 7));
        assert_eq!(
            slot_click(None, Place::Party(0), Some(7), true, last),
            Click::Nothing
        );
        assert_eq!(
            slot_click(held, Place::Pc(a), None, false, last),
            Click::Nothing
        );
        assert!(matches!(
            slot_click(held, Place::Pc(a), Some(8), false, last),
            Click::Ask {
                request: Request::SwapPcParty { .. },
                ..
            }
        ));
        let lenient = Around {
            keep_one_in_party: false,
            ..last
        };
        assert!(matches!(
            slot_click(held, Place::Pc(a), None, false, lenient),
            Click::Ask {
                request: Request::PartyToPc { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_pokemon_in_hand_is_released_unless_it_is_the_party_s_last() {
        let at = PcPosition::new(0, 3);
        assert!(can_release(Place::Pc(at), 0));
        assert!(can_release(Place::Party(1), 2));
        assert!(!can_release(Place::Party(0), 1));
        assert_eq!(
            release(Place::Pc(at), 7),
            Request::ReleasePc { pokemon: 7, at }
        );
        assert_eq!(
            release(Place::Party(4), 7),
            Request::ReleaseParty {
                pokemon: 7,
                slot: 4
            }
        );
    }

    #[test]
    fn the_boxes_go_round() {
        assert_eq!(box_after(0, 1, 30), 1);
        assert_eq!(box_after(29, 1, 30), 0);
        assert_eq!(box_after(0, -1, 30), 29);
        assert_eq!(box_after(5, -1, 30), 4);
        // The wheel turns by more than one: past either end it stops at that end's other side.
        assert_eq!(box_after(28, 3, 30), 0);
        assert_eq!(box_after(1, -3, 30), 29);
        assert_eq!(box_after(0, 1, 1), 0);
        assert_eq!(box_after(0, 1, 0), 0);
    }
}
