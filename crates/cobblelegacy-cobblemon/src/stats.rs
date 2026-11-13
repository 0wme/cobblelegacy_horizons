// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Pokémon's permanent stats, as Cobblemon 1.8.1 keeps and works them out.
//!
//! The server sends a Pokémon's IVs, EVs and nature, never its stats: the client works each one out
//! from them, the level and the form's base stats (`CobblemonStatProvider.getStatForPokemon`), which is
//! what the summary's Stats tab shows. [`stat`] is that function; [`Ivs`] and the EVs are read from the
//! compounds `IVs.CODEC` and `EVs.CODEC` write, in a Pokémon's own compound as in the messages that
//! update them (`cobblemon:iv_update`, `cobblemon:ev_update`).
//!
//! A nature raises one stat by a tenth and lowers another as much; the twenty-five of them are
//! `Natures`' own ([`nature`]). A mint changes the nature that counts, not the one the Pokémon has
//! (`Pokemon.effectiveNature`).
use horizon_world::nbt::Tag;

/// `Stats`' permanent ones, in the order Cobblemon declares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stat {
    Hp,
    Attack,
    Defence,
    SpecialAttack,
    SpecialDefence,
    Speed,
}

impl Stat {
    pub const ALL: [Self; 6] = [
        Self::Hp,
        Self::Attack,
        Self::Defence,
        Self::SpecialAttack,
        Self::SpecialDefence,
        Self::Speed,
    ];

    /// The stat's name in a species file's `baseStats` (`special_attack`), which its identifier
    /// (`cobblemon:special_attack`) ends with.
    pub fn key(self) -> &'static str {
        match self {
            Self::Hp => "hp",
            Self::Attack => "attack",
            Self::Defence => "defence",
            Self::SpecialAttack => "special_attack",
            Self::SpecialDefence => "special_defence",
            Self::Speed => "speed",
        }
    }

    /// `Stats.getStat` by identifier: `cobblemon:hp`, or `hp` alone.
    pub fn named(id: &str) -> Option<Self> {
        let key = id.strip_prefix("cobblemon:").unwrap_or(id);
        Self::ALL.into_iter().find(|stat| stat.key() == key)
    }
}

/// One number for each permanent stat, by `Stat as usize`.
pub type Values = [u32; 6];

/// The numbers of a map `Stat.PERMANENT_ONLY_CODEC` keys (`{"cobblemon:hp": 4, ...}`): each stat's,
/// when the map has it.
fn values(map: Option<&Tag>) -> [Option<u32>; 6] {
    Stat::ALL.map(
        |stat| match map?.get(&format!("cobblemon:{}", stat.key())) {
            Some(&Tag::Int(value)) => u32::try_from(value).ok(),
            _ => None,
        },
    )
}

/// `EVs.CODEC`: the map's numbers, none for a stat it leaves out (`EVs.defaultValue`).
pub fn evs(map: Option<&Tag>) -> Values {
    values(map).map(Option::unwrap_or_default)
}

/// `IVs`: the ones a Pokémon was born with, and those a bottle cap set over them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ivs {
    pub base: Values,
    /// `hyperTrainedIVs`: none for a stat that was not trained.
    pub hyper_trained: [Option<u32>; 6],
}

impl Ivs {
    /// `IVs.MAX_VALUE`.
    pub const MAX: u32 = 31;

    /// `IVs.CODEC`: `Base` and `HyperTrained`, each a map of stats.
    pub fn from_tag(tag: Option<&Tag>) -> Self {
        let base = values(tag.and_then(|ivs| ivs.get("Base"))).map(Option::unwrap_or_default);
        let trained = values(tag.and_then(|ivs| ivs.get("HyperTrained")));
        Self {
            base,
            // `setHyperTrainedIV`: one trained to what it already was is not trained.
            hyper_trained: std::array::from_fn(|i| trained[i].filter(|&value| value != base[i])),
        }
    }

    /// `getEffectiveBattleIV`: the trained one over the base one.
    pub fn effective(&self, stat: Stat) -> u32 {
        self.hyper_trained[stat as usize].unwrap_or(self.base[stat as usize])
    }
}

/// `EVs.MAX_STAT_VALUE`.
pub const MAX_EV: u32 = 252;

/// `Nature`: which stat it raises and which it lowers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nature {
    /// The identifier's path (`bold`), which names its translation (`cobblemon.nature.bold`).
    pub name: &'static str,
    pub increased: Option<Stat>,
    pub decreased: Option<Stat>,
}

impl Nature {
    /// `modifyStat`: a tenth more for the stat it raises, a tenth less for the one it lowers, each
    /// rounded down.
    pub fn modify(&self, stat: Stat, value: u32) -> u32 {
        let by = if self.increased == Some(stat) {
            1.1
        } else if self.decreased == Some(stat) {
            0.9
        } else {
            return value;
        };
        (f64::from(value) * by).floor() as u32
    }
}

const fn of(name: &'static str, stats: Option<(Stat, Stat)>) -> Nature {
    match stats {
        Some((increased, decreased)) => Nature {
            name,
            increased: Some(increased),
            decreased: Some(decreased),
        },
        None => Nature {
            name,
            increased: None,
            decreased: None,
        },
    }
}

/// `Natures`, in the order they are registered.
pub const NATURES: [Nature; 25] = {
    use Stat::{Attack, Defence, SpecialAttack, SpecialDefence, Speed};
    [
        of("hardy", None),
        of("lonely", Some((Attack, Defence))),
        of("brave", Some((Attack, Speed))),
        of("adamant", Some((Attack, SpecialAttack))),
        of("naughty", Some((Attack, SpecialDefence))),
        of("bold", Some((Defence, Attack))),
        of("docile", None),
        of("relaxed", Some((Defence, Speed))),
        of("impish", Some((Defence, SpecialAttack))),
        of("lax", Some((Defence, SpecialDefence))),
        of("timid", Some((Speed, Attack))),
        of("hasty", Some((Speed, Defence))),
        of("serious", None),
        of("jolly", Some((Speed, SpecialAttack))),
        of("naive", Some((Speed, SpecialDefence))),
        of("modest", Some((SpecialAttack, Attack))),
        of("mild", Some((SpecialAttack, Defence))),
        of("quiet", Some((SpecialAttack, Speed))),
        of("bashful", None),
        of("rash", Some((SpecialAttack, SpecialDefence))),
        of("calm", Some((SpecialDefence, Attack))),
        of("gentle", Some((SpecialDefence, Defence))),
        of("sassy", Some((SpecialDefence, Speed))),
        of("careful", Some((SpecialDefence, SpecialAttack))),
        of("quirky", None),
    ]
};

/// `Natures.getNature`: by identifier (`cobblemon:bold`), or by its path alone.
pub fn nature(id: &str) -> Option<&'static Nature> {
    let name = id.strip_prefix("cobblemon:").unwrap_or(id);
    NATURES.iter().find(|nature| nature.name == name)
}

/// `CobblemonStatProvider.getStatForPokemon`: what `stat` is for a Pokémon of `species` at `level`,
/// from its form's base stat, the IV that counts, its EV and the nature that counts.
pub fn stat(
    stat: Stat,
    species: &str,
    base: u32,
    iv: u32,
    ev: u32,
    level: u32,
    nature: Option<&Nature>,
) -> u32 {
    if stat == Stat::Hp {
        return crate::party::max_health(species, base, iv, ev, level);
    }
    let value = (2 * base + iv + ev / 4) * level / 100 + 5;
    nature.map_or(value, |nature| nature.modify(stat, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_world::nbt::parse_snbt;

    #[test]
    fn a_stat_is_what_its_base_iv_ev_level_and_nature_give() {
        // Charizard at level 50, bold (defence up, attack down), as the server gave it
        // (`party_pokemon_2.bin`): base 78/84/78/109/85/100, IVs 4/6/14/26/14/8, no EV.
        let bold = nature("cobblemon:bold").unwrap();
        let of = |s, base, iv| stat(s, "cobblemon:charizard", base, iv, 0, 50, Some(bold));
        assert_eq!(of(Stat::Hp, 78, 4), 140);
        // 92 before the nature: a tenth less, rounded down.
        assert_eq!(of(Stat::Attack, 84, 6), 82);
        // 90 before the nature: a tenth more.
        assert_eq!(of(Stat::Defence, 78, 14), 99);
        assert_eq!(of(Stat::SpecialAttack, 109, 26), 127);
        assert_eq!(of(Stat::SpecialDefence, 85, 14), 97);
        assert_eq!(of(Stat::Speed, 100, 8), 109);
        // EVs count a quarter, rounded down; a nature that changes nothing, or none known.
        assert_eq!(
            stat(Stat::Speed, "cobblemon:pikachu", 90, 31, 252, 100, None),
            279
        );
        assert_eq!(
            stat(
                Stat::Speed,
                "cobblemon:pikachu",
                90,
                31,
                255,
                100,
                nature("hardy")
            ),
            279
        );
        assert_eq!(
            stat(
                Stat::Speed,
                "cobblemon:pikachu",
                90,
                31,
                252,
                100,
                nature("timid")
            ),
            306
        );
        // Shedinja has one point of health, and its other stats as any Pokémon.
        assert_eq!(
            stat(Stat::Hp, "cobblemon:shedinja", 1, 31, 252, 100, None),
            1
        );
        assert_eq!(
            stat(Stat::Attack, "cobblemon:shedinja", 90, 0, 0, 100, None),
            185
        );
    }

    #[test]
    fn a_nature_rounds_down_what_it_changes() {
        let adamant = nature("adamant").unwrap();
        assert_eq!(
            (adamant.increased, adamant.decreased),
            (Some(Stat::Attack), Some(Stat::SpecialAttack))
        );
        assert_eq!(adamant.modify(Stat::Attack, 105), 115);
        assert_eq!(adamant.modify(Stat::SpecialAttack, 105), 94);
        assert_eq!(adamant.modify(Stat::Speed, 105), 105);
        // Whole tenths stay whole, whatever the floating point makes of them.
        for value in 0..=1000 {
            assert_eq!(adamant.modify(Stat::Attack, value), value * 11 / 10);
            assert_eq!(adamant.modify(Stat::SpecialAttack, value), value * 9 / 10);
        }
        // Twenty-five natures, five of which change nothing; none raises what it lowers.
        assert_eq!(NATURES.len(), 25);
        let neutral: Vec<_> = NATURES
            .iter()
            .filter(|n| n.increased.is_none())
            .map(|n| n.name)
            .collect();
        assert_eq!(neutral, ["hardy", "docile", "serious", "bashful", "quirky"]);
        assert!(
            NATURES
                .iter()
                .all(|n| n.increased != n.decreased || n.increased.is_none())
        );
        assert!(NATURES.iter().all(|n| n.increased != Some(Stat::Hp)));
        assert_eq!(nature("cobblemon:quirky"), nature("quirky"));
        assert!(nature("cobblemon:sleepy").is_none());
    }

    #[test]
    fn ivs_and_evs_are_read_from_their_maps() {
        let (tag, _) = parse_snbt(
            r#"{Base:{"cobblemon:hp":4,"cobblemon:attack":6,"cobblemon:defence":14,
                "cobblemon:special_attack":26,"cobblemon:special_defence":14,"cobblemon:speed":31},
                HyperTrained:{"cobblemon:hp":31,"cobblemon:speed":31}}"#,
        )
        .unwrap();
        let ivs = Ivs::from_tag(Some(&tag));
        assert_eq!(ivs.base, [4, 6, 14, 26, 14, 31]);
        // Speed was trained to what it was: not trained.
        assert_eq!(ivs.hyper_trained, [Some(31), None, None, None, None, None]);
        assert_eq!(ivs.effective(Stat::Hp), 31);
        assert_eq!(ivs.effective(Stat::Attack), 6);
        assert_eq!(Ivs::from_tag(None), Ivs::default());
        let (tag, _) =
            parse_snbt(r#"{"cobblemon:hp":252,"cobblemon:speed":6,"cobblemon:luck":9}"#).unwrap();
        assert_eq!(evs(Some(&tag)), [252, 0, 0, 0, 0, 6]);
        assert_eq!(evs(None), [0; 6]);
        assert_eq!(
            Stat::named("cobblemon:special_defence"),
            Some(Stat::SpecialDefence)
        );
        assert_eq!(Stat::named("speed"), Some(Stat::Speed));
        assert_eq!(Stat::named("cobblemon:evasion"), None);
    }
}
