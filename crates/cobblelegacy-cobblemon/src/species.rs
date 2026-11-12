// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a species file (`data/cobblemon/species/**/charizard.json`) says of a Pokémon that the server
//! never sends with the Pokémon itself: its number, types, base stats, weight, and the moves it learns.
//!
//! The summary screen needs them for a Pokémon's stats ([`crate::stats::stat`]), the Info tab's lines
//! and the moves a Pokémon may take back (`Pokemon.relearnableMoves`). A form (`FormData`) may give its
//! own value for any of them and falls back on the species for the rest, with one exception Cobblemon
//! makes: a form that names its first type does not take the species' second one.
//!
//! Cobblemon's own client gets all of this from the server (`SpeciesRegistrySyncPacket`), so that a data
//! pack on the server changes it; here it is read from the client's files, as the party overlay's base
//! HP is.
use crate::{
    party::ExperienceGroup,
    stats::{Stat, Values},
};
use serde_json::Value;

/// What the summary reads of a species, for one of its forms.
#[derive(Clone, Debug, PartialEq)]
pub struct Species {
    /// `nationalPokedexNumber`.
    pub dex: u32,
    pub base_stats: Values,
    /// The types' Showdown ids (`fire`).
    pub primary_type: String,
    pub secondary_type: Option<String>,
    pub experience: Option<ExperienceGroup>,
    /// The species' own weight, in hectograms: what `Pokemon.getMaxFullness` asks.
    pub weight: f64,
    /// The moves learnt by level, as the file lists them: the level, and the move's Showdown id.
    pub level_up_moves: Vec<(u32, String)>,
    /// The species this one evolves from (`charmander`), whose file says what this one learns as it
    /// evolves.
    pub pre_evolution: Option<String>,
    /// Each evolution's result (`charizard`, the first word of its properties) and `learnableMoves`.
    pub evolutions: Vec<(String, Vec<String>)>,
    /// Whether it can be ridden (`riding.behaviours`), which gives the Stats tab a fifth view.
    pub rideable: bool,
}

/// `FormData.formOnlyShowdownId`: the name in lower case, without what is no letter or digit.
fn showdown_id(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl Species {
    /// The file's facts for the form `form` (its Showdown id, `normal` for the species' own).
    pub fn parse(json: &str, form: &str) -> Result<Self, String> {
        let data: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let of_form = data["forms"].as_array().and_then(|forms| {
            forms
                .iter()
                .find(|f| f["name"].as_str().is_some_and(|n| showdown_id(n) == form))
        });
        // The form's value where it gives one, else the species'.
        let pick = |key: &str| -> &Value {
            match of_form.map(|form| &form[key]) {
                Some(value) if !value.is_null() => value,
                _ => &data[key],
            }
        };
        let base = pick("baseStats");
        let mut base_stats = [0; 6];
        for stat in Stat::ALL {
            // `CobblemonStatProvider.allocate`: a stat the file leaves out is 1.
            base_stats[stat as usize] = base[stat.key()]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(1);
        }
        if !base.is_object() {
            return Err("a species without baseStats".into());
        }
        let kind = |value: &Value| value.as_str().map(str::to_ascii_lowercase);
        let form_first = of_form.and_then(|form| kind(&form["primaryType"]));
        let form_second = of_form.and_then(|form| kind(&form["secondaryType"]));
        let secondary_type = match (&form_first, form_second) {
            (None, None) => kind(&data["secondaryType"]),
            (_, second) => second,
        };
        let primary_type = form_first
            .or_else(|| kind(&data["primaryType"]))
            .ok_or("a species without a primaryType")?;
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        // `Learnset.levelUpInterpreter`: `12:dragonbreath`, a level and a move and nothing else.
        let level_up_moves = strings(pick("moves"))
            .iter()
            .filter_map(|entry| {
                let (level, name) = entry.split_once(':')?;
                (!name.contains(':')).then_some(())?;
                Some((level.parse().ok()?, name.to_owned()))
            })
            .collect();
        let evolutions = pick("evolutions")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|evolution| {
                let result = evolution["result"].as_str()?.split(' ').next()?;
                Some((
                    result.to_ascii_lowercase(),
                    strings(&evolution["learnableMoves"]),
                ))
            })
            .collect();
        Ok(Self {
            dex: data["nationalPokedexNumber"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            base_stats,
            primary_type,
            secondary_type,
            experience: pick("experienceGroup")
                .as_str()
                .and_then(ExperienceGroup::named),
            weight: data["weight"].as_f64().unwrap_or(0.0),
            level_up_moves,
            pre_evolution: pick("preEvolution")
                .as_str()
                .and_then(|name| name.split(' ').next())
                .map(str::to_ascii_lowercase),
            evolutions,
            rideable: pick("riding")["behaviours"].is_object(),
        })
    }

    /// `Learnset.getLevelUpMovesUpTo`: the moves learnt up to `level`, lowest level first and each
    /// once.
    pub fn moves_up_to(&self, level: u32) -> Vec<&str> {
        let mut learnt: Vec<&(u32, String)> = self
            .level_up_moves
            .iter()
            .filter(|(at, _)| *at <= level)
            .collect();
        learnt.sort_by_key(|(at, _)| *at);
        let mut moves: Vec<&str> = Vec::new();
        for (_, name) in learnt {
            if !moves.contains(&name.as_str()) {
                moves.push(name);
            }
        }
        moves
    }

    /// What a Pokémon of the species `result` learns as this one evolves into it
    /// (`Species.resolveEvolutionMoves`).
    pub fn evolution_moves(&self, result: &str) -> Vec<&str> {
        self.evolutions
            .iter()
            .filter(|(to, _)| to == result)
            .flat_map(|(_, moves)| moves.iter().map(String::as_str))
            .collect()
    }

    /// `Pokemon.getMaxFullness`: half a tenth of Grass Knot's power against the species' weight (which
    /// the file gives in hectograms and `getGrassKnotPower` reads as it is), and one.
    pub fn max_fullness(&self) -> u32 {
        // `getGrassKnotPower`'s own ranges: a weight between two of them is in none.
        let steps = [
            (0.1..=21.8, 20),
            (21.9..=54.9, 40),
            (55.0..=110.1, 60),
            (110.2..=220.3, 80),
            (220.4..=440.8, 100),
            (440.9..=f64::INFINITY, 120),
        ];
        let power = steps
            .iter()
            .find(|(range, _)| range.contains(&self.weight))
            .map_or(0, |&(_, power)| power);
        power / 10 / 2 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Charmeleon's file, cut to what is read, with a made-up form that changes some of it.
    const CHARMELEON: &str = r#"{
        "nationalPokedexNumber": 5, "name": "Charmeleon", "primaryType": "fire", "secondaryType": "dragon",
        "weight": 190, "experienceGroup": "medium_slow", "preEvolution": "charmander",
        "baseStats": {"hp": 58, "attack": 64, "defence": 58, "special_attack": 80,
            "special_defence": 65, "speed": 80},
        "moves": ["1:scratch", "1:growl", "12:dragonbreath", "24:slash", "19:firefang", "24:scratch",
            "egg:bite", "tm:cut", "tutor:blastburn", "x:y:z"],
        "evolutions": [{"id": "charmeleon_charizard", "variant": "level_up", "result": "charizard",
            "learnableMoves": ["wingattack", "airslash"], "requirements": []},
            {"result": "Charizard shiny=true", "learnableMoves": ["heatwave"]},
            {"result": "raichu alolan"}],
        "forms": [
            {"name": "Hisui-Bias", "primaryType": "ghost", "baseStats": {"hp": 60, "attack": 70},
             "moves": ["3:lick"], "experienceGroup": "slow", "riding": {"behaviours": {"LAND": {}}}},
            {"name": "Mega", "secondaryType": "flying"}
        ]
    }"#;

    #[test]
    fn a_species_file_gives_what_the_server_does_not_send() {
        let species = Species::parse(CHARMELEON, "normal").unwrap();
        assert_eq!(species.dex, 5);
        assert_eq!(species.base_stats, [58, 64, 58, 80, 65, 80]);
        assert_eq!(species.primary_type, "fire");
        assert_eq!(species.secondary_type.as_deref(), Some("dragon"));
        assert_eq!(species.experience, Some(ExperienceGroup::MediumSlow));
        assert_eq!(species.pre_evolution.as_deref(), Some("charmander"));
        assert!(!species.rideable);
        // Only what a level teaches, in the file's order.
        let levels: Vec<_> = species.level_up_moves.iter().map(|(l, _)| *l).collect();
        assert_eq!(levels, [1, 1, 12, 24, 19, 24]);
        // By level, each move once.
        assert_eq!(
            species.moves_up_to(20),
            ["scratch", "growl", "dragonbreath", "firefang"]
        );
        assert_eq!(
            species.moves_up_to(100),
            ["scratch", "growl", "dragonbreath", "firefang", "slash"]
        );
        assert!(species.moves_up_to(0).is_empty());
        // What the species it evolves into learns then, whatever the result's other properties.
        assert_eq!(
            species.evolution_moves("charizard"),
            ["wingattack", "airslash", "heatwave"]
        );
        assert!(species.evolution_moves("raichu").is_empty());
        assert!(species.evolution_moves("venusaur").is_empty());
    }

    #[test]
    fn a_form_gives_its_own_and_takes_the_rest_from_its_species() {
        // By its Showdown id. A form that names its first type has no second one unless it says so.
        let form = Species::parse(CHARMELEON, "hisuibias").unwrap();
        assert_eq!(form.primary_type, "ghost");
        assert_eq!(form.secondary_type, None);
        // Its own base stats, 1 for those it leaves out.
        assert_eq!(form.base_stats, [60, 70, 1, 1, 1, 1]);
        assert_eq!(form.experience, Some(ExperienceGroup::Slow));
        assert_eq!(form.moves_up_to(50), ["lick"]);
        assert!(form.rideable);
        assert_eq!((form.dex, form.weight), (5, 190.0));
        // One that names only a second type keeps the species' first.
        let mega = Species::parse(CHARMELEON, "mega").unwrap();
        assert_eq!(
            (mega.primary_type.as_str(), mega.secondary_type.as_deref()),
            ("fire", Some("flying"))
        );
        assert_eq!(mega.base_stats, [58, 64, 58, 80, 65, 80]);
        // A form the file does not have is the species itself.
        assert_eq!(
            Species::parse(CHARMELEON, "gmax").unwrap(),
            Species::parse(CHARMELEON, "normal").unwrap()
        );
        assert!(Species::parse("{}", "normal").is_err());
        assert!(Species::parse("not json", "normal").is_err());
    }

    #[test]
    fn fullness_grows_with_the_species_weight() {
        let weighing = |weight: f64| {
            let mut species = Species::parse(CHARMELEON, "normal").unwrap();
            species.weight = weight;
            species.max_fullness()
        };
        // Grass Knot's steps: 20, 40, 60, 80, 100, 120.
        assert_eq!(
            [0.0, 0.1, 21.8, 21.9, 60.0, 190.0, 220.4, 440.9, 9999.0].map(weighing),
            [1, 2, 2, 3, 4, 5, 6, 7, 7]
        );
        // Between two of its ranges, as below the first: no power.
        assert_eq!(weighing(21.85), 1);
    }
}
