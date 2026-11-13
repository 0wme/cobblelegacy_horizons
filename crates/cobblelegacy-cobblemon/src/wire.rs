// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cobblemon's plugin-channel payloads, as a 1.21.1 server writes them.
//!
//! A Pokémon never arrives as vanilla's add_entity: Cobblemon sends `cobblemon:spawn_pokemon_entity`, its own
//! fields followed by the bytes of vanilla's `ClientboundAddEntityPacket` (`SpawnPokemonPacket` and
//! `SpawnExtraDataEntityPacket.encode`). Later movement and metadata come as vanilla packets for that
//! entity id.
//!
//! Cobblemon's own buffer helpers differ from vanilla's: `writeEnumConstant` is an Int ordinal and
//! `writeIdentifier` a plain string, while `writeNullable`, `writeCollection`, `writeMap` and `writeUUID`
//! resolve to `FriendlyByteBuf`'s members (a Boolean, VarInt counts, two Longs).
use horizon_protocol::{Error, Result, buf::Reader};
use horizon_world::nbt::Tag;

/// `SpawnPokemonPacket.ID`.
pub const SPAWN_POKEMON: &str = "cobblemon:spawn_pokemon_entity";

/// `Gender`, by ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gender {
    Male,
    Female,
    Genderless,
}

/// `PoseType`, by ordinal: which of a poser's poses the entity is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoseType {
    Stand,
    Walk,
    Sleep,
    Hover,
    Fly,
    Float,
    Swim,
    Glide,
    ShoulderLeft,
    ShoulderRight,
    Profile,
    Portrait,
    Open,
    None,
}

impl PoseType {
    pub const ALL: [Self; 14] = [
        Self::Stand,
        Self::Walk,
        Self::Sleep,
        Self::Hover,
        Self::Fly,
        Self::Float,
        Self::Swim,
        Self::Glide,
        Self::ShoulderLeft,
        Self::ShoulderRight,
        Self::Profile,
        Self::Portrait,
        Self::Open,
        Self::None,
    ];
}

/// Vanilla 1.21.1's `ClientboundAddEntityPacket`, as `write` lays it out. `kind` is the server's own number
/// for the entity type (a mod's type has no vanilla number), so nothing here interprets it.
#[derive(Clone, Debug, PartialEq)]
pub struct AddEntity {
    pub id: i32,
    pub uuid: u128,
    pub kind: i32,
    pub position: [f64; 3],
    /// Degrees.
    pub pitch: f32,
    pub yaw: f32,
    pub head_yaw: f32,
    pub data: i32,
    /// Blocks per tick.
    pub velocity: [f64; 3],
}

impl AddEntity {
    pub(crate) fn read(r: &mut Reader) -> Result<Self> {
        let id = r.varint()?;
        let uuid = r.uuid()?;
        let kind = r.varint()?;
        let position = [r.f64()?, r.f64()?, r.f64()?];
        let angle = |r: &mut Reader| Ok::<_, Error>(f32::from(r.i8()?) * 360.0 / 256.0);
        let pitch = angle(r)?;
        let yaw = angle(r)?;
        let head_yaw = angle(r)?;
        let data = r.varint()?;
        let speed = |r: &mut Reader| Ok::<_, Error>(f64::from(r.i16()?) / 8000.0);
        let velocity = [speed(r)?, speed(r)?, speed(r)?];
        Ok(Self {
            id,
            uuid,
            kind,
            position,
            pitch,
            yaw,
            head_yaw,
            data,
            velocity,
        })
    }
}

/// `SpawnPokemonPacket`: a Pokémon entering the client's view.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnPokemon {
    pub owner: Option<u128>,
    pub pokemon_uuid: u128,
    pub storage: Option<u128>,
    pub scale_modifier: f32,
    /// A species id such as `cobblemon:pikachu`.
    pub species: String,
    pub gender: Gender,
    pub shiny: bool,
    /// The form's showdown id, `normal` for the standard one.
    pub form: String,
    /// What picks the model variation: `shiny`, `male`, `alolan`...
    pub aspects: Vec<String>,
    pub battle: Option<u128>,
    pub phasing_target: i32,
    pub beam_mode: i8,
    /// `PlatformType`'s ordinal (0 is none).
    pub platform: i32,
    /// A text component, as network NBT.
    pub nickname: Option<Tag>,
    pub mark: Option<String>,
    /// -1 when the server hides level labels.
    pub label_level: i32,
    pub pose: PoseType,
    pub unbattlable: bool,
    pub hide_label: bool,
    pub caught_ball: String,
    pub spawn_yaw: f32,
    pub friendship: i32,
    pub freeze_frame: f32,
    pub tick_spawned: i32,
    /// `RidingStat` ordinals and their boosts.
    pub ride_boosts: Vec<(i32, f32)>,
    pub ride_stamina: f32,
    pub alpha: bool,
    pub silent: bool,
    pub riders: Vec<i32>,
    pub entity: AddEntity,
}

pub(crate) fn nullable<'a, T>(
    r: &mut Reader<'a>,
    read: impl FnOnce(&mut Reader<'a>) -> Result<T>,
) -> Result<Option<T>> {
    if r.bool()? {
        read(r).map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) fn string(r: &mut Reader) -> Result<String> {
    r.string(32767)
}

fn ordinal<T: Copy>(r: &mut Reader, values: &[T], what: &str) -> Result<T> {
    let i = r.i32()?;
    usize::try_from(i)
        .ok()
        .and_then(|i| values.get(i).copied())
        .ok_or_else(|| Error::new(format!("{what} ordinal {i}")))
}

impl SpawnPokemon {
    /// The payload of a [`SPAWN_POKEMON`] message, whole: leftover bytes are an error, so a layout that
    /// drifted is noticed instead of misread.
    pub fn read(payload: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(payload);
        let owner = nullable(r, Reader::uuid)?;
        let pokemon_uuid = r.uuid()?;
        let storage = nullable(r, Reader::uuid)?;
        let scale_modifier = r.f32()?;
        let species = string(r)?;
        let gender = ordinal(
            r,
            &[Gender::Male, Gender::Female, Gender::Genderless],
            "gender",
        )?;
        let shiny = r.bool()?;
        let form = string(r)?;
        let aspects = (0..r.count(4096)?)
            .map(|_| string(r))
            .collect::<Result<_>>()?;
        let battle = nullable(r, Reader::uuid)?;
        let phasing_target = r.i32()?;
        let beam_mode = r.i8()?;
        let platform = r.i32()?;
        let nickname = nullable(r, Reader::nbt)?;
        let mark = nullable(r, string)?;
        let label_level = r.i32()?;
        let pose = ordinal(r, &PoseType::ALL, "pose")?;
        let unbattlable = r.bool()?;
        let hide_label = r.bool()?;
        let caught_ball = string(r)?;
        let spawn_yaw = r.f32()?;
        let friendship = r.i32()?;
        let freeze_frame = r.f32()?;
        let tick_spawned = r.i32()?;
        let ride_boosts = (0..r.count(64)?)
            .map(|_| Ok((r.i32()?, r.f32()?)))
            .collect::<Result<_>>()?;
        let ride_stamina = r.f32()?;
        let alpha = r.bool()?;
        let silent = r.bool()?;
        let riders = (0..r.count(64)?).map(|_| r.i32()).collect::<Result<_>>()?;
        let entity = AddEntity::read(r)?;
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{SPAWN_POKEMON}: {} bytes left over",
                r.remaining()
            )));
        }
        Ok(Self {
            owner,
            pokemon_uuid,
            storage,
            scale_modifier,
            species,
            gender,
            shiny,
            form,
            aspects,
            battle,
            phasing_target,
            beam_mode,
            platform,
            nickname,
            mark,
            label_level,
            pose,
            unbattlable,
            hide_label,
            caught_ball,
            spawn_yaw,
            friendship,
            freeze_frame,
            tick_spawned,
            ride_boosts,
            ride_stamina,
            alpha,
            silent,
            riders,
            entity,
        })
    }
}

/// `ServerSettingsPacket.ID`.
pub const SERVER_SETTINGS: &str = "cobblemon:server_settings";

/// `ServerSettingsPacket`: what of the server's configuration its clients follow, sent as they join.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ServerSettings {
    pub prevent_complete_party_deposit: bool,
    /// Whether a Pokémon's label shows its level.
    pub display_entity_level_label: bool,
    /// Whether a Pokémon's label shows its name.
    pub display_entity_name_label: bool,
    pub max_pokemon_level: i32,
    pub max_pokemon_friendship: i32,
    pub max_dynamax_level: i32,
    pub unlock_all_move_dex_moves: bool,
    /// The least and the greatest `scaleModifier` of a Pokémon.
    pub intrinsic_size: [f32; 2],
}

impl ServerSettings {
    /// The payload of a [`SERVER_SETTINGS`] message, whole.
    pub fn read(payload: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(payload);
        let settings = Self {
            prevent_complete_party_deposit: r.bool()?,
            display_entity_level_label: r.bool()?,
            display_entity_name_label: r.bool()?,
            max_pokemon_level: r.i32()?,
            max_pokemon_friendship: r.i32()?,
            max_dynamax_level: r.i32()?,
            unlock_all_move_dex_moves: r.bool()?,
            intrinsic_size: [r.f32()?, r.f32()?],
        };
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{SERVER_SETTINGS}: {} bytes left over",
                r.remaining()
            )));
        }
        Ok(settings)
    }
}

/// `SetClientPlayerDataPacket.ID`.
pub const SET_CLIENT_PLAYERDATA: &str = "cobblemon:set_client_playerdata";

/// `PokedexEntryProgress`, by ordinal: what the player's Pokédex has of a species.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DexKnowledge {
    Unregistered,
    Seen,
    Owned,
}

/// A `SetClientPlayerDataPacket` of the player's Pokédex (`ClientPokedexManager`): for each species
/// it carries, the most the Pokédex has of any of its forms (`SpeciesDexRecord.getKnowledge`).
#[derive(Clone, Debug, PartialEq)]
pub struct PokedexData {
    /// Whether the species add to those known (`runIncremental`) or are all there is (`runAction`).
    pub incremental: bool,
    /// A species' identifier (`cobblemon:pikachu`) and what is known of it.
    pub species: Vec<(String, DexKnowledge)>,
}

impl PokedexData {
    /// The payload of a [`SET_CLIENT_PLAYERDATA`] message, whole. None for another kind of player
    /// data (`cobblemon:general`, `cobblemon:tm_moves`), which nothing here reads.
    pub fn read(payload: &[u8]) -> Result<Option<Self>> {
        const KNOWLEDGE: [DexKnowledge; 3] = [
            DexKnowledge::Unregistered,
            DexKnowledge::Seen,
            DexKnowledge::Owned,
        ];
        let r = &mut Reader::new(payload);
        // `PlayerInstancedDataStoreType.id`, as `writeResourceLocation` writes it.
        if string(r)? != "cobblemon:pokedex" {
            return Ok(None);
        }
        let incremental = r.bool()?;
        let mut species = Vec::new();
        for _ in 0..r.count(r.remaining())? {
            // `asIdentifierDefaultingNamespace`.
            let mut id = string(r)?;
            if !id.contains(':') {
                id.insert_str(0, "cobblemon:");
            }
            // `SpeciesDexRecord.encode`: aspects, the highest level seen, then its forms.
            for _ in 0..r.count(r.remaining())? {
                string(r)?;
            }
            r.i32()?;
            let mut knowledge = DexKnowledge::Unregistered;
            let forms = r.i32()?;
            for _ in 0..forms {
                // The form's name, then `FormDexRecord.encode`: genders, shiny states, a level.
                string(r)?;
                for _ in 0..r.count(r.remaining())? {
                    r.i32()?;
                }
                for _ in 0..r.count(r.remaining())? {
                    string(r)?;
                }
                r.i32()?;
                knowledge = knowledge.max(ordinal(r, &KNOWLEDGE, "Pokédex knowledge")?);
            }
            species.push((id, knowledge));
        }
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{SET_CLIENT_PLAYERDATA}: {} bytes left over",
                r.remaining()
            )));
        }
        Ok(Some(Self {
            incremental,
            species,
        }))
    }
}

/// What a `PokemonEntity` syncs after it appeared that vanilla's readers stop at: its data from the
/// first entry of a serializer of Cobblemon's on (`horizon_protocol::adapter::UNREAD_ENTITY_DATA`). A
/// field is `None` when the packet does not carry it: a server only sends what changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PokemonData {
    /// `PLATFORM_TYPE`: the raft under a Pokémon that stands on water, by ordinal (0 is none).
    pub platform: Option<i32>,
    /// `BEAM_MODE`: 1 thrown out in its Ball, 2 appearing where it stands, 3 called back, 0 neither.
    pub beam_mode: Option<i8>,
    /// `BATTLE_ID`: the battle it is in, if any.
    pub battle: Option<Option<u128>>,
    /// `ASPECTS`: what picks its model and texture (`shiny`, `sheared`...).
    pub aspects: Option<Vec<String>>,
    /// `DYING_EFFECTS_STARTED`: it has fainted, and plays its faint before it goes.
    pub dying: Option<bool>,
    /// `POSE_TYPE`: what it is doing, as `PokemonServerDelegate.updatePoseType` decides it each tick.
    pub pose: Option<PoseType>,
    /// `LABEL_LEVEL`.
    pub label_level: Option<i32>,
    /// `HIDE_LABEL`.
    pub hide_label: Option<bool>,
    /// `SCALE_MODIFIER`.
    pub scale_modifier: Option<f32>,
    /// `IS_ALPHA`.
    pub alpha: Option<bool>,
    /// The index reading stopped at, when it is before the end: an item in `SHOWN_HELD_ITEM`, whose
    /// components nothing here measures, or an index past those of Cobblemon 1.8.1.
    pub stopped_at: Option<u8>,
}

/// Vanilla 1.21.1's serializers by number (`EntityDataSerializers`' static block), for those a
/// `PokemonEntity` uses. There are 31; a mod's come after, numbered in the order mods register theirs,
/// so a number above tells nothing but "a mod's".
mod serializer {
    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const FLOAT: i32 = 3;
    pub const STRING: i32 = 4;
    pub const ITEM_STACK: i32 = 7;
    pub const BOOLEAN: i32 = 8;
    pub const OPTIONAL_UUID: i32 = 13;
    pub const VANILLA: i32 = 31;
}

impl PokemonData {
    /// `PokemonEntity.PLATFORM_TYPE`'s index, the first of a serializer of Cobblemon's: after Entity's 8
    /// entries, LivingEntity's 7, Mob's 1, AgeableMob's 1, TamableAnimal's 2 and the Pokémon's first 8.
    pub const FIRST: u8 = 27;

    /// The entries from the first one vanilla's readers do not know, as the server wrote them: for
    /// each an index, the serializer's number, the value, then 0xFF.
    ///
    /// Read by the entity's layout, an index giving its type: Cobblemon's serializers have no number of
    /// their own (see [`serializer`]). The numbers still have to agree with the layout, vanilla's where
    /// it says vanilla and a mod's where it says Cobblemon's: an entity laid out otherwise (another
    /// version of Cobblemon) is an error, not a misread.
    pub fn read_rest(rest: &[u8]) -> Result<Self> {
        use serializer::*;
        let r = &mut Reader::new(rest);
        let mut data = Self::default();
        loop {
            let index = r.u8()?;
            if index == 0xFF {
                break;
            }
            let wire = r.varint()?;
            // `None`: one of Cobblemon's own.
            let expected = match index {
                27 | 30 | 32 | 43 => None,
                28 => Some(BYTE),
                29 => Some(OPTIONAL_UUID),
                31 | 34..=36 | 41 | 46 => Some(BOOLEAN),
                33 | 38 => Some(INT),
                37 | 39 | 44 | 45 => Some(FLOAT),
                40 => Some(STRING),
                42 => Some(ITEM_STACK),
                _ => {
                    data.stopped_at = Some(index);
                    return Ok(data);
                }
            };
            if expected.map_or(wire < VANILLA, |vanilla| wire != vanilla) {
                return Err(Error::new(format!(
                    "a Pokémon's data {index} has serializer {wire}: not the layout of Cobblemon 1.8.1"
                )));
            }
            match index {
                // `PlatformTypeDataSerializer`: an Int ordinal.
                27 => data.platform = Some(r.i32()?),
                28 => data.beam_mode = Some(r.i8()?),
                29 => data.battle = Some(nullable(r, Reader::uuid)?),
                // `StringSetDataSerializer`: a count in one byte, then the strings.
                30 => {
                    data.aspects = Some((0..r.u8()?).map(|_| string(r)).collect::<Result<_>>()?);
                }
                // `PoseTypeDataSerializer`: an Int ordinal.
                32 => data.pose = Some(ordinal(r, &PoseType::ALL, "pose")?),
                33 => data.label_level = Some(r.varint()?),
                34 => data.hide_label = Some(r.bool()?),
                31 => data.dying = Some(r.bool()?),
                // UNBATTLEABLE, COUNTS_TOWARDS_SPAWN_CAP, EVOLUTION_STARTED.
                35 | 36 | 41 => {
                    r.bool()?;
                }
                // FRIENDSHIP.
                38 => {
                    r.varint()?;
                }
                // SPAWN_DIRECTION, FREEZE_FRAME, RIDE_STAMINA.
                37 | 39 | 44 => {
                    r.f32()?;
                }
                // CAUGHT_BALL.
                40 => {
                    string(r)?;
                }
                // SHOWN_HELD_ITEM: an empty stack is its count alone.
                42 => {
                    if r.varint()? > 0 {
                        data.stopped_at = Some(index);
                        return Ok(data);
                    }
                }
                // `RideBoostsDataSerializer`: `writeMap` of a stat's name and its boost.
                43 => {
                    for _ in 0..r.count(64)? {
                        string(r)?;
                        r.f32()?;
                    }
                }
                45 => data.scale_modifier = Some(r.f32()?),
                _ => data.alpha = Some(r.bool()?),
            }
        }
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "a Pokémon's data: {} bytes after its end",
                r.remaining()
            )));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first two spawns a Cobblemon 1.8.1 server (Fabric 0.18.4, Minecraft 1.21.1, seed 1234) sent a
    /// client near its world spawn, as they came off the wire (`examples/capture.rs`).
    const COMBEE: &[u8] = include_bytes!("../tests/fixtures/spawn_pokemon_1.bin");
    const SECOND: &[u8] = include_bytes!("../tests/fixtures/spawn_pokemon_2.bin");
    /// What vanilla's readers left of the data of two Pokémon of that server, as it sent them: one as it
    /// appeared, already walking, and one as it fell asleep.
    const APPEARING: &[u8] = include_bytes!("../tests/fixtures/pokemon_data_appearing.bin");
    const FALLING_ASLEEP: &[u8] =
        include_bytes!("../tests/fixtures/pokemon_data_falling_asleep.bin");
    /// What that server told a player as they joined: its settings, their general data, and their
    /// Pokédex after `pokedex grant` of a Pikachu and a Bulbasaur. Then the Pokédex's update as the
    /// Pikachu was granted.
    const SETTINGS: &[u8] = include_bytes!("../tests/fixtures/server_settings.bin");
    const GENERAL: &[u8] = include_bytes!("../tests/fixtures/playerdata_general.bin");
    const POKEDEX: &[u8] = include_bytes!("../tests/fixtures/pokedex_at_login.bin");
    const ONE_MORE: &[u8] = include_bytes!("../tests/fixtures/pokedex_one_more.bin");

    #[test]
    fn the_server_s_settings_and_the_player_s_pokedex_read_whole() {
        let settings = ServerSettings::read(SETTINGS).unwrap();
        assert!(settings.display_entity_level_label && settings.display_entity_name_label);
        assert!(!settings.prevent_complete_party_deposit && !settings.unlock_all_move_dex_moves);
        assert_eq!(
            (
                settings.max_pokemon_level,
                settings.max_pokemon_friendship,
                settings.max_dynamax_level
            ),
            (100, 255, 10)
        );
        assert_eq!(settings.intrinsic_size, [0.95, 1.05]);
        assert!(ServerSettings::read(&SETTINGS[..23]).is_err());
        assert!(ServerSettings::read(&[SETTINGS, &[0]].concat()).is_err());

        let owned = |name: &str| (name.to_owned(), DexKnowledge::Owned);
        assert_eq!(
            PokedexData::read(POKEDEX).unwrap(),
            Some(PokedexData {
                incremental: false,
                species: vec![owned("cobblemon:pikachu"), owned("cobblemon:bulbasaur")],
            })
        );
        assert_eq!(
            PokedexData::read(ONE_MORE).unwrap(),
            Some(PokedexData {
                incremental: true,
                species: vec![owned("cobblemon:pikachu")],
            })
        );
        // Another kind of player data is not read, and says so.
        assert_eq!(PokedexData::read(GENERAL).unwrap(), None);
        // The form's knowledge is the record's last Int: seen, then nothing a Pokédex can hold.
        let last = ONE_MORE.len() - 1;
        assert_eq!(ONE_MORE[last - 3..], [0, 0, 0, 2]);
        let mut seen = ONE_MORE.to_vec();
        seen[last] = 1;
        assert_eq!(
            PokedexData::read(&seen).unwrap().unwrap().species[0].1,
            DexKnowledge::Seen
        );
        seen[last] = 3;
        let e = PokedexData::read(&seen).unwrap_err();
        assert!(e.0.contains("Pokédex knowledge ordinal 3"), "{}", e.0);
        for cut in [0, 5, 20, last] {
            assert!(PokedexData::read(&ONE_MORE[..cut]).is_err(), "cut at {cut}");
        }
        assert!(PokedexData::read(&[ONE_MORE, &[0]].concat()).is_err());
    }

    #[test]
    fn a_pokemon_s_own_data_reads_by_its_layout() {
        let data = PokemonData::read_rest(APPEARING).unwrap();
        assert_eq!(data.aspects, Some(vec!["male".to_owned()]));
        assert_eq!(data.pose, Some(PoseType::Walk));
        assert_eq!(data.label_level, Some(4));
        let scale = data.scale_modifier.unwrap();
        assert!((scale - 0.9056).abs() < 0.001, "{scale}");
        assert_eq!(
            (data.platform, data.battle, data.alpha, data.stopped_at),
            (None, None, None, None)
        );
        // A change of pose travels alone.
        assert_eq!(
            PokemonData::read_rest(FALLING_ASLEEP).unwrap(),
            PokemonData {
                pose: Some(PoseType::Sleep),
                ..Default::default()
            }
        );
        // The entries the captures lack, as Cobblemon's serializers write them: a raft, no battle, a
        // boost of the ride's speed.
        let mut rest = vec![27, 34, 0, 0, 0, 2, 29, 13, 0, 43, 38, 1, 5];
        rest.extend_from_slice(b"SPEED");
        rest.extend_from_slice(&1.5f32.to_be_bytes());
        rest.extend_from_slice(&[46, 8, 1, 0xFF]);
        assert_eq!(
            PokemonData::read_rest(&rest).unwrap(),
            PokemonData {
                platform: Some(2),
                battle: Some(None),
                alpha: Some(true),
                ..Default::default()
            }
        );
    }

    #[test]
    fn another_layout_is_an_error_and_what_cannot_be_measured_ends_the_reading() {
        for cut in [0, 1, 3, FALLING_ASLEEP.len() - 1] {
            assert!(
                PokemonData::read_rest(&FALLING_ASLEEP[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut longer = FALLING_ASLEEP.to_vec();
        longer.push(0);
        let e = PokemonData::read_rest(&longer).unwrap_err();
        assert!(e.0.contains("1 bytes after its end"), "{}", e.0);
        // The pose under vanilla's Int, a level under a mod's serializer: not this layout.
        let mut vanilla = FALLING_ASLEEP.to_vec();
        vanilla[1] = 1;
        let e = PokemonData::read_rest(&vanilla).unwrap_err();
        assert!(e.0.contains("data 32 has serializer 1"), "{}", e.0);
        let e = PokemonData::read_rest(&[33, 40, 4, 0xFF]).unwrap_err();
        assert!(e.0.contains("data 33 has serializer 40"), "{}", e.0);
        let mut bad = FALLING_ASLEEP.to_vec();
        bad[5] = 99;
        let e = PokemonData::read_rest(&bad).unwrap_err();
        assert!(e.0.contains("pose ordinal 99"), "{}", e.0);
        // An index past Cobblemon 1.8.1's, a held item: what was read before is kept.
        let asleep = Some(PoseType::Sleep);
        let past = PokemonData::read_rest(&[32, 33, 0, 0, 0, 2, 47, 8, 1, 0xFF]).unwrap();
        assert_eq!((past.pose, past.stopped_at), (asleep, Some(47)));
        let item = PokemonData::read_rest(&[32, 33, 0, 0, 0, 2, 42, 7, 1, 9, 9, 9]).unwrap();
        assert_eq!((item.pose, item.stopped_at), (asleep, Some(42)));
        // An empty hand is its count alone, and the reading goes on.
        let empty = PokemonData::read_rest(&[42, 7, 0, 46, 8, 0, 0xFF]).unwrap();
        assert_eq!((empty.alpha, empty.stopped_at), (Some(false), None));
    }

    #[test]
    fn a_wild_combee_decodes_to_its_last_byte() {
        let p = SpawnPokemon::read(COMBEE).unwrap();
        assert_eq!(p.species, "cobblemon:combee");
        assert_eq!(
            (p.gender, p.shiny, p.form.as_str()),
            (Gender::Male, false, "normal")
        );
        assert_eq!(p.aspects, ["male"]);
        assert_eq!(p.pose, PoseType::Hover);
        assert_eq!((p.owner, p.storage, p.battle), (None, None, None));
        assert_eq!((p.nickname, p.mark), (None, None));
        assert_eq!((p.label_level, p.friendship), (19, 50));
        assert_eq!(p.caught_ball, "cobblemon:poke_ball");
        assert!(
            (p.scale_modifier - 0.981).abs() < 0.001,
            "{}",
            p.scale_modifier
        );
        assert_eq!(p.freeze_frame, -1.0);
        assert!(p.ride_boosts.is_empty() && p.riders.is_empty());
        assert_eq!(
            (p.alpha, p.silent, p.unbattlable, p.hide_label),
            (false, false, false, false)
        );
        // The vanilla packet at the end: the server's own number for cobblemon:pokemon, then where it is.
        assert_eq!((p.entity.id, p.entity.kind), (56, 130));
        let [x, y, z] = p.entity.position;
        assert!(
            (x + 437.499).abs() < 0.001 && (y - 78.411).abs() < 0.001 && (z + 30.5).abs() < 0.001
        );
        assert_eq!(p.entity.velocity, [-1.0 / 8000.0, 0.0, 0.0]);
    }

    #[test]
    fn a_drifted_layout_is_an_error_not_a_misread() {
        assert!(SpawnPokemon::read(SECOND).is_ok());
        for cut in [0, 1, 17, COMBEE.len() - 1] {
            assert!(SpawnPokemon::read(&COMBEE[..cut]).is_err(), "cut at {cut}");
        }
        let mut longer = COMBEE.to_vec();
        longer.push(0);
        let e = SpawnPokemon::read(&longer).unwrap_err();
        assert!(e.0.contains("1 bytes left over"), "{}", e.0);
        // An ordinal outside the enum (the pose, here) is named.
        let pose = 73;
        assert_eq!(
            &COMBEE[pose..pose + 4],
            [0, 0, 0, 3],
            "the pose's Int, HOVER"
        );
        let mut bad = COMBEE.to_vec();
        bad[pose + 3] = 99;
        assert!(
            SpawnPokemon::read(&bad)
                .unwrap_err()
                .0
                .contains("pose ordinal 99")
        );
    }
}
