//! A modded Fabric server's own numbers (CobbleLegacy's fork).
//!
//! A Fabric server numbers what its mods register after the game's own entries, registry by registry,
//! and tells a client which name has which number when that client says it can read it: a
//! `minecraft:register` of [`REGISTRY_SYNC`] during configuration. The table comes as payloads on that
//! channel, slices of one buffer with an empty one last (Fabric API's `DirectRegistryPacketHandler`),
//! and the server waits for [`REGISTRY_SYNC_COMPLETE`] before it goes on.
//!
//! The table names blocks, not their states: how many states a mod's block has is in its Java code and
//! never on the wire. Whoever knows it (a report made on the server) turns the two into a [`Modded`],
//! which the session then translates with (`Session::set_modded`).
use crate::{
    Error, Result,
    buf::{Reader, Writer},
};
use std::collections::{BTreeMap, HashMap};

/// `DirectRegistryPacketHandler.Payload.ID`.
pub const REGISTRY_SYNC: &str = "fabric:registry/sync/direct";
/// `SyncCompletePayload.ID`: the client's answer once it has the whole table.
pub const REGISTRY_SYNC_COMPLETE: &str = "fabric:registry/sync/complete";

/// `CommonVersionPayload.ID` and `CommonRegisterPayload.ID`: Fabric API's "common packets", by which a
/// client says during configuration which channels it takes once in play. A server's mods ask Fabric
/// whether a player takes a channel before they send on it (`ServerPlayNetworking.canSend`), and some
/// turn away the player who does not: Cardinal Components API ends the connection with "This server
/// requires Cardinal Components API (unhandled packet: ...)".
///
/// The server speaks them only to a client whose `minecraft:register` named the two: it then sends the
/// versions it speaks on the first and waits for one back, sends its own play channels on the second
/// and waits for the client's.
pub const COMMON_VERSION: &str = "c:version";
pub const COMMON_REGISTER: &str = "c:register";
/// `CommonPacketsImpl.PACKET_VERSION_1`, the only version there is.
const COMMON_PACKETS: i32 = 1;
/// `CommonRegisterPayload.PLAY_PHASE`.
const PLAY_PHASE: &str = "play";

/// The answer to a server's `c:version` (`CommonVersionPayload`: the versions it speaks, a counted
/// array of VarInts): the version spoken here. `None` when the server does not speak it: Fabric then
/// has no answer it would take.
pub fn common_version_reply(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let r = &mut Reader::new(data);
    let mut spoken = false;
    for _ in 0..r.count(64)? {
        spoken |= r.varint()? == COMMON_PACKETS;
    }
    Ok(spoken.then(|| {
        let mut w = Writer::new();
        w.varint(1);
        w.varint(COMMON_PACKETS);
        w.data
    }))
}

/// The answer to a server's `c:register` (`CommonRegisterPayload`: a version, a phase, the server's
/// own channels of that phase): the same version and phase with `play`, the channels the client takes
/// once in play. For another phase it names none.
pub fn common_register_reply(data: &[u8], play: &[String]) -> Result<Vec<u8>> {
    let r = &mut Reader::new(data);
    let version = r.varint()?;
    let phase = r.string(64)?;
    let channels = if phase == PLAY_PHASE { play } else { &[] };
    let mut w = Writer::new();
    w.varint(version);
    w.string(&phase);
    w.varint(channels.len() as i32);
    for channel in channels {
        w.string(channel);
    }
    Ok(w.data)
}

/// The registries a Fabric server synced: for each (`minecraft:block`), the names of its entries by
/// their number there, the game's own entries included.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegistrySync {
    pub registries: BTreeMap<String, BTreeMap<i32, String>>,
}

impl RegistrySync {
    /// The buffer the slices make, as `DirectRegistryPacketHandler.sendPacket` writes it: registries
    /// grouped by namespace, and in each its entries grouped by namespace then by runs of numbers
    /// that follow one another (a run starts as far after the end of the one before as it says).
    /// `minecraft` is written as the empty string. What follows the last registry is not read: the
    /// buffer comes padded.
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(bytes);
        let namespace = |r: &mut Reader| -> Result<String> {
            let written = r.string(32767)?;
            Ok(if written.is_empty() {
                "minecraft".to_owned()
            } else {
                written
            })
        };
        let mut registries = BTreeMap::new();
        for _ in 0..r.count(r.remaining())? {
            let registry_namespace = namespace(r)?;
            for _ in 0..r.count(r.remaining())? {
                let registry = format!("{registry_namespace}:{}", r.string(32767)?);
                let mut entries = BTreeMap::new();
                let mut last = 0i32;
                for _ in 0..r.count(r.remaining())? {
                    let entry_namespace = namespace(r)?;
                    for _ in 0..r.count(r.remaining())? {
                        let start = last.checked_add(r.varint()?).ok_or_else(|| {
                            Error::new("fabric registry sync: a number overflows")
                        })?;
                        let length = r.count(r.remaining())?;
                        for offset in 0..length {
                            let number = start + offset as i32;
                            entries
                                .insert(number, format!("{entry_namespace}:{}", r.string(32767)?));
                            last = number;
                        }
                    }
                }
                registries.insert(registry, entries);
            }
        }
        Ok(Self { registries })
    }

    /// A registry's entries, by number.
    pub fn entries(&self, registry: &str) -> impl Iterator<Item = (i32, &str)> {
        self.registries
            .get(registry)
            .into_iter()
            .flatten()
            .map(|(number, name)| (*number, name.as_str()))
    }
}

/// What a modded server's own blocks and items are numbered here, for a session to translate with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Modded {
    /// How many block states the server has in all: what sizes its chunk sections' global palette,
    /// which the game's own count (26,684 in 1.21.1) no longer does.
    pub states: u32,
    /// The server's first block state past the game's own.
    pub first_state: u32,
    /// The pinned state of each of the server's states from [`Self::first_state`] on; 0, air, for a
    /// state of a block the client has none for.
    pub block_states: Vec<u32>,
    /// The pinned item of each of the server's items past the game's own that the client has.
    pub items_up: HashMap<i32, i32>,
    /// The same pairs, from the pinned item to the server's.
    pub items_down: HashMap<i32, i32>,
    /// How the value of each of the server's mods' item components is read past, by the server's
    /// number: the client has nothing to hold them, and a stack that carries one is kept without it.
    pub components: HashMap<i32, Skip>,
    /// The name of each of the server's mods' sound events, by the server's number: the client has no
    /// number for them, and a packet that plays one names it instead, as an inline sound does.
    pub sounds: HashMap<i32, String>,
}

/// How a mod's item component is written, for reading past it: nothing on the wire says how long a
/// component's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// One NBT tag: `ByteBufCodecs.fromCodec`, which nearly every mod's component uses.
    Nbt,
    /// One byte: `ByteBufCodecs.BOOL`.
    Bool,
}

impl Skip {
    /// Reads past a value written so.
    pub(crate) fn pass(self, r: &mut Reader) -> Result<()> {
        match self {
            Self::Nbt => r.nbt().map(drop),
            Self::Bool => r.bool().map(drop),
        }
    }
}

impl Modded {
    /// The pinned state of the server's `state`, for one past the game's own.
    pub fn state(&self, state: i32) -> Option<i32> {
        let index = u32::try_from(state).ok()?.checked_sub(self.first_state)?;
        self.block_states
            .get(index as usize)
            .and_then(|pinned| i32::try_from(*pinned).ok())
    }

    /// `Mth.ceillog2` of the server's state count: the bits of an entry of a section's global palette.
    pub fn state_bits(&self) -> u8 {
        (u32::BITS - self.states.saturating_sub(1).leading_zeros()) as u8
    }

    /// The pairs of items, with the reverse filled from them.
    pub fn with_items(mut self, items: impl IntoIterator<Item = (i32, i32)>) -> Self {
        for (server, pinned) in items {
            self.items_up.insert(server, pinned);
            self.items_down.insert(pinned, server);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_packets_are_answered_as_fabric_reads_them() {
        // `c:version`: the server speaks version 1.
        assert_eq!(common_version_reply(&[1, 1]).unwrap(), Some(vec![1, 1]));
        // A server that would only speak a later one gets no answer.
        assert_eq!(common_version_reply(&[1, 2]).unwrap(), None);
        assert!(common_version_reply(&[2, 1]).is_err());
        // `c:register` for play, with one channel of the server's own: the answer names the
        // client's, under the same version and phase.
        let mut asked = Writer::new();
        asked.varint(1);
        asked.string("play");
        asked.varint(1);
        asked.string("fabric:x");
        let reply =
            common_register_reply(&asked.data, &["a:b".to_owned(), "c:d".to_owned()]).unwrap();
        let r = &mut Reader::new(&reply);
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.string(64).unwrap(), "play");
        assert_eq!(r.count(8).unwrap(), 2);
        assert_eq!(r.string(64).unwrap(), "a:b");
        assert_eq!(r.string(64).unwrap(), "c:d");
        assert_eq!(r.remaining(), 0);
        // Another phase: none named.
        let mut asked = Writer::new();
        asked.varint(1);
        asked.string("configuration");
        asked.varint(0);
        let reply = common_register_reply(&asked.data, &["a:b".to_owned()]).unwrap();
        assert_eq!(reply.last(), Some(&0));
    }

    /// Two registries of one namespace as Fabric writes them: blocks 0, 1, then 1060 and 1061 of a
    /// mod; items with a gap inside the mod's namespace.
    fn sync() -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(1);
        w.string("");
        w.varint(2);
        w.string("block");
        w.varint(2);
        w.string("");
        w.varint(1);
        w.varint(0);
        w.varint(2);
        w.string("air");
        w.string("stone");
        w.string("cobblemon");
        w.varint(1);
        // 1060 is 1059 past the last number written, 1.
        w.varint(1059);
        w.varint(2);
        w.string("ice_stone_ore");
        w.string("pc");
        w.string("item");
        w.varint(1);
        w.string("cobblemon");
        w.varint(2);
        w.varint(1333);
        w.varint(1);
        w.string("poke_ball");
        w.varint(3);
        w.varint(1);
        w.string("rare_candy");
        // The padding of Fabric's buffer.
        w.data.extend_from_slice(&[0; 64]);
        w.data
    }

    #[test]
    fn a_fabric_server_s_table_names_each_number() {
        let table = RegistrySync::read(&sync()).unwrap();
        let blocks: Vec<_> = table.entries("minecraft:block").collect();
        assert_eq!(
            blocks,
            [
                (0, "minecraft:air"),
                (1, "minecraft:stone"),
                (1060, "cobblemon:ice_stone_ore"),
                (1061, "cobblemon:pc"),
            ]
        );
        let items: Vec<_> = table.entries("minecraft:item").collect();
        assert_eq!(
            items,
            [
                (1333, "cobblemon:poke_ball"),
                (1336, "cobblemon:rare_candy")
            ]
        );
        assert_eq!(table.entries("minecraft:menu").count(), 0);
        // Cut short, it is an error rather than half a table.
        let whole = sync();
        assert!(RegistrySync::read(&whole[..whole.len() - 80]).is_err());
    }

    #[test]
    fn a_modded_server_s_states_and_items_are_looked_up_past_the_game_s_own() {
        let modded = Modded {
            states: 59_579,
            first_state: 26_684,
            block_states: vec![40_000, 0, 40_002],
            ..Default::default()
        }
        .with_items([(1333, 1600)]);
        assert_eq!(modded.state_bits(), 16);
        assert_eq!(modded.state(26_684), Some(40_000));
        assert_eq!(modded.state(26_686), Some(40_002));
        // One of the game's own, and one past what is known: not this table's to answer.
        assert_eq!(modded.state(26_683), None);
        assert_eq!(modded.state(26_687), None);
        assert_eq!(modded.state(-1), None);
        assert_eq!(modded.items_up.get(&1333), Some(&1600));
        assert_eq!(modded.items_down.get(&1600), Some(&1333));
        // 1.21.1 on its own: 26,684 states, 15 bits.
        let vanilla = Modded {
            states: 26_684,
            ..Default::default()
        };
        assert_eq!(vanilla.state_bits(), 15);
    }
}
