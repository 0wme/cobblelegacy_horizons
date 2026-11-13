//! A modded Fabric server's blocks and items, matched by name to the client's (CobbleLegacy's fork).
//!
//! The protocol adapter translates an older server's numbers with tables made for the game's own
//! entries. A Fabric server with mods has more: its mods' blocks and items, numbered after the game's.
//! Three things tell what each of those is here:
//!
//! - the server's table of names by number, which Fabric sends a client that asks for it
//!   (`horizon_protocol::adapter::fabric`);
//! - a report of the server's mod blocks' state properties, made once on a server with the same mods
//!   (`HORIZON_SERVER_REPORT`: the `registry-report.json` the atelier's `releve-serveur` mod writes),
//!   because how many states a mod's block has is in its Java code and never on the wire;
//! - the client's own registries, where a content mod carried from the same jars declares those
//!   blocks and items (`docs/modpacks.md`).
//!
//! A server state becomes the client state of the block of that name whose properties say the same;
//! a property the client's block lacks is dropped, one the server's lacks keeps the client's default.
//! A block the client has none for stays air, as before.
use horizon_protocol::adapter::ServerVersion;
use horizon_protocol::adapter::fabric::{Modded, RegistrySync, Skip};
use horizon_world::registry::{BlockRegistry, BlockStateId};
use std::{collections::HashMap, io, path::Path};

/// The variable that names the server report.
pub const REPORT: &str = "HORIZON_SERVER_REPORT";

/// The variable that names the release a network's servers speak (`1.21.1`).
///
/// A status ping tells what the address answers, and behind a proxy that is the proxy: Velocity names
/// the newest release it takes and lets the player in as that, then the server it hands the player to,
/// which speaks its own release only, turns the player away ("Outdated server!"). A network's launcher
/// knows which release its servers run and says so here; the server is then not asked.
pub const VERSION: &str = "HORIZON_SERVER_VERSION";

/// The release of that name among those the adapter translates: `1.21.1`, `1.21.11`...
pub fn version_named(name: &str) -> Option<ServerVersion> {
    let name = name.trim();
    ServerVersion::ALL.into_iter().find(|v| v.name() == name)
}

/// A mod's block as the server has it: its state properties in the game's order (by name), each
/// with its values in theirs. Its states are numbered one after another, the last property varying
/// fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerBlock {
    pub properties: Vec<(String, Vec<String>)>,
}

impl ServerBlock {
    pub fn states(&self) -> usize {
        self.properties
            .iter()
            .map(|(_, values)| values.len())
            .product()
    }

    /// The value of each property in the block's `index`th state.
    fn state(&self, mut index: usize) -> Vec<(&str, &str)> {
        let mut values = vec![("", ""); self.properties.len()];
        for (slot, (name, of)) in values.iter_mut().zip(&self.properties).rev() {
            *slot = (name.as_str(), of[index % of.len()].as_str());
            index /= of.len();
        }
        values
    }
}

/// The report's blocks by name, and how many states the game itself has on that server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerReport {
    pub vanilla_states: u32,
    pub blocks: HashMap<String, ServerBlock>,
}

impl ServerReport {
    /// `registry-report.json`. A block whose states do not follow that order makes it unusable: the
    /// numbers after it could not be told.
    pub fn parse(json: &str) -> io::Result<Self> {
        let bad = |what: &str| {
            io::Error::new(io::ErrorKind::InvalidData, format!("server report: {what}"))
        };
        let root: serde_json::Value =
            serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?;
        let vanilla_states = root["vanilla_states"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad("no vanilla_states"))?;
        let mut blocks = HashMap::new();
        for block in root["blocks"].as_array().ok_or_else(|| bad("no blocks"))? {
            let id = block["id"]
                .as_str()
                .ok_or_else(|| bad("a block without an id"))?;
            if block["order"].as_str() != Some("last_fastest") {
                return Err(bad(&format!("{id}'s states are in an order not read here")));
            }
            let mut properties = Vec::new();
            for property in block["properties"].as_array().into_iter().flatten() {
                let name = property["name"]
                    .as_str()
                    .ok_or_else(|| bad("a property without a name"))?;
                let values: Vec<String> = property["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                if values.is_empty() {
                    return Err(bad(&format!("{id}'s {name} has no value")));
                }
                properties.push((name.to_owned(), values));
            }
            let block_states = ServerBlock { properties };
            if block["states"].as_u64() != Some(block_states.states() as u64) {
                return Err(bad(&format!(
                    "{id}'s states are not its properties' product"
                )));
            }
            blocks.insert(id.to_owned(), block_states);
        }
        Ok(Self {
            vanilla_states,
            blocks,
        })
    }
}

/// The client's network number of each item (`registries.json` of the resource root, `minecraft:item`).
pub fn client_items(registries_json: &str) -> io::Result<HashMap<String, i32>> {
    let root: serde_json::Value = serde_json::from_str(registries_json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(root["minecraft:item"]["entries"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, entry)| {
            let id = i32::try_from(entry["protocol_id"].as_i64()?).ok()?;
            Some((name.clone(), id))
        })
        .collect())
}

/// What the server's own numbers are here. `report` is none when there is no report: the items are
/// still matched, the blocks stay as the adapter had them.
///
/// The server's blocks are walked in the order of their numbers: the game's first, then each mod
/// block, whose first state is where the block before it ended. A mod block the report does not
/// have ends the walk, since its size is unknown: the states from there on stay air, and the count
/// of states (the width of a section's global palette) is then unknown too, so no state is matched.
pub fn modded(
    sync: &RegistrySync,
    report: Option<&ServerReport>,
    blocks: &BlockRegistry,
    items: &HashMap<String, i32>,
) -> (Modded, Summary) {
    let mut summary = Summary::default();
    let mut modded = Modded::default();
    if let Some(report) = report {
        // The client's states of each block name the server's mods have.
        let mut ours: HashMap<&str, Vec<u32>> = HashMap::new();
        for id in 0..blocks.len() as u32 {
            if let Some(state) = blocks.state(BlockStateId(id))
                && report.blocks.contains_key(state.name.as_str())
            {
                ours.entry(state.name.as_str()).or_default().push(id);
            }
        }
        let mut states = Vec::new();
        let mut whole = true;
        for (_, name) in sync.entries("minecraft:block") {
            let Some(block) = report.blocks.get(name) else {
                // One of the game's own, which come first; or a mod's block the report lacks.
                if !name.starts_with("minecraft:") {
                    summary.unknown_block = Some(name.to_owned());
                    whole = false;
                    break;
                }
                continue;
            };
            let Some(candidates) = ours.get(name) else {
                summary.blocks_missing += 1;
                states.extend(std::iter::repeat_n(0, block.states()));
                continue;
            };
            summary.blocks_matched += 1;
            let default = blocks.default_state(name).map_or(candidates[0], |id| id.0);
            for index in 0..block.states() {
                let said = block.state(index);
                // The client's state that agrees with the server's on every property both have.
                let found = candidates.iter().copied().find(|id| {
                    blocks.state(BlockStateId(*id)).is_some_and(|state| {
                        said.iter().all(|(name, value)| {
                            state
                                .properties
                                .get(*name)
                                .is_none_or(|ours| ours.as_str() == *value)
                        })
                    })
                });
                states.push(found.unwrap_or(default));
            }
        }
        if whole {
            modded.first_state = report.vanilla_states;
            modded.states = report.vanilla_states + states.len() as u32;
            modded.block_states = states;
        }
    }
    let pairs: Vec<(i32, i32)> = sync
        .entries("minecraft:item")
        .filter(|(_, name)| !name.starts_with("minecraft:"))
        .filter_map(|(server, name)| match items.get(name) {
            Some(client) => Some((server, *client)),
            None => {
                summary.items_missing += 1;
                None
            }
        })
        .collect();
    summary.items_matched = pairs.len();
    // The mods' item components: the client holds none, and reads past their values.
    modded.components = sync
        .entries("minecraft:data_component_type")
        .filter(|(_, name)| !name.starts_with("minecraft:"))
        .map(|(server, name)| {
            let skip = if BOOLEAN_COMPONENTS.contains(&name) {
                Skip::Bool
            } else {
                Skip::Nbt
            };
            (server, skip)
        })
        .collect();
    // The mods' sound events: the client numbers none of them, and plays them by name.
    modded.sounds = sync
        .entries("minecraft:sound_event")
        .filter(|(_, name)| !name.starts_with("minecraft:"))
        .map(|(server, name)| (server, name.to_owned()))
        .collect();
    summary.sounds = modded.sounds.len();
    (modded.with_items(pairs), summary)
}

/// The mods' item components whose value is a boolean and not an NBT tag
/// (`CobblemonItemComponents`: `crafted` is `networkSynchronized(ByteBufCodecs.BOOL)`, its twelve
/// others `ByteBufCodecs.fromCodec`).
const BOOLEAN_COMPONENTS: [&str; 1] = ["cobblemon:crafted"];

/// What [`modded`] found, for the log.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub blocks_matched: usize,
    /// The server's mod blocks the client has no block for.
    pub blocks_missing: usize,
    /// The mod block that ended the walk, the report having none of that name.
    pub unknown_block: Option<String>,
    pub items_matched: usize,
    pub items_missing: usize,
    /// The server's mod sound events, which the session names from now on.
    pub sounds: usize,
}

/// [`modded`] from the client's resource root, with the report [`REPORT`] names if it does.
pub fn from_resources(sync: &RegistrySync, resources: &Path) -> io::Result<(Modded, Summary)> {
    let report = match std::env::var_os(REPORT) {
        Some(path) => Some(ServerReport::parse(&std::fs::read_to_string(path)?)?),
        None => None,
    };
    let blocks = BlockRegistry::from_facts_json(io::BufReader::new(std::fs::File::open(
        resources.join("facts.json"),
    )?))?;
    let items = client_items(&std::fs::read_to_string(resources.join("registries.json"))?)?;
    Ok(modded(sync, report.as_ref(), &blocks, &items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_s_release_is_found_by_its_name() {
        assert!(version_named("1.21.1") == Some(ServerVersion::V1_21_1));
        assert!(version_named(" 1.21.11\n") == Some(ServerVersion::V1_21_11));
        // The pinned release has a name too: nothing is translated for it.
        assert!(version_named(horizon_protocol::GAME_VERSION) == Some(ServerVersion::Current));
        assert!(version_named("1.8").is_none());
        assert!(version_named("").is_none());
    }

    const REPORT_JSON: &str = r#"{
        "vanilla_states": 26684,
        "blocks": [
            {"id": "cobblemon:apricorn_log", "states": 3, "order": "last_fastest",
             "properties": [{"name": "axis", "values": ["x", "y", "z"]}]},
            {"id": "cobblemon:pc", "states": 8, "order": "last_fastest",
             "properties": [{"name": "facing", "values": ["north", "south", "west", "east"]},
                            {"name": "on", "values": ["true", "false"]}]},
            {"id": "cobblemon:apricorn_planks", "states": 1, "order": "last_fastest", "properties": []}
        ]
    }"#;

    /// The client's registry: stone, then a log with an axis and planks from a content mod. No PC.
    fn registry() -> BlockRegistry {
        let facts = r#"{"states": [
            ["minecraft:stone", false, false, "", {"id": "minecraft:stone"}],
            ["cobblemon:apricorn_log", false, false, "", {"id": "cobblemon:apricorn_log", "properties": {"axis": "x"}}],
            ["cobblemon:apricorn_log", false, false, "", {"id": "cobblemon:apricorn_log", "properties": {"axis": "y"}}],
            ["cobblemon:apricorn_log", false, false, "", {"id": "cobblemon:apricorn_log", "properties": {"axis": "z"}}],
            ["cobblemon:apricorn_planks", false, false, "", {"id": "cobblemon:apricorn_planks"}]
        ], "defaults": {"minecraft:stone": 0, "cobblemon:apricorn_log": 2, "cobblemon:apricorn_planks": 4}}"#;
        BlockRegistry::from_facts_json(facts.as_bytes()).unwrap()
    }

    fn sync() -> RegistrySync {
        let mut table = RegistrySync::default();
        table.registries.insert(
            "minecraft:block".into(),
            [
                (0, "minecraft:stone"),
                (1060, "cobblemon:pc"),
                (1061, "cobblemon:apricorn_log"),
                (1062, "cobblemon:apricorn_planks"),
            ]
            .map(|(n, name)| (n, name.to_owned()))
            .into(),
        );
        table.registries.insert(
            "minecraft:data_component_type".into(),
            [
                (0, "minecraft:custom_data"),
                (57, "cobblemon:flavour"),
                (67, "cobblemon:crafted"),
            ]
            .map(|(n, name)| (n, name.to_owned()))
            .into(),
        );
        table.registries.insert(
            "minecraft:item".into(),
            [
                (1, "minecraft:stone"),
                (1333, "cobblemon:poke_ball"),
                (1334, "cobblemon:pc"),
            ]
            .map(|(n, name)| (n, name.to_owned()))
            .into(),
        );
        table.registries.insert(
            "minecraft:sound_event".into(),
            [
                (0, "minecraft:entity.allay.ambient_with_item"),
                (1611, "cobblemon:poke_ball.throw"),
                (1612, "cobblemon:poke_ball.recall"),
            ]
            .map(|(n, name)| (n, name.to_owned()))
            .into(),
        );
        table
    }

    #[test]
    fn a_server_block_s_states_count_with_the_last_property_fastest() {
        let report = ServerReport::parse(REPORT_JSON).unwrap();
        let pc = &report.blocks["cobblemon:pc"];
        assert_eq!(pc.states(), 8);
        assert_eq!(pc.state(0), [("facing", "north"), ("on", "true")]);
        assert_eq!(pc.state(1), [("facing", "north"), ("on", "false")]);
        assert_eq!(pc.state(6), [("facing", "east"), ("on", "true")]);
        assert_eq!(report.blocks["cobblemon:apricorn_planks"].states(), 1);
        // A count that is not the properties' product, or another order, is refused.
        assert!(
            ServerReport::parse(&REPORT_JSON.replace("\"states\": 8", "\"states\": 9")).is_err()
        );
        assert!(
            ServerReport::parse(&REPORT_JSON.replacen("last_fastest", "scattered", 1)).is_err()
        );
    }

    #[test]
    fn a_server_s_mod_states_and_items_become_the_client_s_by_name() {
        let report = ServerReport::parse(REPORT_JSON).unwrap();
        let items = HashMap::from([
            ("minecraft:stone".to_owned(), 1),
            ("cobblemon:poke_ball".to_owned(), 1600),
        ]);
        let (modded, summary) = modded(&sync(), Some(&report), &registry(), &items);
        // The PC's 8 states come first on the server and the client has no such block: air. Then
        // the log's three, by axis, and the planks.
        assert_eq!(modded.first_state, 26684);
        assert_eq!(modded.states, 26684 + 12);
        assert_eq!(modded.block_states, [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(modded.state(26684 + 9), Some(2));
        assert_eq!(modded.items_up, HashMap::from([(1333, 1600)]));
        assert_eq!(modded.items_down, HashMap::from([(1600, 1333)]));
        // The table's item components of mods are read past: a boolean for `crafted`, a tag else.
        assert_eq!(
            modded.components,
            HashMap::from([(57, Skip::Nbt), (67, Skip::Bool)])
        );
        // The mods' sounds keep their names; the game's own have their numbers in the adapter's tables.
        assert_eq!(
            modded.sounds,
            HashMap::from([
                (1611, "cobblemon:poke_ball.throw".to_owned()),
                (1612, "cobblemon:poke_ball.recall".to_owned()),
            ])
        );
        assert_eq!(
            summary,
            Summary {
                blocks_matched: 2,
                blocks_missing: 1,
                unknown_block: None,
                items_matched: 1,
                items_missing: 1,
                sounds: 2,
            }
        );
    }

    #[test]
    fn without_the_report_or_with_a_block_it_lacks_no_state_is_matched() {
        let items = HashMap::from([("cobblemon:poke_ball".to_owned(), 1600)]);
        let (modded, _) = super::modded(&sync(), None, &registry(), &items);
        assert_eq!((modded.states, modded.block_states.len()), (0, 0));
        assert_eq!(modded.items_up.len(), 1);
        // A mod's block the report has not: where the ones after it start is unknown.
        let mut report = ServerReport::parse(REPORT_JSON).unwrap();
        report.blocks.remove("cobblemon:apricorn_log");
        let (modded, summary) = super::modded(&sync(), Some(&report), &registry(), &items);
        assert_eq!((modded.states, modded.block_states.len()), (0, 0));
        assert_eq!(
            summary.unknown_block.as_deref(),
            Some("cobblemon:apricorn_log")
        );
    }

    #[test]
    fn a_property_only_one_side_has_does_not_prevent_the_match() {
        // The server's log also says whether it is stripped; the client's has the axis only.
        let json = REPORT_JSON.replace(
            r#"{"id": "cobblemon:apricorn_log", "states": 3, "order": "last_fastest",
             "properties": [{"name": "axis", "values": ["x", "y", "z"]}]}"#,
            r#"{"id": "cobblemon:apricorn_log", "states": 6, "order": "last_fastest",
             "properties": [{"name": "axis", "values": ["x", "y", "z"]},
                            {"name": "stripped", "values": ["true", "false"]}]}"#,
        );
        let report = ServerReport::parse(&json).unwrap();
        assert_eq!(report.blocks["cobblemon:apricorn_log"].states(), 6);
        let (modded, _) = modded(&sync(), Some(&report), &registry(), &HashMap::new());
        assert_eq!(modded.block_states[8..14], [1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn the_client_s_items_are_read_from_its_registries() {
        let items = client_items(
            r#"{"minecraft:item": {"entries": {"minecraft:stone": {"protocol_id": 1},
                "cobblemon:poke_ball": {"protocol_id": 1600}}}}"#,
        )
        .unwrap();
        assert_eq!(items["cobblemon:poke_ball"], 1600);
        assert_eq!(items.len(), 2);
    }
}
