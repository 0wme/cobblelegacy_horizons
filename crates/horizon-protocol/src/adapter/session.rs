//! One connection to a server of an older release: which state each direction is in, followed from the
//! packets themselves, and every packet renumbered and translated on its way through.
//!
//! The states change where vanilla's `Connection` switches its codecs: the handshake's intent picks status
//! or login for both directions, `login_finished` and `login_acknowledged` move each direction to
//! configuration, `finish_configuration` to play, and `start_configuration` / `configuration_acknowledged`
//! back to configuration.
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use super::{ServerVersion, translate};
use crate::{
    Result, State,
    buf::{Reader, Writer},
    ids,
    packets::handshake,
};

fn to_u8(state: State) -> u8 {
    match state {
        State::Handshake => 0,
        State::Status => 1,
        State::Login => 2,
        State::Configuration => 3,
        State::Play => 4,
    }
}
fn from_u8(v: u8) -> State {
    match v {
        1 => State::Status,
        2 => State::Login,
        3 => State::Configuration,
        4 => State::Play,
        _ => State::Handshake,
    }
}

/// Shared by a connection's reader and writer.
pub struct Session {
    pub version: ServerVersion,
    /// Dimension of the last play login or respawn, for older packets with only a local block position.
    pub(super) dimension: Mutex<String>,
    /// Local entity id and last shift input, for pre-1.21.6 crouch commands.
    pub(super) local_player: Mutex<Option<(i32, bool)>>,
    pub(super) chat_index: std::sync::atomic::AtomicI32,
    pub(super) minecart_display: Mutex<HashMap<i32, (i32, bool)>>,
    pub(super) trim_patterns: Mutex<HashMap<i32, i32>>,
    pub(super) legacy_recipes: Mutex<translate::v1_21_1::recipes::Recipes>,
    pub(super) legacy_boats: Mutex<HashMap<i32, translate::v1_21_1::boats::Boat>>,
    pub(super) legacy_registries: Mutex<Arc<super::ids::LegacyRegistries>>,
    pub(super) legacy_metadata: Mutex<translate::v1_20_3::Metadata>,
    pub(super) legacy_items: Arc<Mutex<crate::item::v1_20_3::Cache>>,
    pub(super) legacy_resource_packs: Mutex<translate::v1_20_2::ResourcePacks>,
    login_requires_authentication: AtomicBool,
    legacy_secure_chat_enforcement: Mutex<Option<bool>>,
    reading: AtomicU8,
    writing: AtomicU8,
    /// Entity UUIDs by network id, for a server that is told which entity a spectator picked by its UUID.
    pub(super) uuids: Mutex<HashMap<i32, u128>>,
    /// Entity types (pinned ids) by network id, for entity data laid out by type.
    pub(super) kinds: Mutex<HashMap<i32, i32>>,
    /// Packets the translation made, for Horizon to read before the next one from the server
    /// (pinned id, payload).
    pub(super) incoming: Mutex<VecDeque<(i32, Vec<u8>)>>,
    /// Packet bodies to send right after the one being sent.
    pub(super) outgoing: Mutex<Vec<Vec<u8>>>,
    /// What a modded server's own blocks and items are numbered here (`set_modded`), once its
    /// table was read; none on a server without mods.
    modded: std::sync::RwLock<Option<Arc<super::fabric::Modded>>>,
}

impl Session {
    pub fn new(version: ServerVersion) -> Self {
        Self {
            version,
            dimension: Mutex::new("minecraft:overworld".into()),
            local_player: Mutex::new(None),
            chat_index: std::sync::atomic::AtomicI32::new(0),
            minecart_display: Mutex::new(HashMap::new()),
            trim_patterns: Mutex::new(HashMap::new()),
            legacy_recipes: Mutex::new(translate::v1_21_1::recipes::Recipes::default()),
            legacy_boats: Mutex::new(HashMap::new()),
            legacy_registries: Mutex::new(Arc::new(super::legacy::registries(version))),
            legacy_metadata: Mutex::new(Default::default()),
            legacy_items: Arc::new(Mutex::new(Default::default())),
            legacy_resource_packs: Mutex::new(Default::default()),
            login_requires_authentication: AtomicBool::new(false),
            legacy_secure_chat_enforcement: Mutex::new(None),
            reading: AtomicU8::new(0),
            writing: AtomicU8::new(0),
            uuids: Mutex::new(HashMap::new()),
            kinds: Mutex::new(HashMap::new()),
            incoming: Mutex::new(VecDeque::new()),
            outgoing: Mutex::new(Vec::new()),
            modded: std::sync::RwLock::new(None),
        }
    }
    /// Translates from here on with a modded server's own numbers: its chunk sections are as wide
    /// as its state count says, and its mods' block states and items become the client's.
    pub fn set_modded(&self, modded: super::fabric::Modded) {
        *self.modded.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(modded));
    }
    pub(super) fn modded(&self) -> Option<Arc<super::fabric::Modded>> {
        self.modded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn reading(&self) -> State {
        from_u8(self.reading.load(Ordering::Acquire))
    }
    pub fn writing(&self) -> State {
        from_u8(self.writing.load(Ordering::Acquire))
    }
    fn set_reading(&self, state: State) {
        self.reading.store(to_u8(state), Ordering::Release);
    }
    fn set_writing(&self, state: State) {
        self.writing.store(to_u8(state), Ordering::Release);
    }

    pub(super) fn login_requires_authentication(&self) -> bool {
        self.login_requires_authentication.load(Ordering::Acquire)
    }

    /// Before 1.20.5 this flag arrives in ServerData instead of the play login.
    pub fn legacy_secure_chat_enforcement(&self) -> Option<bool> {
        if !self.version.at_most(ServerVersion::V1_20_3) {
            return None;
        }
        *self.legacy_secure_chat_enforcement.lock().unwrap()
    }

    pub(super) fn set_legacy_secure_chat_enforcement(&self, value: Option<bool>) {
        *self.legacy_secure_chat_enforcement.lock().unwrap() = value;
    }

    /// A packet the server sent, as the pinned version numbers and lays it out. `None` drops it: Horizon
    /// knows no such packet.
    pub fn clientbound(&self, id: i32, payload: Vec<u8>) -> Result<Option<(i32, Vec<u8>)>> {
        let state = self.reading();
        let _legacy = self
            .version
            .at_most(ServerVersion::V1_20_3)
            .then(|| super::ids::legacy_registries(self.legacy_registries.lock().unwrap().clone()));
        let _items = self
            .version
            .at_most(ServerVersion::V1_20_3)
            .then(|| super::ids::legacy_items(self.legacy_items.clone()));
        let recipe_book = if self.version.at_most(ServerVersion::V1_20_3) {
            63
        } else {
            65
        };
        if self.version.at_most(ServerVersion::V1_21_1) && state == State::Play && id == recipe_book
        {
            return translate::v1_21_1::recipes::recipe_book(self, &payload).map(Some);
        }
        // Vanilla 1.21.4's separate add_experience_orb (id 2) became AddEntity plus metadata.
        if self.version.at_most(ServerVersion::V1_21_4) && state == State::Play && id == 2 {
            return translate::old_experience_orb(self, &payload).map(Some);
        }
        let Some(pinned) = self.version.clientbound_id(state, id) else {
            return Ok(None);
        };
        match (state, pinned) {
            (State::Login, ids::login::clientbound::LOGIN_FINISHED) => {
                self.set_reading(State::Configuration)
            }
            (State::Configuration, ids::configuration::clientbound::FINISH_CONFIGURATION) => {
                self.set_reading(State::Play)
            }
            (State::Play, ids::play::clientbound::START_CONFIGURATION) => {
                self.set_legacy_secure_chat_enforcement(None);
                self.set_reading(State::Configuration)
            }
            (State::Play, ids::play::clientbound::LOGIN) => {
                self.set_legacy_secure_chat_enforcement(None);
            }
            _ => {}
        }
        // A packet the translation can't read goes on as it came, as an unknown packet would: Horizon's
        // own reader then skips it or reports it, rather than the connection dropping here.
        let translated = match translate::clientbound(self, state, pinned, &payload) {
            Ok(Some((id, Some(translated)))) => Some((id, translated)),
            Ok(Some((id, None))) => Some((id, payload)),
            Ok(None) => None,
            Err(e) => {
                log::warn!(
                    "Horizon: could not translate {} packet {pinned} ({state:?}): {e}",
                    self.version.name()
                );
                Some((pinned, payload))
            }
        };
        if state == State::Login
            && let Some((id, payload)) = &translated
            && let Ok(crate::packets::login::Clientbound::Hello(hello)) =
                crate::packets::login::Clientbound::decode(*id, payload)
        {
            self.login_requires_authentication
                .store(hello.should_authenticate, Ordering::Release);
        }
        if self.version.at_most(ServerVersion::V1_21_1)
            && state == State::Play
            && let Some((id, body)) = translated
        {
            return translate::v1_21_1::boats::finish(self, id, body).map(Some);
        }
        Ok(translated)
    }

    /// A packet body (VarInt id + payload) Horizon sends, as this server numbers and lays it out. `None`:
    /// the server has no such packet, so nothing is sent.
    pub fn serverbound(&self, body: &[u8]) -> Result<Option<Vec<u8>>> {
        let state = self.writing();
        let _legacy = self
            .version
            .at_most(ServerVersion::V1_20_3)
            .then(|| super::ids::legacy_registries(self.legacy_registries.lock().unwrap().clone()));
        let _items = self
            .version
            .at_most(ServerVersion::V1_20_3)
            .then(|| super::ids::legacy_items(self.legacy_items.clone()));
        let mut r = Reader::new(body);
        let id = r.varint()?;
        let payload = r.rest();
        match (state, id) {
            (State::Handshake, ids::handshake::serverbound::INTENTION) => {
                return self.intention(payload).map(Some);
            }
            (State::Login, ids::login::serverbound::LOGIN_ACKNOWLEDGED) => {
                self.set_writing(State::Configuration)
            }
            (State::Configuration, ids::configuration::serverbound::FINISH_CONFIGURATION) => {
                self.set_writing(State::Play)
            }
            (State::Play, ids::play::serverbound::CONFIGURATION_ACKNOWLEDGED) => {
                self.set_writing(State::Configuration)
            }
            _ => {}
        }
        let Some((mapped, payload)) = translate::serverbound(self, state, id, payload)? else {
            return Ok(None);
        };
        let mut w = Writer::new();
        w.varint(mapped);
        w.bytes(&payload);
        Ok(Some(w.data))
    }

    /// A packet the translation made that Horizon reads next, already in the pinned version's numbering.
    pub fn next_incoming(&self) -> Option<(i32, Vec<u8>)> {
        self.incoming.lock().unwrap().pop_front()
    }
    /// Bodies the translation added after the packet just sent, in the server's numbering.
    pub fn take_outgoing(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.outgoing.lock().unwrap())
    }

    /// The handshake states this server's protocol, and its intent moves both directions on.
    fn intention(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut r = Reader::new(payload);
        let _pinned = r.varint()?;
        let host = r.string(255)?;
        let port = r.u16()?;
        let intent = r.varint()?;
        let next = if intent == handshake::STATUS {
            State::Status
        } else {
            State::Login
        };
        self.set_writing(next);
        self.set_reading(next);
        let mut w = Writer::new();
        w.varint(ids::handshake::serverbound::INTENTION);
        w.varint(self.version.protocol());
        w.string(&host);
        w.u16(port);
        w.varint(intent);
        Ok(w.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(id: i32, payload: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(id);
        w.bytes(payload);
        w.data
    }

    fn old_play_id(version: ServerVersion, pinned: i32) -> i32 {
        (0..256)
            .find(|&id| version.clientbound_id(State::Play, id) == Some(pinned))
            .unwrap()
    }

    fn old_login(version: ServerVersion) -> Vec<u8> {
        let mut w = Writer::new();
        w.i32(42);
        w.bool(false);
        w.varint(1);
        w.string("minecraft:overworld");
        for n in [20, 8, 6] {
            w.varint(n);
        }
        for flag in [false, true, false] {
            w.bool(flag);
        }
        if version.at_most(ServerVersion::V1_20_3) {
            w.string("minecraft:overworld");
        } else {
            w.varint(0);
        }
        w.string("minecraft:overworld");
        w.i64(0);
        w.i8(0);
        w.i8(-1);
        for _ in 0..3 {
            w.bool(false);
        }
        w.varint(0);
        if !version.at_most(ServerVersion::V1_20_3) {
            w.bool(true); // The newer login has its own enforcement flag.
        }
        w.data
    }

    #[test]
    fn legacy_logins_preserve_the_encryption_requests_authentication_signal() {
        use crate::packets::{login, play};
        for version in [ServerVersion::V1_20_3, ServerVersion::V1_20_6] {
            for authenticated in [false, true] {
                let s = Session::new(version);
                s.set_reading(State::Login);
                // 765 had no opt-out bit: offline login omitted the encryption request.
                if authenticated || version == ServerVersion::V1_20_6 {
                    let mut hello = Writer::new();
                    hello.string("");
                    hello.byte_array(&[1, 2]);
                    hello.byte_array(&[3, 4]);
                    if version == ServerVersion::V1_20_6 {
                        hello.bool(authenticated);
                    }
                    let (id, payload) = s.clientbound(1, hello.data).unwrap().unwrap();
                    let login::Clientbound::Hello(hello) =
                        login::Clientbound::decode(id, &payload).unwrap()
                    else {
                        panic!("hello")
                    };
                    assert_eq!(hello.should_authenticate, authenticated);
                }
                assert_eq!(s.login_requires_authentication(), authenticated);
                if version == ServerVersion::V1_20_3 {
                    Arc::make_mut(&mut s.legacy_registries.lock().unwrap()).insert(
                        "minecraft:dimension_type".into(),
                        crate::registry::Ids::from_names(vec!["minecraft:overworld".into()]),
                    );
                }
                s.set_reading(State::Play);
                let (id, payload) = s
                    .clientbound(
                        old_play_id(version, ids::play::clientbound::LOGIN),
                        old_login(version),
                    )
                    .unwrap()
                    .unwrap();
                let play::Clientbound::Login(login) =
                    play::Clientbound::decode(id, &payload).unwrap()
                else {
                    panic!("login")
                };
                assert_eq!(login.online_mode, authenticated, "{}", version.name());
                assert_eq!(
                    login.enforces_secure_chat,
                    version == ServerVersion::V1_20_6
                );
            }
        }
    }

    /// A modded 1.21.1 server (Fabric with Cobblemon) sends its own sound events after vanilla's 1,611:
    /// such a packet is dropped, where it used to reach the reader as an inline sound with no location
    /// ("needed 8 bytes, 2 left"). A vanilla sound and an inline one still go through.
    #[test]
    fn a_sound_the_pinned_version_lacks_is_dropped_not_misread() {
        use crate::packets::play;
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        let sound = |holder: &dyn Fn(&mut Writer)| {
            let mut w = Writer::new();
            holder(&mut w);
            w.varint(4); // blocks
            for v in [8, 64, -8] {
                w.i32(v);
            }
            w.f32(1.0);
            w.f32(1.0);
            w.i64(7);
            w.data
        };
        let sound_id = old_play_id(s.version, ids::play::clientbound::SOUND);
        // The first id past vanilla 1.21.1's registry, as a holder (id + 1).
        let modded = sound(&|w| w.varint(1611 + 1));
        assert_eq!(s.clientbound(sound_id, modded).unwrap(), None);
        // Vanilla's first sound still renumbers and decodes.
        let (id, payload) = s
            .clientbound(sound_id, sound(&|w| w.varint(1)))
            .unwrap()
            .unwrap();
        let play::Clientbound::Sound(p) = play::Clientbound::decode(id, &payload).unwrap() else {
            panic!("sound")
        };
        assert!(p.sound.0 >= 0 && p.sound.1.is_none());
        assert_eq!((p.source, p.seed), (4, 7));
        // An inline sound names itself: it needs no registry entry.
        let inline = sound(&|w| {
            w.varint(0);
            w.string("cobblemon:pokemon.pikachu.cry");
            w.bool(false);
        });
        let (id, payload) = s.clientbound(sound_id, inline).unwrap().unwrap();
        let play::Clientbound::Sound(p) = play::Clientbound::decode(id, &payload).unwrap() else {
            panic!("inline sound")
        };
        assert_eq!(
            p.sound,
            (-1, Some(("cobblemon:pokemon.pikachu.cry".into(), None)))
        );

        let mut entity = Writer::new();
        entity.varint(1611 + 1);
        entity.varint(6); // neutral
        entity.varint(12);
        entity.f32(1.0);
        entity.f32(1.0);
        entity.i64(7);
        let entity_id = old_play_id(s.version, ids::play::clientbound::SOUND_ENTITY);
        assert_eq!(s.clientbound(entity_id, entity.data).unwrap(), None);
    }

    /// A modded 1.21.1 server (Fabric with Cobblemon) has argument parsers of its own after vanilla's
    /// (`cobblemon:pokemon`, a species): the tree used to be refused whole ("unknown argument type -1"),
    /// so every command read as unknown. Such an argument is now the rest of the line, completed by the
    /// server, and the nodes around it keep theirs.
    #[test]
    fn a_command_argument_the_pinned_version_lacks_asks_the_server() {
        use crate::packets::play;
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        let mut w = Writer::new();
        w.varint(5);
        // The root, over two commands.
        w.u8(0);
        w.varint(2);
        w.varint(1);
        w.varint(3);
        // `pokespawn <pokemon>`: a mod's parser, which writes no property.
        w.u8(1);
        w.varint(1);
        w.varint(2);
        w.string("pokespawn");
        w.u8(2 | 4);
        w.varint(0);
        w.string("pokemon");
        w.varint(200);
        // `level <amount>`: vanilla's integer with a minimum, after the mod's in the packet.
        w.u8(1);
        w.varint(1);
        w.varint(4);
        w.string("level");
        w.u8(2 | 4);
        w.varint(0);
        w.string("amount");
        w.varint(3);
        w.u8(1);
        w.i32(1);
        w.varint(0);
        let (id, payload) = s
            .clientbound(
                old_play_id(s.version, ids::play::clientbound::COMMANDS),
                w.data,
            )
            .unwrap()
            .unwrap();
        let play::Clientbound::Commands(tree) = play::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("commands")
        };
        let pokemon = &tree.nodes[2];
        assert_eq!(
            (pokemon.name.as_str(), pokemon.executable),
            ("pokemon", true)
        );
        // brigadier:string, greedy.
        assert_eq!(pokemon.parser, Some((5, vec![2])));
        assert_eq!(pokemon.suggestions.as_deref(), Some("minecraft:ask_server"));
        let amount = &tree.nodes[4];
        assert_eq!(amount.name, "amount");
        assert_eq!(amount.suggestions, None);
        assert_eq!(
            play::ArgumentProperties::decode(amount.parser.as_ref().unwrap().0, &[1, 0, 0, 0, 1])
                .unwrap(),
            play::ArgumentProperties::Integer {
                min: Some(1),
                max: None
            }
        );
        assert_eq!(tree.root, 0);
    }

    /// A modded 1.21.1 server (Fabric with Cobblemon) numbers its mods' block states after the game's
    /// 26,684. They were all air here; once the session has the server's table (`set_modded`), such a
    /// state is the client's state of that block, and one the client has no block for stays air. The
    /// game's own states are renumbered as before.
    #[test]
    fn a_mod_s_block_state_becomes_the_client_s_once_the_table_is_known() {
        use crate::{adapter::fabric::Modded, packets::play};
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        let update_id = old_play_id(s.version, ids::play::clientbound::BLOCK_UPDATE);
        let state_at = |s: &Session, state: i32| {
            let sent = play::Clientbound::BlockUpdate(play::BlockUpdate {
                pos: [1, 64, -3],
                state,
            })
            .encode();
            // The packet without its id: the position is laid out alike in both releases.
            let mut r = Reader::new(&sent);
            r.varint().unwrap();
            let (id, payload) = s
                .clientbound(update_id, r.rest().to_vec())
                .unwrap()
                .unwrap();
            let play::Clientbound::BlockUpdate(p) =
                play::Clientbound::decode(id, &payload).unwrap()
            else {
                panic!("a block update")
            };
            assert_eq!(p.pos, [1, 64, -3]);
            p.state
        };
        let stone = state_at(&s, 1);
        assert_ne!(stone, 0);
        // Without the table, a state past the game's own is air.
        assert_eq!(state_at(&s, 26_684 + 1), 0);
        s.set_modded(Modded {
            states: 59_579,
            first_state: 26_684,
            block_states: vec![0, 40_001, 40_002],
            ..Default::default()
        });
        assert_eq!(state_at(&s, 26_684 + 1), 40_001);
        assert_eq!(state_at(&s, 26_684 + 2), 40_002);
        // A block the client has none for, and a state past the table's end.
        assert_eq!(state_at(&s, 26_684), 0);
        assert_eq!(state_at(&s, 26_684 + 3), 0);
        assert_eq!(state_at(&s, 1), stone);
    }

    /// A mod's item arrived as an empty slot, and a stack that carried one of a mod's components failed
    /// its whole packet ("unsupported item component -1"): nothing says how long a component's value
    /// is. With the server's table, the item is the client's of that name, and the mods' components are
    /// read past (an NBT tag, or the boolean of Cobblemon's `crafted`) and left out of the stack.
    #[test]
    fn a_mod_s_item_is_the_client_s_and_keeps_none_of_the_mods_components() {
        use crate::{
            adapter::fabric::{Modded, Skip},
            packets::play,
        };
        let equipment_id = old_play_id(
            ServerVersion::V1_21_1,
            ids::play::clientbound::SET_EQUIPMENT,
        );
        // An entity's main hand: three of the server's item 1333 (a berry), with two components added
        // and one removed, all of them a mod's.
        let mut w = Writer::new();
        w.varint(42);
        w.u8(0);
        w.varint(3);
        w.varint(1333);
        w.varint(2);
        w.varint(1);
        w.varint(57);
        // `cobblemon:flavour` as a tag: a string, "abc".
        w.data.extend_from_slice(&[8, 0, 3, b'a', b'b', b'c']);
        w.varint(67);
        w.bool(true);
        w.varint(59);
        let sent = w.data;

        let stack = |s: &Session| {
            let (id, payload) = s
                .clientbound(equipment_id, sent.clone())
                .map_err(|e| e.0)?
                .ok_or("dropped")?;
            match play::Clientbound::decode(id, &payload).map_err(|e| e.0)? {
                play::Clientbound::SetEquipment(p) => Ok(p.slots),
                _ => Err("another packet".to_owned()),
            }
        };
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        s.set_modded(
            Modded {
                components: [(57, Skip::Nbt), (59, Skip::Nbt), (67, Skip::Bool)].into(),
                ..Default::default()
            }
            .with_items([(1333, 1600)]),
        );
        let slots: std::result::Result<_, String> = stack(&s);
        let slots = slots.expect("the packet reads");
        assert_eq!(slots.len(), 1);
        let held = slots[0].1.as_ref().expect("a stack");
        assert_eq!((slots[0].0, held.item, held.count), (0, 1600, 3));
        assert!(held.added.is_empty() && held.removed.is_empty());
    }

    /// 1.21.1's entity teleport is what its tracker syncs an entity's position with: it sets the base the
    /// next moves are relative to, as 26.3's position sync does and its teleport does not. Read as a
    /// teleport, a Pokémon a command moved nine blocks was drawn back where it stood at its next move.
    /// 1.21.3 already sends the sync itself, and its teleport stays one.
    #[test]
    fn an_entity_teleport_up_to_1_21_1_is_a_position_sync() {
        use crate::packets::play;
        let teleport = ids::play::clientbound::TELEPORT_ENTITY;
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        let mut w = Writer::new();
        w.varint(611);
        for v in [-389.5, 120.0, -66.5] {
            w.f64(v);
        }
        // A quarter turn and an eighth, as bytes of a turn; on the ground.
        w.i8(64);
        w.i8(32);
        w.bool(true);
        let (id, payload) = s
            .clientbound(old_play_id(s.version, teleport), w.data)
            .unwrap()
            .unwrap();
        let play::Clientbound::EntityPositionSync(p) =
            play::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("a position sync")
        };
        assert_eq!(p.id, 611);
        assert_eq!(p.path.end(), [-389.5, 120.0, -66.5]);
        assert_eq!((p.yaw, p.pitch, p.on_ground), (90.0, 45.0, true));
        assert!(s.next_incoming().is_none());

        let later = Session::new(ServerVersion::V1_21_3);
        later.set_reading(State::Play);
        let sent = play::Clientbound::TeleportEntity(play::TeleportEntity {
            id: 611,
            position: [1.0, 2.0, 3.0],
            delta: [0.0; 3],
            yaw: 90.0,
            pitch: 45.0,
            relatives: 0,
            on_ground: true,
        })
        .encode();
        // The packet without its id, as a server of that release lays it out (as 26.3 does).
        let mut r = Reader::new(&sent);
        r.varint().unwrap();
        let (id, payload) = later
            .clientbound(old_play_id(later.version, teleport), r.rest().to_vec())
            .unwrap()
            .unwrap();
        assert!(matches!(
            play::Clientbound::decode(id, &payload).unwrap(),
            play::Clientbound::TeleportEntity(p) if p.position == [1.0, 2.0, 3.0]
        ));
    }

    /// A modded 1.21.1 server (Fabric with Cobblemon) numbers its own entity data serializers after
    /// vanilla's 31. Nothing says how long their values are, so the data is read up to the first of
    /// them; what the server sent from there on used to be lost (a Pokémon's pose, at index 32), and now
    /// follows the packet as it came, for the reader that knows the entity's layout.
    #[test]
    fn entity_data_past_a_mod_s_serializer_follows_as_a_payload() {
        use crate::{entity_data::Value, packets::play};
        let s = Session::new(ServerVersion::V1_21_1);
        s.set_reading(State::Play);
        let data_id = old_play_id(s.version, ids::play::clientbound::SET_ENTITY_DATA);
        let moving = |w: &mut Writer| {
            w.varint(611);
            // PokemonEntity.MOVING: vanilla's boolean, 8 in 1.21.1.
            w.u8(24);
            w.varint(8);
            w.bool(true);
        };
        // POSE_TYPE under Cobblemon's serializer (an Int ordinal), then vanilla's Int of LABEL_LEVEL.
        let rest = [32, 33, 0, 0, 0, 1, 33, 1, 4, 0xFF];
        let mut w = Writer::new();
        moving(&mut w);
        w.bytes(&rest);
        let (id, payload) = s.clientbound(data_id, w.data).unwrap().unwrap();
        let play::Clientbound::SetEntityData(p) = play::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("entity data")
        };
        assert_eq!(p.id, 611);
        let read: Vec<(u8, &Value)> = p.values.iter().map(|(i, _, v)| (*i, v)).collect();
        assert_eq!(read, [(24, &Value::Bool(true))]);
        let (id, payload) = s.next_incoming().unwrap();
        let play::Clientbound::CustomPayload(p) = play::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("payload")
        };
        assert_eq!(p.channel, crate::adapter::UNREAD_ENTITY_DATA);
        // The entity's id as a VarInt, then the server's bytes.
        assert_eq!(p.data[..2], [0xE3, 0x04]);
        assert_eq!(p.data[2..], rest);
        assert!(s.next_incoming().is_none());

        // Data vanilla's readers know to its end has nothing follow it.
        let mut w = Writer::new();
        moving(&mut w);
        w.u8(0xFF);
        assert!(s.clientbound(data_id, w.data).unwrap().is_some());
        assert!(s.next_incoming().is_none());
    }

    #[test]
    fn legacy_server_data_updates_and_resets_secure_chat_enforcement() {
        use crate::buf::Codec;
        use crate::packets::play;
        use horizon_world::nbt::Tag;
        let s = Session::new(ServerVersion::V1_20_3);
        s.set_reading(State::Play);
        let data = play::ServerData {
            motd: Tag::String("Wire server".into()),
            icon: None,
        };
        for enforced in [true, false] {
            let mut w = Writer::new();
            play::ServerData::write(&data, &mut w);
            let expected = w.data.clone();
            w.bool(enforced);
            let (id, payload) = s
                .clientbound(
                    old_play_id(s.version, ids::play::clientbound::SERVER_DATA),
                    w.data,
                )
                .unwrap()
                .unwrap();
            assert_eq!(payload, expected);
            assert!(matches!(
                play::Clientbound::decode(id, &payload).unwrap(),
                play::Clientbound::ServerData(_)
            ));
            assert_eq!(s.legacy_secure_chat_enforcement(), Some(enforced));
        }
        s.set_legacy_secure_chat_enforcement(Some(true));
        s.clientbound(
            old_play_id(s.version, ids::play::clientbound::START_CONFIGURATION),
            vec![],
        )
        .unwrap();
        assert_eq!(s.legacy_secure_chat_enforcement(), None);
        s.set_reading(State::Play);
        s.set_legacy_secure_chat_enforcement(Some(true));
        // Reset belongs to the login boundary even if a malformed body cannot be translated.
        s.clientbound(
            old_play_id(s.version, ids::play::clientbound::LOGIN),
            vec![],
        )
        .unwrap();
        assert_eq!(s.legacy_secure_chat_enforcement(), None);
        assert_eq!(
            Session::new(ServerVersion::Current).legacy_secure_chat_enforcement(),
            None
        );
    }

    #[test]
    fn the_handshake_states_the_older_protocol_and_moves_both_directions() {
        for (version, protocol) in [
            (ServerVersion::V26_2, 776),
            (ServerVersion::V1_21_8, 772),
            (ServerVersion::V1_21_6, 771),
            (ServerVersion::V1_21_5, 770),
            (ServerVersion::V1_21_4, 769),
            (ServerVersion::V1_21_3, 768),
            (ServerVersion::V1_21_1, 767),
        ] {
            let s = Session::new(version);
            let mut payload = Writer::new();
            payload.varint(crate::PROTOCOL_VERSION);
            payload.string("example.org");
            payload.u16(25565);
            payload.varint(handshake::LOGIN);
            let sent = s.serverbound(&body(0, &payload.data)).unwrap().unwrap();
            let mut r = Reader::new(&sent);
            assert_eq!(r.varint().unwrap(), 0);
            assert_eq!(r.varint().unwrap(), protocol);
            assert_eq!(r.string(255).unwrap(), "example.org");
            assert_eq!((s.reading(), s.writing()), (State::Login, State::Login));
        }
    }

    #[test]
    fn each_direction_follows_its_own_switches() {
        let s = Session::new(ServerVersion::V26_2);
        s.set_reading(State::Login);
        s.set_writing(State::Login);
        // login_finished arrives: the server already speaks configuration, Horizon still login.
        s.clientbound(ids::login::clientbound::LOGIN_FINISHED, Vec::new())
            .unwrap();
        assert_eq!(
            (s.reading(), s.writing()),
            (State::Configuration, State::Login)
        );
        s.serverbound(&body(ids::login::serverbound::LOGIN_ACKNOWLEDGED, &[]))
            .unwrap();
        assert_eq!(s.writing(), State::Configuration);
        // finish_configuration: 26.2 numbers it 3 as 26.3 does.
        s.clientbound(3, Vec::new()).unwrap();
        assert_eq!(s.reading(), State::Play);
        s.serverbound(&body(
            ids::configuration::serverbound::FINISH_CONFIGURATION,
            &[],
        ))
        .unwrap();
        assert_eq!(s.writing(), State::Play);
    }

    #[test]
    fn packets_are_renumbered_and_unknown_ones_dropped() {
        let s = Session::new(ServerVersion::V26_2);
        s.set_reading(State::Play);
        s.set_writing(State::Play);
        // 26.2's keep_alive is 44, 26.3's 45.
        let (id, payload) = s.clientbound(44, vec![0; 8]).unwrap().unwrap();
        assert_eq!((id, payload.len()), (ids::play::clientbound::KEEP_ALIVE, 8));
        assert_eq!(s.clientbound(141, Vec::new()).unwrap(), None);
        // resource_pack: Horizon's 50 is 26.2's 49.
        let sent = s
            .serverbound(&body(ids::play::serverbound::RESOURCE_PACK, &[7]))
            .unwrap()
            .unwrap();
        assert_eq!(sent, vec![49, 7]);
    }
}
