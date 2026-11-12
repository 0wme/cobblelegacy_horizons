//! A Pokémon's label (CobbleLegacy's fork): its name and its level above it, while the player looks at it.
//!
//! `PokemonRenderer.shouldRenderLabel` and `renderNameTag` of Cobblemon 1.8.1. A Pokémon never shows a
//! name the way a mob does (`shouldShowName` is false): its label shows when the player's view goes
//! through its box within ten blocks, whatever stands in between, and says the species' name, or `???`
//! for a species the player's Pokédex does not have, then `Lv.` and the level. It is smaller near the
//! Pokémon and is pushed along the view as it gets farther. The window's name tags draw it (`names.rs`),
//! so its faded copy through walls has their opacity, 26.3's, where Cobblemon has 1.21.1's fainter one.
//!
//! Cobblemon reads two of its switches from the client's own configuration; they are fixed here at
//! their defaults, which CobbleLegacy's launcher keeps: a species the Pokédex lacks is not named
//! (`displayNameForUnknownPokemon`), and labels do not wait for the player to crouch
//! (`displayEntityLabelsWhenCrouchingOnly`).
//!
//! Not here: the title a mark gives a name (`getTitledName`), the line that tells which key starts a
//! battle (`showChallengeLabel`), a team's rule on name tags, a Pokémon that carries a rider, and the
//! Pokédex scanner, which hides labels while it is open.
use super::super::super::{
    Game,
    names::{Marks, NameTag, name_tag_at},
    ray_box,
};
use cobblelegacy_cobblemon::wire::{
    DexKnowledge, PokedexData, PokemonData, ServerSettings, SpawnPokemon,
};
use horizon_client::{mobs::Species, text::Text};
use horizon_protocol::entity_data::Value;
use horizon_world::nbt::Tag;
use std::collections::{HashMap, HashSet};

/// `Entity.isLookingAt`'s reach, in blocks.
const LOOK_REACH: f64 = 10.0;
/// `CobblemonConfig.displayNameForUnknownPokemon`, at its default.
const NAMES_UNKNOWN_SPECIES: bool = false;
/// `PokemonRenderer.HIDDEN_NAME`.
const HIDDEN_NAME: &str = "???";

/// What the labels of a server's Pokémon read.
#[derive(Default)]
pub(super) struct Labels {
    /// What each Pokémon in view says of itself, by entity id.
    of: HashMap<i32, Label>,
    /// The server's switches for the name and the level (`ServerSettingsPacket`); both on until it
    /// says.
    settings: Option<ServerSettings>,
    /// The species the player's Pokédex has, seen or owned (`ClientPokedexManager`).
    known: HashSet<String>,
}

/// What one Pokémon's label is made of: `PokemonEntity`'s synched data, first as it appeared.
#[derive(Clone, Debug, PartialEq)]
struct Label {
    /// SPECIES: `cobblemon:pikachu`.
    species: String,
    /// NICKNAME, when it is not empty.
    nickname: Option<Text>,
    /// NICKNAME_VISIBLE.
    nickname_visible: bool,
    /// SHOULD_RENDER_NAME.
    shown: bool,
    /// HIDE_LABEL.
    hidden: bool,
    /// LABEL_LEVEL.
    level: i32,
    /// PHASING_TARGET_ID names an entity: the Pokémon is being sent out or called back.
    phasing: bool,
}

/// A nickname's component as text; none for the empty one a Pokémon without a nickname has.
fn nickname(component: &Tag) -> Option<Text> {
    let text = horizon_client::entity_nbt::name_text(component);
    (!text.plain().is_empty()).then_some(text)
}

/// `Species.translatedName`: the key is the namespace, then the species' name without what is no
/// letter or digit (`ShowdownIdentifiable.REGEX`).
pub(super) fn species_name(species: &str) -> Text {
    let (namespace, path) = species.split_once(':').unwrap_or(("cobblemon", species));
    let id: String = path
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    Text::tr(&format!("{namespace}.species.{id}.name"), &[])
}

impl Label {
    /// `PokemonEntity.getName`: the nickname while it shows, else the species.
    fn name(&self) -> Text {
        match &self.nickname {
            Some(nickname) if self.nickname_visible => nickname.clone(),
            _ => species_name(&self.species),
        }
    }

    /// `resolveBaseLabel`, then the level `renderNameTag` puts after it: `names` and `levels` are the
    /// server's switches, `known` whether the player's Pokédex has the species.
    fn text(&self, names: bool, levels: bool, known: bool) -> Text {
        let mut label = Text::empty();
        if names {
            label.push(if known || NAMES_UNKNOWN_SPECIES {
                self.name()
            } else {
                Text::literal(HIDDEN_NAME)
            });
        }
        if levels && self.level > 0 {
            if names {
                label.push(Text::literal(" "));
            }
            // `LEVEL_LABEL_STYLE`: white, whatever colour a nickname has.
            label.push(Text::tr("cobblemon.label.lv", &[&self.level]).with_color(0xFF_FFFF));
        }
        label
    }
}

/// `renderNameTag`'s numbers for a Pokémon whose feet are `distance_sqr` from the camera: the world
/// size of a text pixel, how far the label goes along the view, and how far up the camera's up.
fn placement(distance_sqr: f64) -> (f32, f64, f64) {
    // `d.remap(-16..96, 0..1)`, kept within 0.65 and 1.5; then that, remapped to 0..1.
    let scale = ((distance_sqr + 16.0) / 112.0).clamp(0.65, 1.5);
    let t = (scale - 0.65) / (1.5 - 0.65);
    ((0.025 * (0.5 + 0.5 * t)) as f32, scale + t, t / 2.0)
}

impl Labels {
    /// A Pokémon appeared: its label starts from what `SpawnPokemonPacket` says, the rest at
    /// `defineSynchedData`'s defaults.
    pub(super) fn appeared(&mut self, id: i32, spawn: &SpawnPokemon) {
        self.of.insert(
            id,
            Label {
                species: spawn.species.clone(),
                nickname: spawn.nickname.as_ref().and_then(nickname),
                nickname_visible: true,
                shown: true,
                hidden: spawn.hide_label,
                level: spawn.label_level,
                phasing: spawn.phasing_target != -1,
            },
        );
    }

    pub(super) fn forget(&mut self, id: i32) {
        self.of.remove(&id);
    }

    /// The highest level the server lets a Pokémon reach (`ServerSettings.maxPokemonLevel`), 100 until
    /// it says.
    pub(super) fn max_level(&self) -> u32 {
        self.settings
            .and_then(|s| u32::try_from(s.max_pokemon_level).ok())
            .unwrap_or(100)
    }

    /// `ServerSettingsPacket`.
    pub(super) fn settings(&mut self, payload: &[u8]) {
        match ServerSettings::read(payload) {
            Ok(settings) => self.settings = Some(settings),
            Err(e) => log::warn!("Horizon: Cobblemon's server settings do not decode: {e}"),
        }
    }

    /// `SetClientPlayerDataPacket` of the Pokédex: all of it (`runAction`), or what changed
    /// (`runIncremental`, a species' record taking the place of the one before).
    pub(super) fn pokedex(&mut self, payload: &[u8]) {
        match PokedexData::read(payload) {
            Ok(Some(dex)) => {
                if !dex.incremental {
                    self.known.clear();
                }
                for (species, knowledge) in dex.species {
                    if knowledge == DexKnowledge::Unregistered {
                        self.known.remove(&species);
                    } else {
                        self.known.insert(species);
                    }
                }
            }
            // The player's other data, which nothing here reads.
            Ok(None) => {}
            Err(e) => log::warn!("Horizon: the player's Pokédex does not decode: {e}"),
        }
    }

    /// What of a Pokémon's own data (`remote_pokemon_data`) its label reads.
    pub(super) fn own_data(&mut self, id: i32, data: &PokemonData) {
        let Some(label) = self.of.get_mut(&id) else {
            return;
        };
        if let Some(level) = data.label_level {
            label.level = level;
        }
        if let Some(hidden) = data.hide_label {
            label.hidden = hidden;
        }
    }

    /// What of the data vanilla's readers know its label reads, at a 1.21.1 server's indexes (the
    /// adapter knows no layout for a Pokémon, so nothing renumbers them): SPECIES, NICKNAME,
    /// NICKNAME_VISIBLE, SHOULD_RENDER_NAME and PHASING_TARGET_ID.
    pub(super) fn vanilla_data(&mut self, id: i32, values: &[(u8, i32, Value)]) {
        let Some(label) = self.of.get_mut(&id) else {
            return;
        };
        for (index, _, value) in values {
            match (*index, value) {
                (19, Value::String(species)) => label.species.clone_from(species),
                (20, Value::Text(component)) => label.nickname = nickname(component),
                (21, Value::Bool(visible)) => label.nickname_visible = *visible,
                (23, Value::Bool(shown)) => label.shown = *shown,
                (26, Value::Int(target)) => label.phasing = *target != -1,
                _ => {}
            }
        }
    }
}

impl Game<'_> {
    /// The labels of the Pokémon the player looks at, among the frame's name tags (`names.rs`).
    pub(in super::super::super) fn pokemon_name_tags(
        &self,
        camera: glam::DVec3,
        alpha: f64,
        tags: &mut Vec<NameTag>,
    ) {
        let Some(labels) = self.remote.as_ref().map(|r| &r.pokemon.labels) else {
            return;
        };
        // `Minecraft.renderNames`: none with the HUD hidden.
        if labels.of.is_empty() || self.ui.hide_gui {
            return;
        }
        // `Entity.isLookingAt`: from the player's eyes along their own view.
        let Some((eye, view)) = self.aim() else {
            return;
        };
        let (names, levels) = labels.settings.map_or((true, true), |s| {
            (s.display_entity_name_label, s.display_entity_level_label)
        });
        let [forward, _, up] = self.view_axes();
        let phases = self.remote.as_ref().map(|r| &r.pokemon.phases);
        for m in &self.mobs {
            if m.species() != Species::Pokemon || !m.alive() || m.invisible {
                continue;
            }
            let Some(label) = labels.of.get(&(m.id as i32)) else {
                continue;
            };
            if !label.shown || label.hidden || label.phasing {
                continue;
            }
            // In its Ball still: the server takes the thrower back a moment before the Ball lands here.
            if phases.is_some_and(|p| p.unseen(m.id as i32)) {
                continue;
            }
            // `LivingEntityRenderer.shouldShowName` and `renderNameTag`: within 64 blocks.
            let feet = glam::DVec3::new(m.position.x, m.position.y, m.position.z);
            let distance_sqr = feet.distance_squared(camera);
            if distance_sqr >= 64.0 * 64.0 || ray_box(eye, view, LOOK_REACH, m.bounds()).is_none() {
                continue;
            }
            let text = label.text(names, levels, labels.known.contains(&label.species));
            if text.plain().is_empty() {
                continue;
            }
            let (unit, along, lift) = placement(distance_sqr);
            tags.push(NameTag {
                // Half a block over the box, as any name tag, then along the camera's own axes.
                at: name_tag_at(m, alpha) + forward * along + up * lift,
                lines: vec![(text.resolve_legacy(), 0.0)],
                // `LightTexture.pack(15, 15)`.
                light: 0xFF,
                discrete: false,
                marks: Marks::default(),
                unit,
            });
        }
    }

    /// The entries of a mob's data vanilla's readers know (`remote_mob_data`): a Pokémon's label
    /// reads some.
    pub(in super::super::super) fn remote_pokemon_label_data(
        &mut self,
        id: i32,
        values: &[(u8, i32, Value)],
    ) {
        let Some(r) = self.remote.as_mut() else {
            return;
        };
        r.pokemon.labels.vanilla_data(id, values);
        r.pokemon.phases.vanilla_data(id, values);
        // `BEAM_MODE` is a byte, which vanilla's readers know: it comes here when nothing of a type
        // of Cobblemon's own is before it in the packet (a recall sets the target and the mode, no
        // more), and with the rest of a Pokémon's data otherwise (`remote_pokemon_data`).
        let mode = values.iter().find_map(|(index, _, value)| match value {
            Value::Byte(mode) if *index == 28 => Some(*mode),
            _ => None,
        });
        if let Some(mode) = mode
            && r.pokemon.seen.contains_key(&id)
        {
            self.pokemon_phase(id, mode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_client::lang::{self, Language, ResourceSource};

    /// Cobblemon's text for two species and the level, in English.
    struct Cobblemon;
    impl ResourceSource for Cobblemon {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            (path == "assets/cobblemon/lang/en_us.json").then(|| {
                br#"{"cobblemon.species.pikachu.name": "Pikachu",
                     "cobblemon.species.mrmime.name": "Mr. Mime",
                     "cobblemon.label.lv": "Lv. %1$s"}"#
                    .to_vec()
            })
        }
        fn namespaces(&self) -> Vec<String> {
            vec!["cobblemon".into()]
        }
    }

    fn label(species: &str, level: i32) -> Label {
        Label {
            species: species.into(),
            nickname: None,
            nickname_visible: true,
            shown: true,
            hidden: false,
            level,
            phasing: false,
        }
    }

    #[test]
    fn a_label_names_a_species_the_pokedex_has_and_hides_another() {
        let ((), missed) = lang::scoped(Language::load(&[&Cobblemon], "en_us"), || {
            let pikachu = label("cobblemon:pikachu", 5);
            assert_eq!(pikachu.text(true, true, true).plain(), "Pikachu Lv. 5");
            // Not in the Pokédex: the level shows, not the name.
            assert_eq!(pikachu.text(true, true, false).plain(), "??? Lv. 5");
            // The server's switches: no level, no name (no space before the level then), neither.
            assert_eq!(pikachu.text(true, false, true).plain(), "Pikachu");
            assert_eq!(pikachu.text(false, true, true).plain(), "Lv. 5");
            assert_eq!(pikachu.text(false, false, true).plain(), "");
            // A level of 0 or less is not shown.
            assert_eq!(
                label("cobblemon:pikachu", 0).text(true, true, true).plain(),
                "Pikachu"
            );
            // The key is the species' name without what is no letter or digit.
            assert_eq!(species_name("cobblemon:mr_mime").plain(), "Mr. Mime");
            assert_eq!(species_name("pikachu").plain(), "Pikachu");
            // A nickname takes the species' place while it shows, known species or not hidden.
            let mut named = label("cobblemon:pikachu", 12);
            named.nickname = Some(Text::literal("Sparky"));
            assert_eq!(named.text(true, true, true).plain(), "Sparky Lv. 12");
            assert_eq!(named.text(true, true, false).plain(), "??? Lv. 12");
            named.nickname_visible = false;
            assert_eq!(named.text(true, true, true).plain(), "Pikachu Lv. 12");
        });
        assert_eq!(missed, Vec::<String>::new());
    }

    #[test]
    fn a_label_is_smaller_near_and_goes_along_the_view_with_the_distance() {
        // Up to 7.6 blocks (d = 0.65 * 112 - 16): half the size of a name tag, 0.65 blocks along.
        for d in [0.0, 9.0, 56.0] {
            let (unit, along, lift) = placement(d);
            assert_eq!((unit, along, lift), (0.0125, 0.65, 0.0), "at {d}");
        }
        // At 10 blocks, where the look ends: (100 + 16) / 112.
        let (unit, along, lift) = placement(100.0);
        let scale = 116.0 / 112.0;
        let t = (scale - 0.65) / 0.85;
        assert!((along - (scale + t)).abs() < 1e-12 && (lift - t / 2.0).abs() < 1e-12);
        assert!((unit - 0.025 * (0.5 + 0.5 * t as f32)).abs() < 1e-7);
        // Past 12.3 blocks (seen from a third person camera): a name tag's size, 2.5 blocks along.
        assert_eq!(placement(4000.0), (0.025, 2.5, 0.5));
    }

    #[test]
    fn the_pokedex_and_a_pokemon_s_data_change_what_its_label_reads() {
        let mut labels = Labels::default();
        // `pokedex grant` of a Pikachu and a Bulbasaur, as a 1.8.1 server sent it at login, then the
        // Pikachu's update on its own (`cobblelegacy_cobblemon`'s fixtures).
        const AT_LOGIN: &[u8] =
            include_bytes!("../../../cobblelegacy-cobblemon/tests/fixtures/pokedex_at_login.bin");
        const ONE_MORE: &[u8] =
            include_bytes!("../../../cobblelegacy-cobblemon/tests/fixtures/pokedex_one_more.bin");
        labels.known.insert("cobblemon:mew".into());
        labels.pokedex(ONE_MORE);
        assert!(
            labels.known.contains("cobblemon:mew") && labels.known.contains("cobblemon:pikachu")
        );
        labels.pokedex(AT_LOGIN);
        assert!(!labels.known.contains("cobblemon:mew"));
        assert!(labels.known.contains("cobblemon:bulbasaur"));
        // A record without a form the Pokédex has takes the species out.
        let mut unregistered = ONE_MORE.to_vec();
        *unregistered.last_mut().unwrap() = 0;
        labels.pokedex(&unregistered);
        assert!(!labels.known.contains("cobblemon:pikachu"));

        labels.of.insert(7, label("cobblemon:pikachu", 5));
        labels.own_data(
            7,
            &PokemonData {
                label_level: Some(6),
                hide_label: Some(true),
                ..Default::default()
            },
        );
        labels.vanilla_data(
            7,
            &[
                (19, 0, Value::String("cobblemon:raichu".into())),
                (21, 0, Value::Bool(false)),
                (23, 0, Value::Bool(false)),
                (26, 0, Value::Int(42)),
                // MOVING, which is not the label's.
                (24, 0, Value::Bool(true)),
            ],
        );
        assert_eq!(
            labels.of[&7],
            Label {
                species: "cobblemon:raichu".into(),
                nickname: None,
                nickname_visible: false,
                shown: false,
                hidden: true,
                level: 6,
                phasing: true,
            }
        );
        labels.vanilla_data(7, &[(26, 0, Value::Int(-1))]);
        assert!(!labels.of[&7].phasing);
        // Data for an entity that is no Pokémon in view changes nothing.
        labels.own_data(8, &PokemonData::default());
        labels.forget(7);
        assert!(labels.of.is_empty());
    }
}
