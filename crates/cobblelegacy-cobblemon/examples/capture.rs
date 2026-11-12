// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Log into a 1.21.1 Cobblemon server in offline mode through the protocol adapter, and report what it
//! sends on its plugin channels: every channel with its count and bytes, each Pokémon spawn decoded, and
//! which vanilla packets then follow those entity ids, and what of their data vanilla's readers leave
//! unread (`PokemonData`).
//!
//!     cargo run -p cobblelegacy-cobblemon --example capture -- 127.0.0.1:25599 [seconds] [dump-dir]
//!
//! With a dump directory, the first payloads of each channel are written there as they came, to become
//! test fixtures. Exits with an error when a spawn does not decode to its last byte.
use cobblelegacy_cobblemon::wire::{
    PokedexData, PokemonData, SERVER_SETTINGS, SET_CLIENT_PLAYERDATA, SPAWN_POKEMON,
    ServerSettings, SpawnPokemon,
};
use horizon_protocol::{
    Direction, PROTOCOL_VERSION, State,
    adapter::{ServerVersion, UNREAD_ENTITY_DATA},
    buf::Reader,
    connection::Connection,
    ids,
    packets::{Empty, configuration as cfg, handshake, login, play},
    servers::parse_address,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Duration, Instant},
};

/// Vanilla packets that start with the entity id they are about.
const ABOUT_AN_ENTITY: [&str; 12] = [
    "move_entity_pos",
    "move_entity_pos_rot",
    "move_entity_rot",
    "teleport_entity",
    "entity_position_sync",
    "set_entity_data",
    "set_entity_motion",
    "rotate_head",
    "update_attributes",
    "set_equipment",
    "animate",
    "update_mob_effect",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (host, port) =
        parse_address(args.get(1).map_or("127.0.0.1", String::as_str)).ok_or("bad address")?;
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
    let dump: Option<PathBuf> = args.get(3).map(PathBuf::from);
    if let Some(dir) = &dump {
        std::fs::create_dir_all(dir)?;
    }

    let mut c = Connection::connect(&host, port, Duration::from_secs(5))?;
    c.reader.set_read_timeout(Some(Duration::from_secs(10)))?;
    c.adapt(ServerVersion::V1_21_1);
    c.send(
        &handshake::Serverbound::Intention(handshake::Intention {
            protocol: PROTOCOL_VERSION,
            host: host.clone(),
            port,
            intent: handshake::LOGIN,
        })
        .encode(),
    )?;
    let name = "HorizonProbe";
    c.send(
        &login::Serverbound::Hello(login::ServerHello {
            name: name.into(),
            uuid: horizon_protocol::crypto::offline_uuid(name),
        })
        .encode(),
    )?;
    loop {
        let (id, payload) = c.recv()?;
        match login::Clientbound::decode(id, &payload)? {
            login::Clientbound::Compression(p) => c.set_compression(p.threshold),
            login::Clientbound::LoginFinished(_) => {
                c.send(&login::Serverbound::LoginAcknowledged(Empty).encode())?;
                break;
            }
            login::Clientbound::Disconnect(p) => return Err(p.reason_json.into()),
            _ => {}
        }
    }
    c.send(&cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(8)).encode())?;

    // channel -> (messages, bytes)
    let mut channels: BTreeMap<String, (u32, usize)> = BTreeMap::new();
    // Fabric's registry sync, its slices put together.
    const FABRIC_SYNC: &str = "fabric:registry/sync/direct";
    let (mut sync, mut slices) = (Vec::new(), 0u32);
    let mut note = |phase: &str, channel: &str, data: &[u8]| {
        let entry = channels.entry(format!("{phase} {channel}")).or_default();
        entry.0 += 1;
        entry.1 += data.len();
        if let Some(dir) = &dump
            && entry.0 <= 3
        {
            let file = format!(
                "{phase}-{}-{}.bin",
                channel.replace([':', '/'], "_"),
                entry.0
            );
            let _ = std::fs::write(dir.join(file), data);
        }
    };
    loop {
        let (id, payload) = c.recv()?;
        match cfg::Clientbound::decode(id, &payload) {
            Ok(cfg::Clientbound::SelectKnownPacks(_)) => c.send(
                &cfg::Serverbound::SelectKnownPacks(cfg::SelectKnownPacks { packs: vec![] })
                    .encode(),
            )?,
            Ok(cfg::Clientbound::KeepAlive(p)) => {
                c.send(&cfg::Serverbound::KeepAlive(p).encode())?
            }
            Ok(cfg::Clientbound::Ping(p)) => c.send(&cfg::Serverbound::Pong(p).encode())?,
            Ok(cfg::Clientbound::CodeOfConduct(_)) => {
                c.send(&cfg::Serverbound::AcceptCodeOfConduct(Empty).encode())?
            }
            Ok(cfg::Clientbound::FinishConfiguration(_)) => {
                c.send(&cfg::Serverbound::FinishConfiguration(Empty).encode())?;
                break;
            }
            Ok(cfg::Clientbound::Disconnect(p)) => return Err(format!("{:?}", p.reason).into()),
            // Fabric's registry sync (`DirectRegistryPacketHandler`): the numbers of what the server's
            // mods registered, in slices, an empty one last.
            Ok(cfg::Clientbound::CustomPayload(p)) if p.channel == FABRIC_SYNC => {
                if p.data.is_empty() {
                    println!(
                        "fabric registry sync: {} bytes in {slices} slices",
                        sync.len()
                    );
                    if let Some(dir) = &dump {
                        let _ = std::fs::write(dir.join("fabric_registry_sync.bin"), &sync);
                    }
                    c.send(
                        &cfg::Serverbound::CustomPayload(cfg::CustomPayload {
                            channel: "fabric:registry/sync/complete".into(),
                            data: Vec::new(),
                        })
                        .encode(),
                    )?;
                } else {
                    slices += 1;
                    sync.extend_from_slice(&p.data);
                }
            }
            Ok(cfg::Clientbound::CustomPayload(p)) => {
                if p.channel == "minecraft:register" {
                    // A client that can read the sync says so, as Fabric's does.
                    c.send(
                        &cfg::Serverbound::CustomPayload(cfg::CustomPayload {
                            channel: "minecraft:register".into(),
                            data: FABRIC_SYNC.as_bytes().to_vec(),
                        })
                        .encode(),
                    )?;
                    let offered: Vec<&str> = p
                        .data
                        .split(|b| *b == 0)
                        .filter_map(|c| std::str::from_utf8(c).ok())
                        .filter(|c| !c.is_empty())
                        .collect();
                    let cobblemon = offered
                        .iter()
                        .filter(|c| c.starts_with("cobblemon:"))
                        .count();
                    println!(
                        "configuration: the server registers {} channels ({cobblemon} of cobblemon): {}",
                        offered.len(),
                        offered
                            .iter()
                            .filter(|c| !c.starts_with("cobblemon:"))
                            .copied()
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                note("configuration", &p.channel, &p.data);
            }
            _ => {}
        }
    }

    let start = Instant::now();
    c.reader
        .set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut pokemon: BTreeMap<i32, SpawnPokemon> = BTreeMap::new();
    let mut species: BTreeMap<String, u32> = BTreeMap::new();
    let mut spawn_errors = 0;
    // packet name -> (about a Pokémon, of which undecodable)
    let mut follow_ups: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let mut removed = 0;
    // (index, value's kind) of the metadata entries that arrive for Pokémon -> count
    let mut metadata: BTreeMap<(u8, String), u32> = BTreeMap::new();
    // What vanilla's readers left of a Pokémon's data: (read, not read), then what it carried -> count.
    let mut rests = (0u32, 0u32);
    // The player's Pokédex as the server sent it: messages read, and not.
    let (mut dexes, mut dex_errors) = (0u32, 0u32);
    let mut carried: BTreeMap<String, u32> = BTreeMap::new();
    while start.elapsed() < Duration::from_secs(seconds) {
        let (id, payload) = match c.recv() {
            Ok(p) => p,
            Err(e) if e.is_timed_out() => continue,
            Err(e) => return Err(e.into()),
        };
        let name = ids::name(State::Play, Direction::Clientbound, id).unwrap_or("?");
        let decoded = play::Clientbound::decode(id, &payload);
        if ABOUT_AN_ENTITY.contains(&name)
            && let Ok(entity) = Reader::new(&payload).varint()
            && pokemon.contains_key(&entity)
        {
            let entry = follow_ups.entry(name).or_default();
            entry.0 += 1;
            if decoded.is_err() {
                entry.1 += 1;
            }
        }
        match decoded {
            Ok(play::Clientbound::Login(_) | play::Clientbound::Respawn(_)) => {
                c.send(&play::Serverbound::PlayerLoaded(Empty).encode())?;
            }
            Ok(play::Clientbound::KeepAlive(p)) => {
                c.send(&play::Serverbound::KeepAlive(p).encode())?
            }
            Ok(play::Clientbound::Ping(p)) => c.send(&play::Serverbound::Pong(p).encode())?,
            Ok(play::Clientbound::PlayerPosition(p)) => {
                let [x, y, z] = p.position;
                c.send(
                    &play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                        id: p.teleport,
                        x,
                        y,
                        z,
                        yaw: p.yaw,
                        pitch: p.pitch,
                    })
                    .encode(),
                )?;
                c.send(&play::Serverbound::ClientTickEnd(Empty).encode())?;
            }
            Ok(play::Clientbound::ChunkBatchFinished(_)) => c.send(
                &play::Serverbound::ChunkBatchReceived(play::ChunkBatchReceived {
                    chunks_per_tick: 50.0,
                })
                .encode(),
            )?,
            Ok(play::Clientbound::SetEntityData(p)) if pokemon.contains_key(&p.id) => {
                for (index, _, value) in &p.values {
                    let kind = format!("{value:?}");
                    let kind = kind.split(['(', ' ', '{']).next().unwrap_or("?").to_owned();
                    *metadata.entry((*index, kind)).or_default() += 1;
                }
            }
            Ok(play::Clientbound::RemoveEntities(p)) => {
                removed += p.ids.iter().filter(|id| pokemon.contains_key(id)).count();
            }
            Ok(play::Clientbound::CustomPayload(p)) if p.channel == UNREAD_ENTITY_DATA => {
                let mut r = Reader::new(&p.data);
                let Ok(entity) = r.varint() else { continue };
                if !pokemon.contains_key(&entity) {
                    *carried.entry("(not a Pokémon's)".into()).or_default() += 1;
                    continue;
                }
                let rest = r.rest();
                match PokemonData::read_rest(rest) {
                    Ok(data) => {
                        rests.0 += 1;
                        let mut what = Vec::new();
                        if let Some(pose) = data.pose {
                            what.push(format!("pose {pose:?}"));
                        }
                        for (given, name) in [
                            (data.platform.is_some(), "platform"),
                            (data.battle.is_some(), "battle"),
                            (data.aspects.is_some(), "aspects"),
                            (data.label_level.is_some(), "label level"),
                            (data.scale_modifier.is_some(), "scale"),
                            (data.alpha.is_some(), "alpha"),
                        ] {
                            if given {
                                what.push(name.to_owned());
                            }
                        }
                        if let Some(index) = data.stopped_at {
                            what.push(format!("stopped at {index}"));
                        }
                        let what = what.join(", ");
                        // One of each kind as a fixture.
                        if let Some(dir) = &dump
                            && !carried.contains_key(&what)
                        {
                            let file = format!("pokemon_data_{}.bin", carried.len() + 1);
                            let _ = std::fs::write(dir.join(file), rest);
                            println!("pokemon data {} of {entity}: {what}", carried.len() + 1);
                        }
                        *carried.entry(what).or_default() += 1;
                    }
                    Err(e) => {
                        rests.1 += 1;
                        if rests.1 <= 3 {
                            println!("pokemon data decode error ({} bytes): {e}", rest.len());
                            if let Some(dir) = &dump {
                                let file = format!("pokemon_data_error_{}.bin", rests.1);
                                let _ = std::fs::write(dir.join(file), rest);
                            }
                        }
                    }
                }
            }
            Ok(play::Clientbound::CustomPayload(p)) => {
                note("play", &p.channel, &p.data);
                if p.channel == SERVER_SETTINGS {
                    println!("server settings: {:?}", ServerSettings::read(&p.data));
                }
                if p.channel == SET_CLIENT_PLAYERDATA {
                    match PokedexData::read(&p.data) {
                        Ok(Some(dex)) => {
                            dexes += 1;
                            println!(
                                "pokedex {dexes}: incremental {}, {} species, first {:?}",
                                dex.incremental,
                                dex.species.len(),
                                dex.species.iter().take(3).collect::<Vec<_>>()
                            );
                            // A fixture of each that carries something.
                            if let Some(dir) = &dump
                                && !dex.species.is_empty()
                            {
                                let _ = std::fs::write(
                                    dir.join(format!("pokedex_{dexes}.bin")),
                                    &p.data,
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            dex_errors += 1;
                            println!("pokedex decode error ({} bytes): {e}", p.data.len());
                        }
                    }
                }
                if p.channel == SPAWN_POKEMON {
                    match SpawnPokemon::read(&p.data) {
                        Ok(spawn) => {
                            *species.entry(spawn.species.clone()).or_default() += 1;
                            if pokemon.len() < 6 {
                                println!(
                                    "pokemon {} {} {:?} {:?} form {} aspects {:?} scale {} level {} at {:.1?} yaw {:.0} kind {}",
                                    spawn.entity.id,
                                    spawn.species,
                                    spawn.gender,
                                    spawn.pose,
                                    spawn.form,
                                    spawn.aspects,
                                    spawn.scale_modifier,
                                    spawn.label_level,
                                    spawn.entity.position,
                                    spawn.entity.yaw,
                                    spawn.entity.kind,
                                );
                            }
                            pokemon.insert(spawn.entity.id, spawn);
                        }
                        Err(e) => {
                            spawn_errors += 1;
                            if spawn_errors <= 3 {
                                println!("spawn decode error ({} bytes): {e}", p.data.len());
                            }
                        }
                    }
                }
            }
            Ok(play::Clientbound::Disconnect(p)) => {
                return Err(format!("disconnected: {:?}", p.reason).into());
            }
            _ => {}
        }
    }

    println!("--- channels (messages, bytes) ---");
    for (channel, (n, bytes)) in &channels {
        println!("{channel:64} {n:5} {bytes:9}");
    }
    println!("--- pokemon ---");
    let kinds: BTreeSet<i32> = pokemon.values().map(|p| p.entity.kind).collect();
    println!(
        "{} spawned ({} species), {} removed again, {spawn_errors} undecodable; entity type numbers {kinds:?}",
        pokemon.len(),
        species.len(),
        removed
    );
    for (name, n) in &species {
        println!("species {name} {n}");
    }
    println!("--- vanilla packets about those entities (count, undecodable) ---");
    for (name, (n, errors)) in &follow_ups {
        println!("{name:24} {n:6} {errors:6}");
    }
    println!(
        "--- metadata entries that reach the client for those entities (index, kind, count) ---"
    );
    for ((index, kind), n) in &metadata {
        println!("{index:3} {kind:16} {n:5}");
    }
    println!(
        "--- what vanilla's readers left unread of their data: {} read, {} not ---",
        rests.0, rests.1
    );
    for (what, n) in &carried {
        println!("{n:5} {what}");
    }
    if spawn_errors > 0 {
        return Err(format!("{spawn_errors} Pokémon spawns did not decode").into());
    }
    if rests.1 > 0 {
        return Err(format!("{} Pokémon data did not decode", rests.1).into());
    }
    if dex_errors > 0 {
        return Err(format!("{dex_errors} Pokédex messages did not decode").into());
    }
    Ok(())
}
