// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a Cobblemon 1.8.1 client plays that its server does not send as a sound.
//!
//! The server plays a Ball's throw and a recall with vanilla's sound packets. The rest is the client's
//! own doing, and is worked out here so that the window only has to play it:
//!
//! - the sounds of a Pokémon coming out of its Ball (`PokemonClientDelegate`): the Ball's trail while
//!   it flies, and what it opens with ([`thrown_sounds`], [`appearing_sound`]);
//! - a cry, which is no sound packet but an animation the server names
//!   ([`PlayAnimation`], `cobblemon:play_posable_animation`): the sound is a keyframe of that
//!   animation in the species' animation file ([`NamedSounds`]);
//! - a battle's music ([`BattleMusic`], `cobblemon:battle_music`), which Cobblemon starts, swaps and
//!   fades out as `BattleMusicHandler` and `BattleMusicInstance` say ([`music_step`], [`faded`]).
use crate::{
    animation::{AnimationFile, SoundKeyframe},
    poser::{Argument, Call, Expression, Poser},
    wire::{nullable, string},
};
use horizon_protocol::{Result, buf::Reader};
use std::{collections::HashMap, sync::Arc};

/// `PlayPosableAnimationPacket.ID`.
pub const PLAY_ANIMATION: &str = "cobblemon:play_posable_animation";

/// `CobblemonSounds.POKE_BALL_TRAIL`: a Ball in flight.
pub const BALL_TRAIL: &str = "cobblemon:poke_ball.trail";
/// `CobblemonSounds.POKE_BALL_SEND_OUT`: a Ball opening.
pub const SEND_OUT: &str = "cobblemon:poke_ball.send_out";
/// `CobblemonSounds.POKE_BALL_SHINY_SEND_OUT`: a Ball opening on a shiny Pokémon.
pub const SHINY_SEND_OUT: &str = "cobblemon:poke_ball.shiny_send_out";
/// `CobblemonSounds.GUI_CLICK`: a choice made in one of Cobblemon's screens.
pub const GUI_CLICK: &str = "cobblemon:gui.click";

/// The trail's volume (`MovingSoundInstance(.., 0.1f, 1f, false, 20, 0)`).
pub const TRAIL_VOLUME: f32 = 0.1;
/// The trail's `duration`, in ticks: it stops itself on the tick its count is past it, the 22nd.
pub const TRAIL_TICKS: u32 = 20;
/// What a Ball opens at (`playSound(.., SoundSource.PLAYERS, 0.6f, 1f)`).
pub const SEND_OUT_VOLUME: f32 = 0.6;
/// An alpha Pokémon's cry (`AlphaCrySoundInstance(.., 1.5f, 0.92f, ..)`): louder and lower.
pub const ALPHA_CRY: (f32, f32) = (1.5, 0.92);
/// `BattleMusicInstance.fadeTime`: the ticks a battle's music takes to fade out.
pub const MUSIC_FADE_TICKS: u32 = 60;

/// What plays the moment the Ball of a thrown Pokémon opens (`BEAM_MODE` 1, ten ticks in): the
/// shiny's sound or the plain one, then the plain one once more for a Pokémon that has an owner. The
/// two blocks of `PokemonClientDelegate` are not exclusive: an owned Pokémon is heard twice.
pub fn thrown_sounds(shiny: bool, owned: bool) -> impl Iterator<Item = &'static str> {
    let first = if shiny { SHINY_SEND_OUT } else { SEND_OUT };
    std::iter::once(first).chain(owned.then_some(SEND_OUT))
}

/// What plays as a Pokémon appears where it stands (`BEAM_MODE` 2: put down from a pasture, out of
/// a Ball that failed): one sound, and only for a Pokémon that has an owner.
pub fn appearing_sound(shiny: bool, owned: bool) -> Option<&'static str> {
    owned.then_some(if shiny { SHINY_SEND_OUT } else { SEND_OUT })
}

/// `PlayPosableAnimationPacket`: an entity plays the first of `animations` its model has.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayAnimation {
    pub entity: i32,
    /// Names of the model's animations (`cry`, `recoil`, `faint`...), in the order they were written.
    pub animations: Vec<String>,
    /// MoLang run on the entity's state before the animation starts. Read and not run here.
    pub expressions: Vec<String>,
}

impl PlayAnimation {
    pub fn read(data: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(data);
        let entity = r.i32()?;
        let strings = |r: &mut Reader| -> Result<Vec<String>> {
            (0..r.count(r.remaining())?).map(|_| string(r)).collect()
        };
        Ok(Self {
            entity,
            animations: strings(r)?,
            expressions: strings(r)?,
        })
    }
}

/// `BattleMusicPacket`: the sound to play for as long as the battle lasts.
#[derive(Clone, Debug, PartialEq)]
pub struct BattleMusic {
    /// A sound event's id. None ends the music that plays, fading it out.
    pub music: Option<String>,
    pub volume: f32,
    pub pitch: f32,
    /// `restartExisting`: whether music that is already this one starts over.
    pub restart: bool,
}

impl BattleMusic {
    pub fn read(data: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(data);
        Ok(Self {
            music: nullable(r, string)?,
            volume: r.f32()?,
            pitch: r.f32()?,
            restart: r.bool()?,
        })
    }
}

/// What `BattleMusicHandler` does of a [`BattleMusic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicStep {
    /// `BattleMusicController.endMusic`: what plays fades out.
    End,
    /// `initializeMusic`: nothing played, the music starts, and what the game played of music,
    /// ambience and records waits for it to end.
    Start,
    /// The music asked for plays already and is not to start over.
    Keep,
    /// `switchMusic`: what plays stops at once and the music asked for starts.
    Switch,
}

/// What a [`BattleMusic`] asks for, `playing` being the battle music the sound engine still plays
/// (`SoundManager.isActive`), fading or not.
pub fn music_step(asked: &BattleMusic, playing: Option<&str>) -> MusicStep {
    match (asked.music.as_deref(), playing) {
        (None, _) => MusicStep::End,
        (Some(_), None) => MusicStep::Start,
        (Some(asked_for), Some(playing)) if asked_for == playing && !asked.restart => {
            MusicStep::Keep
        }
        (Some(_), Some(_)) => MusicStep::Switch,
    }
}

/// A battle music's volume `ticks` into its fade (`BattleMusicInstance.tick`: a straight line from
/// the volume it started at to none), None once it is silent and done.
pub fn faded(initial: f32, ticks: u32) -> Option<f32> {
    let volume =
        (f64::from(initial) * (1.0 - f64::from(ticks) / f64::from(MUSIC_FADE_TICKS))) as f32;
    (volume > 0.0).then_some(volume)
}

/// The sounds of a model's named animations: what `PosableModel.getAnimation` finds for a name the
/// server sends, with the sounds that animation plays. Worked out once, as the model's files are
/// read: the animation files are not the render thread's to open when a cry comes.
///
/// Only an animation the poser names is here. A name that is itself MoLang or a `group:animation`,
/// which Cobblemon also takes, finds nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NamedSounds {
    /// By pose, in the poser's order: its `namedAnimations` that resolve, each with its sounds. One
    /// without any sound is there too: it hides the poser's own of that name.
    poses: Vec<HashMap<String, Vec<SoundKeyframe>>>,
    /// The poser's own `animations`.
    poser: HashMap<String, Vec<SoundKeyframe>>,
}

/// The Bedrock animation a named animation's MoLang makes: the group and the name `q.bedrock`,
/// `q.bedrock_stateful` and `q.bedrock_primary` take first, whatever follows them.
fn bedrock(expression: &Expression) -> Option<(&str, &str)> {
    let Expression::Call(Call {
        function,
        arguments,
    }) = expression
    else {
        return None;
    };
    if !matches!(
        function.as_str(),
        "bedrock" | "bedrock_stateful" | "bedrock_primary"
    ) {
        return None;
    }
    match (arguments.first()?, arguments.get(1)?) {
        (Argument::String(group), Argument::String(name)) => Some((group, name)),
        _ => None,
    }
}

impl NamedSounds {
    /// `load` reads a Bedrock animation group (`pikachu` for `pikachu.animation.json`).
    pub fn new(poser: &Poser, load: &mut dyn FnMut(&str) -> Option<Arc<AnimationFile>>) -> Self {
        let mut resolved = |named: &[(String, Expression)]| -> HashMap<_, _> {
            named
                .iter()
                .filter_map(|(name, expression)| {
                    // One that does not resolve is as good as absent (`resolveFromAnimationMap`
                    // answers null for it, and the next map is asked).
                    let (group, animation) = bedrock(expression)?;
                    let file = load(group)?;
                    let animation = file.get(&format!("animation.{group}.{animation}"))?;
                    Some((name.clone(), animation.sounds.clone()))
                })
                .collect()
        };
        Self {
            poses: poser
                .poses
                .iter()
                .map(|pose| resolved(&pose.named_animations))
                .collect(),
            poser: resolved(&poser.named_animations),
        }
    }

    /// The sounds of what a model in the pose at `pose` (its place in the poser's `poses`) plays when
    /// told `names`: the first of them it has an animation for (`PosableState.addFirstAnimation`), the
    /// pose's own before the poser's. Empty when it has none, or one without a sound.
    pub fn sounds(&self, pose: Option<usize>, names: &[String]) -> &[SoundKeyframe] {
        let pose = pose.and_then(|pose| self.poses.get(pose));
        names
            .iter()
            .find_map(|name| {
                pose.and_then(|pose| pose.get(name))
                    .or_else(|| self.poser.get(name))
            })
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_protocol::buf::Writer;

    #[test]
    fn a_ball_opens_once_and_once_more_for_an_owned_pokemon() {
        let sounds = |shiny, owned| thrown_sounds(shiny, owned).collect::<Vec<_>>();
        assert_eq!(sounds(false, false), [SEND_OUT]);
        assert_eq!(sounds(false, true), [SEND_OUT, SEND_OUT]);
        assert_eq!(sounds(true, false), [SHINY_SEND_OUT]);
        assert_eq!(sounds(true, true), [SHINY_SEND_OUT, SEND_OUT]);
        // Appearing where it stands: once, and a wild one in silence.
        assert_eq!(appearing_sound(false, true), Some(SEND_OUT));
        assert_eq!(appearing_sound(true, true), Some(SHINY_SEND_OUT));
        assert_eq!(appearing_sound(true, false), None);
    }

    /// `PokemonEntity.cry`: `PlayPosableAnimationPacket(id, setOf("cry"), emptyList())`.
    #[test]
    fn an_animation_packet_names_the_entity_and_its_animations() {
        let mut w = Writer::new();
        w.i32(4711);
        w.varint(2);
        w.string("cry");
        w.string("recoil");
        w.varint(1);
        w.string("v.intensity = 2;");
        assert_eq!(
            PlayAnimation::read(&w.data).unwrap(),
            PlayAnimation {
                entity: 4711,
                animations: vec!["cry".into(), "recoil".into()],
                expressions: vec!["v.intensity = 2;".into()],
            }
        );
        // The cry as the server writes it: one name, no expression.
        let cry = [0, 0, 0, 12, 1, 3, b'c', b'r', b'y', 0];
        let read = PlayAnimation::read(&cry).unwrap();
        assert_eq!(
            (read.entity, read.animations.as_slice()),
            (12, &["cry".to_owned()][..])
        );
        assert!(read.expressions.is_empty());
        assert!(PlayAnimation::read(&cry[..6]).is_err());
    }

    fn music(music: Option<&str>, restart: bool) -> BattleMusic {
        BattleMusic {
            music: music.map(str::to_owned),
            volume: 1.0,
            pitch: 1.0,
            restart,
        }
    }

    #[test]
    fn battle_music_reads_and_the_handler_s_four_ways() {
        let mut w = Writer::new();
        w.bool(true);
        w.string("cobblemon:battle.pvw.default");
        w.f32(0.5);
        w.f32(1.25);
        w.bool(true);
        let read = BattleMusic::read(&w.data).unwrap();
        assert_eq!(
            read,
            BattleMusic {
                music: Some("cobblemon:battle.pvw.default".into()),
                volume: 0.5,
                pitch: 1.25,
                restart: true,
            }
        );
        // The end of a battle: no music, the defaults after it.
        let mut w = Writer::new();
        w.bool(false);
        w.f32(1.0);
        w.f32(1.0);
        w.bool(true);
        assert_eq!(BattleMusic::read(&w.data).unwrap(), music(None, true));

        let wild = "cobblemon:battle.pvw.default";
        assert_eq!(music_step(&music(Some(wild), true), None), MusicStep::Start);
        assert_eq!(
            music_step(&music(Some(wild), false), Some(wild)),
            MusicStep::Keep
        );
        assert_eq!(
            music_step(&music(Some(wild), true), Some(wild)),
            MusicStep::Switch
        );
        assert_eq!(
            music_step(
                &music(Some("cobblemon:battle.pvp.default"), false),
                Some(wild)
            ),
            MusicStep::Switch
        );
        assert_eq!(music_step(&music(None, true), Some(wild)), MusicStep::End);
        assert_eq!(music_step(&music(None, true), None), MusicStep::End);
    }

    #[test]
    fn battle_music_fades_in_a_straight_line_over_sixty_ticks() {
        assert_eq!(faded(1.0, 0), Some(1.0));
        assert_eq!(faded(1.0, 30), Some(0.5));
        assert_eq!(faded(0.5, 15), Some(0.375));
        assert!(faded(1.0, 59).is_some_and(|v| v > 0.0 && v < 0.02));
        assert_eq!(faded(1.0, 60), None);
        assert_eq!(faded(1.0, 61), None);
    }

    #[test]
    fn a_cry_s_sound_is_the_keyframe_of_the_animation_the_pose_names() {
        let poser = Poser::parse(
            r#"{
                "animations": {
                    "cry": "q.bedrock_stateful('testmon', 'cry')",
                    "recoil": "q.bedrock_stateful('testmon', 'recoil')",
                    "physical": "q.bedrock_primary('nobody', 'physical', q.curve('symmetrical_wide'))",
                    "odd": "q.is_ridden ? q.bedrock('testmon', 'cry') : q.bedrock('testmon', 'recoil')"
                },
                "poses": {
                    "standing": {"poseTypes": ["STAND"], "animations": []},
                    "battle": {
                        "poseTypes": ["WALK"],
                        "animations": [],
                        "namedAnimations": {
                            "cry": "q.bedrock_primary('testmon', 'battle_cry', q.curve('symmetrical_wide'))",
                            "recoil": "q.bedrock_stateful('testmon', 'missing')"
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let file = Arc::new(
            AnimationFile::parse(
                r#"{"animations": {
                    "animation.testmon.cry": {"sound_effects": {"0.5": {"effect": "pokemon.testmon.cry"}}},
                    "animation.testmon.battle_cry": {"sound_effects": {"1.25": {"effect": "pokemon.testmon.cry"}}},
                    "animation.testmon.recoil": {}
                }}"#,
            )
            .unwrap(),
        );
        let mut asked = Vec::new();
        let named = NamedSounds::new(&poser, &mut |group| {
            asked.push(group.to_owned());
            (group == "testmon").then(|| file.clone())
        });
        let heard = |pose, names: &[&str]| -> Vec<(f32, String)> {
            let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
            named
                .sounds(pose, &names)
                .iter()
                .map(|s| (s.seconds, s.sound.clone()))
                .collect()
        };
        let cry = |seconds| vec![(seconds, "cobblemon:pokemon.testmon.cry".to_owned())];
        // The poser's own cry where the pose names none, the pose's where it does.
        assert_eq!(heard(Some(0), &["cry"]), cry(0.5));
        assert_eq!(heard(Some(1), &["cry"]), cry(1.25));
        assert_eq!(heard(None, &["cry"]), cry(0.5));
        // The pose's recoil names an animation the group lacks: the poser's is played, in silence.
        assert!(heard(Some(1), &["recoil"]).is_empty());
        // The first name the model has an animation for; one it has none for is passed over.
        assert_eq!(heard(Some(0), &["physical", "odd", "cry"]), cry(0.5));
        assert_eq!(heard(Some(0), &["recoil", "cry"]), []);
        assert!(heard(Some(0), &["faint"]).is_empty());
        assert!(asked.contains(&"nobody".to_owned()));
    }
}
