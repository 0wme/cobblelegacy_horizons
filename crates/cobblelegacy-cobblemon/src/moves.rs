// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The moves a Cobblemon 1.8.1 server knows, as it tells them to a client that logs in.
//!
//! A battle's request names a move by its Showdown id and says what is left of it (`InBattleMove`), not
//! what the move is. Its type, which tints its tile and picks its icon, and whether it is physical,
//! special or a status move come from the registry the server sends once at login
//! (`MovesRegistrySyncPacket`: one `MoveTemplate` an entry, which the client's `Moves` keeps by name).
//! [`Moves`] is that registry.
//!
//! The packet is a `DataRegistrySyncPacket`: the entries go to a buffer of their own, a VarInt count
//! first (`writeCollection` on a `RegistryFriendlyByteBuf`), and the payload is that buffer's length in
//! four bytes, then the buffer.
//!
//! A type travels by name. What the screens draw of it is not sent: its colour and the cell of its icon
//! are `ElementalTypes`' own ([`type_hue`], [`type_icon`]).
//!
//! Not worked out here: the type a move takes from the Pokémon that uses it
//! (`MoveTemplate.getEffectiveElementalType`: Hidden Power, Pixilate, Normalize...), which is the one
//! the battle screen tints a tile with.
use crate::battle::MoveTarget;
use crate::wire::string;
use horizon_protocol::{Error, Result, buf::Reader};
use std::collections::HashMap;

/// `MovesRegistrySyncPacket.ID`.
pub const MOVES_SYNC: &str = "cobblemon:moves_sync";

/// `DamageCategories`: how a move does what it does. In the order of their cells in
/// `textures/gui/categories.png`, top to bottom (`MoveCategoryIcon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageCategory {
    Physical,
    Special,
    Status,
}

impl DamageCategory {
    /// `DamageCategories.getOrException`: by name, whatever its case.
    fn named(name: &str) -> Result<Self> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "physical" => Self::Physical,
            "special" => Self::Special,
            "status" => Self::Status,
            _ => return Err(Error::new(format!("a move of category {name}"))),
        })
    }
}

/// `ElementalTypes`: each type's Showdown id, its `hue` and its `textureXMultiplier`, in the order they
/// are registered.
const TYPES: [(&str, u32, u32); 18] = [
    ("normal", 0xE8E8DA, 0),
    ("fire", 0xFF6E21, 1),
    ("water", 0x3FA5FF, 2),
    ("grass", 0x62D14F, 3),
    ("electric", 0xFFD314, 4),
    ("ice", 0x54F2F2, 5),
    ("fighting", 0xEF565D, 6),
    ("poison", 0xD651FF, 7),
    ("ground", 0xF4A453, 8),
    ("flying", 0xB8B2FF, 9),
    ("psychic", 0xFF5E9E, 10),
    ("bug", 0xD3D319, 11),
    ("rock", 0xB7A16E, 12),
    ("ghost", 0x9C80F7, 13),
    ("dragon", 0x7580FF, 14),
    ("dark", 0x587DA0, 15),
    ("steel", 0xABD1F4, 16),
    ("fairy", 0xFF7FE5, 17),
];

/// `ElementalTypes.get`: by name, whatever its case.
fn elemental_type(name: &str) -> Option<&'static (&'static str, u32, u32)> {
    TYPES
        .iter()
        .find(|(known, ..)| known.eq_ignore_ascii_case(name))
}

/// `ElementalType.hue`: the colour of the type `name` as 0xRRGGBB, which tints a move's tile
/// (`BattleMoveSelection.MoveTile`). None for a type Cobblemon does not have.
pub fn type_hue(name: &str) -> Option<u32> {
    elemental_type(name).map(|&(_, hue, _)| hue)
}

/// `ElementalType.textureXMultiplier`: the cell of the type `name` in `textures/gui/types.png` and
/// `types_small.png`, a row of eighteen square icons counted from the left (`TypeIcon`). None for a
/// type Cobblemon does not have.
pub fn type_icon(name: &str) -> Option<u32> {
    elemental_type(name).map(|&(.., cell)| cell)
}

/// `MoveTemplate`: what a move is, whichever Pokémon knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveTemplate {
    /// Showdown's id (`electroball`), which a battle's request names the move by.
    pub name: String,
    /// Showdown's number for the move, which several can share (every G-Max move is 1000).
    pub num: i32,
    /// The type's Showdown id (`electric`): see [`type_hue`] and [`type_icon`]. A name `ElementalTypes`
    /// does not have is kept as sent, where Cobblemon's client gives up on the packet.
    pub elemental_type: String,
    pub category: DamageCategory,
    /// The base power: 0 for a status move, and for a move whose power the battle works out.
    pub power: f64,
    pub target: MoveTarget,
    /// Out of 100; -1 for a move that does not check its accuracy.
    pub accuracy: f64,
    /// Before any PP Up.
    pub pp: u32,
    pub priority: i32,
    /// Showdown's `critRatio`: 1 for most moves, more for one that lands critical hits more often.
    pub crit_ratio: f64,
    /// The chance of each of its secondary effects, out of 100.
    pub effect_chances: Vec<f64>,
    /// Its weight when a Pokémon's moves are drawn (`move_weights.json`, 50 for a move not in it).
    pub weight: f32,
}

impl MoveTemplate {
    /// `MovesRegistrySyncPacket.decodeEntry`.
    fn read(r: &mut Reader) -> Result<Self> {
        let name = string(r)?;
        Ok(Self {
            num: r.i32()?,
            elemental_type: string(r)?,
            category: DamageCategory::named(&string(r)?)?,
            power: r.f64()?,
            target: MoveTarget::read(r)?,
            accuracy: r.f64()?,
            pp: {
                let pp = r.i32()?;
                u32::try_from(pp).map_err(|_| Error::new(format!("{name} with {pp} PP")))?
            },
            priority: r.i32()?,
            crit_ratio: r.f64()?,
            // `repeat`: a count below one is no chance at all.
            effect_chances: (0..r.i32()?).map(|_| r.f64()).collect::<Result<_>>()?,
            weight: r.f32()?,
            name,
        })
    }
}

/// The client's `Moves`: the templates the server sent, in its order, found by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moves {
    moves: Vec<MoveTemplate>,
    /// Where each name's template is in `moves`.
    by_name: HashMap<String, usize>,
}

impl Moves {
    /// The payload of a [`MOVES_SYNC`] message, whole: bytes left over are an error, in the entries'
    /// buffer as after it, so a layout that drifted is noticed instead of misread.
    pub fn read(payload: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(payload);
        // `DataRegistrySyncPacket.encode`: the length in bytes of the entries' buffer, then the buffer.
        let size = r.i32()?;
        if usize::try_from(size).ok() != Some(r.remaining()) {
            return Err(Error::new(format!(
                "{MOVES_SYNC}: {size} bytes of entries announced, {} sent",
                r.remaining()
            )));
        }
        let mut registry = Self::default();
        for _ in 0..r.count(r.remaining())? {
            registry.register(MoveTemplate::read(r)?);
        }
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{MOVES_SYNC}: {} bytes left unread",
                r.remaining()
            )));
        }
        Ok(registry)
    }

    /// `Moves.register`: a name sent twice keeps its place and takes the later template.
    fn register(&mut self, template: MoveTemplate) {
        match self.by_name.get(&template.name) {
            Some(&at) => self.moves[at] = template,
            None => {
                self.by_name.insert(template.name.clone(), self.moves.len());
                self.moves.push(template);
            }
        }
    }

    /// `Moves.getByName`: the move of this Showdown id, whatever the id's case. A move a battle offers
    /// that is not here is drawn as a Normal status move (`getByNameOrDummy`).
    pub fn get(&self, id: &str) -> Option<&MoveTemplate> {
        let at = *self.by_name.get(id.to_lowercase().as_str())?;
        self.moves.get(at)
    }

    pub fn len(&self) -> usize {
        self.moves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Every move, in the order the server sent them.
    pub fn iter(&self) -> impl Iterator<Item = &MoveTemplate> {
        self.moves.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_protocol::buf::Writer;

    /// The registry a Cobblemon 1.8.1 server sent a player as they logged in: the whole payload of its
    /// one `cobblemon:moves_sync` message.
    const SYNC: &[u8] = include_bytes!("../tests/fixtures/moves_sync.bin");

    /// The entries' buffer behind the length `DataRegistrySyncPacket.encode` writes before it.
    fn framed(entries: &[u8]) -> Vec<u8> {
        let mut payload = (entries.len() as i32).to_be_bytes().to_vec();
        payload.extend_from_slice(entries);
        payload
    }

    /// An entry as `MovesRegistrySyncPacket.encodeEntry` writes it, for what the capture does not hold.
    fn entry(name: &str, kind: &str, category: &str, [target, pp, chances]: [i32; 3]) -> Vec<u8> {
        let mut w = Writer::new();
        w.string(name);
        w.i32(-1);
        w.string(kind);
        w.string(category);
        w.f64(60.0);
        w.i32(target);
        w.f64(100.0);
        w.i32(pp);
        w.i32(0);
        w.f64(1.0);
        w.i32(chances);
        w.f32(50.0);
        w.data
    }

    /// A payload of these entries.
    fn sync_of(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut buffer = vec![entries.len() as u8];
        buffer.extend(entries.concat());
        framed(&buffer)
    }

    #[test]
    fn a_login_s_registry_is_read_to_its_last_byte() {
        let moves = Moves::read(SYNC).unwrap();
        // What the packet says of itself: the length of its entries' buffer, then how many they are.
        let r = &mut Reader::new(SYNC);
        assert_eq!(r.i32().unwrap(), 70_444);
        assert_eq!(r.varint().unwrap(), 936);
        assert_eq!(moves.len(), 936);
        // In the server's order, Showdown's.
        let names: Vec<_> = moves.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            (names[0], names[1], names[935]),
            ("10000000voltthunderbolt", "absorb", "zippyzap")
        );

        // What a battle's screen asks of the moves of the captured battles.
        let seen = |id: &str| {
            let m = moves.get(id).unwrap();
            (m.elemental_type.as_str(), m.category, m.power)
        };
        assert_eq!(
            seen("electroball"),
            ("electric", DamageCategory::Special, 0.0)
        );
        assert_eq!(seen("tackle"), ("normal", DamageCategory::Physical, 40.0));
        assert_eq!(seen("growl"), ("normal", DamageCategory::Status, 0.0));
        assert_eq!(seen("firespin"), ("fire", DamageCategory::Special, 35.0));
        // By id as `Moves.getByName` finds it, and nothing for a move the server does not have.
        assert_eq!(moves.get("ElectroBall"), moves.get("electroball"));
        assert!(moves.get("electroballs").is_none());

        // Every field of one move, then what tells the other fields apart elsewhere.
        assert_eq!(
            moves.get("firefang"),
            Some(&MoveTemplate {
                name: "firefang".into(),
                num: 424,
                elemental_type: "fire".into(),
                category: DamageCategory::Physical,
                power: 65.0,
                target: MoveTarget::Normal,
                accuracy: 95.0,
                pp: 15,
                priority: 0,
                crit_ratio: 1.0,
                effect_chances: vec![10.0, 10.0],
                weight: 50.0,
            })
        );
        let growl = moves.get("growl").unwrap();
        assert_eq!(
            (growl.num, growl.pp, growl.target),
            (45, 40, MoveTarget::AllAdjacentFoes)
        );
        // Trick Room goes last and cannot miss.
        let trick_room = moves.get("trickroom").unwrap();
        assert_eq!(
            (trick_room.priority, trick_room.accuracy, trick_room.target),
            (-7, -1.0, MoveTarget::All)
        );
        assert_eq!(moves.get("quickattack").unwrap().priority, 1);
        assert_eq!(
            moves.get("10000000voltthunderbolt").unwrap().crit_ratio,
            3.0
        );
        assert_eq!(moves.get("acid").unwrap().effect_chances, [10.0]);
        assert_eq!(moves.get("terablast").unwrap().weight, 0.0);
    }

    #[test]
    fn a_payload_cut_short_or_with_a_byte_too_many_is_an_error() {
        for cut in [0, 3, 4, 5, 60, SYNC.len() - 1] {
            assert!(Moves::read(&SYNC[..cut]).is_err(), "cut at {cut}");
        }
        let e = Moves::read(&[SYNC, &[0]].concat()).unwrap_err();
        assert!(
            e.0.contains("70444 bytes of entries announced, 70445 sent"),
            "{}",
            e.0
        );
        // The same behind a length that agrees: the entries end early, or leave a byte.
        let entries = &SYNC[4..];
        assert!(Moves::read(&framed(entries)).unwrap() == Moves::read(SYNC).unwrap());
        for cut in [0, 1, 2, 60, entries.len() - 1] {
            assert!(
                Moves::read(&framed(&entries[..cut])).is_err(),
                "cut at {cut}"
            );
        }
        let e = Moves::read(&framed(&[entries, &[0]].concat())).unwrap_err();
        assert!(e.0.contains("1 bytes left unread"), "{}", e.0);
    }

    #[test]
    fn an_entry_is_taken_or_refused_as_cobblemon_s_client_does() {
        // A category whatever its case, a count of chances below zero as none; and a type Cobblemon
        // does not have, kept by name with nothing to draw it with.
        let moves = Moves::read(&sync_of(&[entry(
            "shadowrush",
            "shadow",
            "PHYSICAL",
            [4, 5, -1],
        )]))
        .unwrap();
        let rush = moves.get("shadowrush").unwrap();
        assert_eq!(
            (rush.category, rush.target, rush.pp),
            (DamageCategory::Physical, MoveTarget::Myself, 5)
        );
        assert!(rush.effect_chances.is_empty());
        assert_eq!(
            (
                type_hue(&rush.elemental_type),
                type_icon(&rush.elemental_type)
            ),
            (None, None)
        );
        // A name sent twice keeps its place, with what came last.
        let moves = Moves::read(&sync_of(&[
            entry("pound", "normal", "physical", [5, 35, 0]),
            entry("growl", "normal", "status", [3, 40, 0]),
            entry("pound", "fighting", "special", [5, 35, 0]),
        ]))
        .unwrap();
        let order: Vec<_> = moves
            .iter()
            .map(|m| (m.name.as_str(), m.elemental_type.as_str()))
            .collect();
        assert_eq!(order, [("pound", "fighting"), ("growl", "normal")]);
        assert_eq!(
            moves.get("pound").unwrap().category,
            DamageCategory::Special
        );
        // A category or a target outside Cobblemon's, which its client throws on; and PP below zero.
        let refused = |bytes: Vec<u8>| Moves::read(&sync_of(&[bytes])).unwrap_err().0;
        let e = refused(entry("pound", "normal", "magical", [5, 35, 0]));
        assert!(e.contains("category magical"), "{e}");
        let e = refused(entry("pound", "normal", "physical", [15, 35, 0]));
        assert!(e.contains("move target ordinal 15"), "{e}");
        let e = refused(entry("pound", "normal", "physical", [5, -1, 0]));
        assert!(e.contains("pound with -1 PP"), "{e}");
    }

    #[test]
    fn every_type_a_move_has_is_one_the_screens_can_draw() {
        let moves = Moves::read(SYNC).unwrap();
        let mut cells = Vec::new();
        for m in moves.iter() {
            let (hue, cell) = (type_hue(&m.elemental_type), type_icon(&m.elemental_type));
            assert!(
                hue.is_some() && cell.is_some(),
                "{}: {}",
                m.name,
                m.elemental_type
            );
            cells.extend(cell);
        }
        // The moves are of all eighteen types, each with a cell of its own in the sheet's eighteen.
        cells.sort_unstable();
        cells.dedup();
        assert_eq!(cells, (0..18).collect::<Vec<u32>>());
        // `ElementalTypes`' own values.
        let drawn = |name: &str| (type_hue(name), type_icon(name));
        assert_eq!(drawn("normal"), (Some(0xE8E8DA), Some(0)));
        assert_eq!(drawn("fire"), (Some(0xFF6E21), Some(1)));
        assert_eq!(drawn("electric"), (Some(0xFFD314), Some(4)));
        assert_eq!(drawn("fairy"), (Some(0xFF7FE5), Some(17)));
        // By name whatever its case, as `ElementalTypes.get` finds it.
        assert_eq!(drawn("Electric"), drawn("electric"));
        assert_eq!(drawn("shadow"), (None, None));
    }
}
