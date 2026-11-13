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
//!
//! With `CAPTURE_BATTLE=<seconds>` in the environment it also fights: that long after joining it
//! challenges a wild Pokémon with the first Pokémon of its party, then answers every request with that
//! Pokémon's first move; once its own Pokémon faints (the server says nothing of a move chosen then,
//! it only waits) it switches to the next Pokémon of the party, which fights on the same way. The wild
//! Pokémon is the
//! one that appeared nearest to `CAPTURE_BATTLE_AT=x,y,z`
//! (the server refuses one farther than 12 blocks from the player without a word), or the last one
//! seen without it. From the challenge on, every message of a Cobblemon channel but the spawns is
//! written to the dump directory in the order it came (`flow-007-battle_initialize.bin`).
//!
//! With `CAPTURE_PC=x,y,z`, the place of a PC block near the player, it also uses the PC:
//! `CAPTURE_PC_AFTER` seconds after joining (20 without it) it right-clicks the block until the server
//! opens the PC's screen, then asks for one thing every second and a half: a Pokémon of the PC moved to
//! another box, two swapped, one swapped with the party's last, that one put back in the PC, one taken
//! into the party, one released, a box renamed, given another wallpaper and sorted, then the link ended
//! and one request more, which the server refuses by closing the screen. It needs a full party and two
//! Pokémon in the PC. Every message of a Cobblemon channel but the spawns is written to the dump
//! directory in the order it came (`pc-041-set_pc_pokemon.bin`), and what the PC holds is printed after
//! each one that changed it.
use cobblelegacy_cobblemon::party;
use cobblelegacy_cobblemon::pc;
use cobblelegacy_cobblemon::wire::{
    PokedexData, PokemonData, SERVER_SETTINGS, SET_CLIENT_PLAYERDATA, SPAWN_POKEMON,
    ServerSettings, SpawnPokemon,
};
use horizon_protocol::{
    Direction, PROTOCOL_VERSION, State,
    adapter::{ServerVersion, UNREAD_ENTITY_DATA},
    buf::{Reader, Writer},
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
    // A battle fought from here (see the top of the file).
    let battle_after: Option<u64> = std::env::var("CAPTURE_BATTLE")
        .ok()
        .and_then(|s| s.parse().ok());
    let battle_at: Option<[f64; 3]> = std::env::var("CAPTURE_BATTLE_AT").ok().and_then(|s| {
        let at: Vec<f64> = s.split(',').filter_map(|n| n.trim().parse().ok()).collect();
        at.try_into().ok()
    });
    let mut own = party::Storage::new();
    // The wild Pokémon seen, with where each appeared.
    let mut wild: Vec<(i32, [f64; 3])> = Vec::new();
    let mut challenged = false;
    let mut battle: Option<u128> = None;
    let mut choose: Option<Instant> = None;
    // Choices the server refused: the first is made with a target, the next without, the third is a
    // switch to the next Pokémon of the party, `fighter`.
    let mut refused = 0u32;
    let mut fighter = 0usize;
    let mut flow = 0u32;
    // A PC used from here (see the top of the file).
    let pc_at: Option<[i32; 3]> = std::env::var("CAPTURE_PC").ok().and_then(|s| {
        let at: Vec<i32> = s.split(',').filter_map(|n| n.trim().parse().ok()).collect();
        at.try_into().ok()
    });
    let pc_after: u64 = std::env::var("CAPTURE_PC_AFTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let mut pcs = pc::Storage::new();
    // The PC whose screen the server opened, the step of the script that comes next and when.
    let mut pc_open: Option<u128> = None;
    let mut pc_step = 0usize;
    let mut pc_next = Instant::now();
    let mut pc_clicked: Option<Instant> = None;
    let (mut pc_messages, mut pc_errors) = (0u32, 0u32);
    let mut sequence = 0;
    let custom = |channel: &str, data: Vec<u8>| {
        play::Serverbound::CustomPayload(cfg::CustomPayload {
            channel: channel.into(),
            data,
        })
        .encode()
    };
    while start.elapsed() < Duration::from_secs(seconds) {
        let lead = own.party().iter().flatten().nth(fighter);
        if let Some(after) = battle_after
            && !challenged
            && start.elapsed() >= Duration::from_secs(after)
            && let (Some(target), Some(lead)) = (
                match battle_at {
                    Some(at) => wild
                        .iter()
                        .min_by(|a, b| {
                            let far =
                                |p: &[f64; 3]| (0..3).map(|i| (p[i] - at[i]).powi(2)).sum::<f64>();
                            far(&a.1).total_cmp(&far(&b.1))
                        })
                        .map(|w| w.0),
                    None => wild.last().map(|w| w.0),
                },
                lead,
            )
        {
            challenged = true;
            // `BattleChallengePacket`: the entity, the leading Pokémon, then `BattleFormat.GEN_9_SINGLES`.
            let mut w = Writer::new();
            w.i32(target);
            w.uuid(lead.uuid);
            w.string("cobblemon");
            w.string("singles");
            w.nbt(&horizon_world::nbt::Tag::compound([(
                "translate",
                horizon_world::nbt::Tag::String("cobblemon.battle.types.singles".into()),
            )]));
            w.u8(1);
            w.u8(1);
            w.u8(3);
            for rule in ["Obtainable", "+Past", "+Unobtainable"] {
                w.string(rule);
            }
            w.i32(-1);
            println!("battle: challenging entity {target} with {}", lead.species);
            c.send(&custom("cobblemon:battle_challenge", w.data))?;
        }
        if let (Some(asked), Some(id)) = (choose, battle)
            && refused >= 2
            && asked.elapsed() >= Duration::from_secs(1)
            && let Some(next) = own.party().iter().flatten().nth(fighter + 1)
        {
            choose = None;
            refused = 0;
            fighter += 1;
            // `SwitchActionResponse` (type 0): the Pokémon to send in.
            let mut w = Writer::new();
            w.uuid(id);
            w.u8(1);
            w.u8(0);
            w.uuid(next.uuid);
            println!("battle: switching to {}", next.species);
            c.send(&custom("cobblemon:battle_select_actions", w.data))?;
            continue;
        }
        if let (Some(asked), Some(id), Some(lead)) = (choose, battle, lead)
            && asked.elapsed() >= Duration::from_secs(1)
        {
            choose = None;
            let first_move = match lead.tag.get("MoveSet") {
                Some(horizon_world::nbt::Tag::List(moves)) => {
                    moves.first().and_then(|m| match m.get("MoveName") {
                        Some(horizon_world::nbt::Tag::String(name)) => Some(name.to_string_lossy()),
                        _ => None,
                    })
                }
                _ => None,
            };
            if let Some(first_move) = first_move {
                // `BattleSelectActionsPacket`: the battle, then one `MoveActionResponse` (type 1).
                let mut w = Writer::new();
                w.uuid(id);
                w.u8(1);
                w.u8(1);
                w.string(&first_move);
                let target = (refused == 0).then_some("p2a");
                w.bool(target.is_some());
                if let Some(target) = target {
                    w.string(target);
                }
                w.bool(false);
                println!("battle: choosing {first_move} at {target:?}");
                c.send(&custom("cobblemon:battle_select_actions", w.data))?;
            }
        }
        if let Some(at) = pc_at
            && pc_open.is_none()
            && pc_step == 0
            && start.elapsed() >= Duration::from_secs(pc_after)
            && pc_clicked.is_none_or(|clicked| clicked.elapsed() >= Duration::from_secs(3))
        {
            pc_clicked = Some(Instant::now());
            sequence += 1;
            println!("pc: right click on the block at {at:?}");
            c.send(
                &play::Serverbound::UseItemOn(play::UseItemOn {
                    hand: 0,
                    pos: at,
                    // Its south face, at its middle.
                    face: 3,
                    cursor: [0.5, 0.5, 1.0],
                    inside: false,
                    world_border: false,
                    sequence,
                })
                .encode(),
            )?;
        }
        if let Some(store) = pc_open
            && pc_next <= Instant::now()
            && let Some(held) = pcs.pc(store)
        {
            pc_next = Instant::now() + Duration::from_millis(1500);
            match pc_request(pc_step, held, own.party()) {
                Some(request) => {
                    println!("pc: step {pc_step} asks {request:?}");
                    c.send(&custom(request.channel(), request.payload()))?;
                    pc_step += 1;
                }
                None => pc_open = None,
            }
        }
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
                if party::reads(&p.channel) {
                    let _ = own.read(&p.channel, &p.data);
                }
                if pc_at.is_some()
                    && p.channel.starts_with("cobblemon:")
                    && p.channel != SPAWN_POKEMON
                {
                    pc_messages += 1;
                    let name = p.channel.trim_start_matches("cobblemon:").replace('/', "_");
                    if let Some(dir) = &dump {
                        let file = format!("pc-{pc_messages:03}-{name}.bin");
                        let _ = std::fs::write(dir.join(file), &p.data);
                    }
                    if pc::reads(&p.channel) || pc::shares(&p.channel) {
                        match pcs.read(&p.channel, &p.data) {
                            Ok(pc::Change::Nothing) => {}
                            Ok(change) => {
                                println!(
                                    "pc {pc_messages:03} {name} {} bytes: {}",
                                    p.data.len(),
                                    match &change {
                                        pc::Change::Stores => pc_summary(&pcs),
                                        other => format!("{other:?}"),
                                    }
                                );
                                match change {
                                    pc::Change::Open { store, .. } => {
                                        pc_open = Some(store);
                                        pc_next = Instant::now() + Duration::from_millis(1500);
                                    }
                                    pc::Change::Close { .. } => pc_open = None,
                                    // As Cobblemon's client: the wallpapers it has textures for.
                                    pc::Change::WallpapersAsked => {
                                        let wallpapers = (1..=11)
                                            .map(|n| {
                                                format!(
                                                    "cobblemon:textures/gui/pc/wallpaper/basic/wallpaper_basic_{n:02}.png"
                                                )
                                            })
                                            .collect();
                                        let answer = pc::Request::Wallpapers(wallpapers);
                                        c.send(&custom(answer.channel(), answer.payload()))?;
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                pc_errors += 1;
                                println!(
                                    "pc {pc_messages:03} {name} decode error ({} bytes): {e}",
                                    p.data.len()
                                );
                            }
                        }
                    }
                    if party::reads(&p.channel) && !p.channel.ends_with("_update") {
                        let party: Vec<String> = own
                            .party()
                            .iter()
                            .map(|slot| {
                                slot.as_ref().map_or("-".to_owned(), |p| {
                                    p.species.trim_start_matches("cobblemon:").to_owned()
                                })
                            })
                            .collect();
                        println!("pc {pc_messages:03} {name}: party {}", party.join(" "));
                    }
                }
                if challenged && p.channel.starts_with("cobblemon:") && p.channel != SPAWN_POKEMON {
                    flow += 1;
                    let name = p.channel.trim_start_matches("cobblemon:").replace('/', "_");
                    println!("flow {flow:03} {name} {} bytes", p.data.len());
                    if let Some(dir) = &dump {
                        let _ =
                            std::fs::write(dir.join(format!("flow-{flow:03}-{name}.bin")), &p.data);
                    }
                    match p.channel.as_str() {
                        "cobblemon:battle_initialize" => battle = Reader::new(&p.data).uuid().ok(),
                        "cobblemon:battle_make_choice" => choose = Some(Instant::now()),
                        "cobblemon:battle_made_invalid_choice" => {
                            refused += 1;
                            choose = Some(Instant::now());
                        }
                        // `BattleFaintPacket` starts with who fainted (`p1a`): one's own is switched.
                        "cobblemon:battle_faint"
                            if p.data.get(1..3).is_some_and(|side| side == b"p1") =>
                        {
                            refused = 2;
                        }
                        "cobblemon:battle_end" => {
                            battle = None;
                            choose = None;
                        }
                        _ => {}
                    }
                }
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
                            if spawn.owner.is_none() {
                                wild.push((spawn.entity.id, spawn.entity.position));
                            }
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
    if pc_errors > 0 {
        return Err(format!("{pc_errors} messages of the PC did not decode").into());
    }
    if pc_at.is_some() && pc_step == 0 {
        return Err("the server never opened the PC".into());
    }
    Ok(())
}

/// Every Pokémon the PCs hold, each with its box and slot, and the names the boxes were given.
fn pc_summary(pcs: &pc::Storage) -> String {
    let mut held = Vec::new();
    for pc in pcs.pcs() {
        held.push(format!("{} boxes", pc.boxes.len()));
        for (number, pc_box) in pc.boxes.iter().enumerate() {
            if let Some(name) = &pc_box.name {
                held.push(format!("box {number} is {name:?}"));
            }
            for (slot, pokemon) in pc_box.slots.iter().enumerate() {
                if let Some(pokemon) = pokemon {
                    let species = pokemon.species.trim_start_matches("cobblemon:");
                    held.push(format!("{species} {number}/{slot}"));
                }
            }
        }
    }
    held.join(", ")
}

/// The step `step` of what the probe asks of an open PC, from what the PC and the party hold then;
/// none once the script is over, or when what a step needs is not there.
fn pc_request(
    step: usize,
    held: &pc::Pc,
    party: &[Option<party::PartyPokemon>],
) -> Option<pc::Request> {
    use pc::{PcPosition, Request};
    let mut stored = Vec::new();
    for (number, pc_box) in held.boxes.iter().enumerate() {
        for (slot, pokemon) in pc_box.slots.iter().enumerate() {
            if let Some(pokemon) = pokemon {
                stored.push((pokemon.uuid, PcPosition::new(number, slot)));
            }
        }
    }
    let free = |number: usize| {
        let slot = held.boxes.get(number)?.first_free()?;
        Some(PcPosition::new(number, slot))
    };
    let last = party
        .iter()
        .enumerate()
        .rev()
        .find_map(|(slot, pokemon)| Some((pokemon.as_ref()?.uuid, slot as u16)));
    let first = stored.first().copied();
    Some(match step {
        0 => {
            let (pokemon, from) = first?;
            // To the box after its own, a few slots in.
            let number = (usize::from(from.pc_box) + 1) % held.boxes.len();
            let to = PcPosition::new(number, 8);
            Request::MovePc {
                pokemon,
                from,
                to: if held.get(to).is_none() {
                    to
                } else {
                    free(number)?
                },
            }
        }
        1 => Request::SwapPc {
            first: first?,
            second: *stored.get(1)?,
        },
        2 => Request::SwapPcParty {
            party: last?,
            pc: first?,
        },
        3 => {
            let (pokemon, from) = last?;
            Request::PartyToPc {
                pokemon,
                from,
                to: free(0),
            }
        }
        4 => {
            let (pokemon, from) = first?;
            Request::PcToParty {
                pokemon,
                from,
                to: None,
            }
        }
        5 => {
            let (pokemon, at) = *stored.last()?;
            Request::ReleasePc { pokemon, at }
        }
        6 => Request::RenameBox {
            store: held.uuid,
            pc_box: 1,
            name: "Atelier".into(),
        },
        7 => Request::ChangeWallpaper {
            store: held.uuid,
            pc_box: 1,
            wallpaper: "cobblemon:textures/gui/pc/wallpaper/basic/wallpaper_basic_01.png".into(),
            alt: None,
        },
        8 => Request::SortBox {
            store: held.uuid,
            pc_box: first.map_or(0, |(_, at)| at.pc_box),
            mode: pc::SortMode::Level,
            descending: true,
        },
        9 => Request::Unlink,
        // Without a link the server answers by closing the screen.
        10 => {
            let (pokemon, from) = first?;
            Request::MovePc {
                pokemon,
                from,
                to: free(2)?,
            }
        }
        _ => return None,
    })
}
