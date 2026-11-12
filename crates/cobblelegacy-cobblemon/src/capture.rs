// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A thrown Poké Ball and the capture it attempts (`EmptyPokeBallEntity`).
//!
//! Like a Pokémon, the Ball never arrives as vanilla's add_entity: Cobblemon sends
//! `cobblemon:spawn_empty_pokeball_entity`, which Ball it is and its aspects, then the bytes of vanilla's
//! `ClientboundAddEntityPacket` (`SpawnPokeballPacket`). What follows is vanilla, by entity id: its moves,
//! its motion, its removal, and its data, where the capture is told: `CAPTURE_STATE` goes from the hit to
//! the Ball shut for good or burst open, and `SHAKE` flips each time the Ball shakes on the ground.
//!
//! The server sends nothing more: what the Ball looks like at each moment (open as it takes the Pokémon
//! in, bouncing as it lands, one of six wobbles per shake) is `PokeBallPosableState`'s doing on the
//! client, kept here as [`Display`].
use crate::wire::{AddEntity, string};
use horizon_protocol::{Error, Result, buf::Reader};

/// `SpawnPokeballPacket.ID`.
pub const SPAWN_POKEBALL: &str = "cobblemon:spawn_empty_pokeball_entity";

/// `SpawnPokeballPacket`: a thrown Ball entering the client's view.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnPokeBall {
    /// The Ball's id, such as `cobblemon:poke_ball`.
    pub ball: String,
    pub aspects: Vec<String>,
    pub entity: AddEntity,
}

impl SpawnPokeBall {
    /// The payload of a [`SPAWN_POKEBALL`] message, whole: leftover bytes are an error.
    pub fn read(payload: &[u8]) -> Result<Self> {
        let r = &mut Reader::new(payload);
        let ball = string(r)?;
        let aspects = (0..r.count(4096)?)
            .map(|_| string(r))
            .collect::<Result<_>>()?;
        let entity = AddEntity::read(r)?;
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "{SPAWN_POKEBALL}: {} bytes left over",
                r.remaining()
            )));
        }
        Ok(Self {
            ball,
            aspects,
            entity,
        })
    }
}

/// `EmptyPokeBallEntity.CaptureState`, by ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureState {
    /// In flight, nothing hit yet.
    Not,
    /// It hit a Pokémon: it bounces back, opens and takes the Pokémon in.
    Hit,
    /// The Pokémon is in: the Ball falls to the ground.
    Fall,
    /// A critical capture: one sharp rattle before the shakes.
    Critical,
    /// On the ground, shaking.
    Shake,
    Captured,
    BrokenFree,
}

impl CaptureState {
    pub const ALL: [Self; 7] = [
        Self::Not,
        Self::Hit,
        Self::Fall,
        Self::Critical,
        Self::Shake,
        Self::Captured,
        Self::BrokenFree,
    ];

    /// The state `CAPTURE_STATE` holds, a byte.
    pub fn of(ordinal: i8) -> Option<Self> {
        usize::try_from(ordinal)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }

    /// `EmptyPokeBallEntity.onSyncedDataUpdated`: whether the Ball falls once it is in this state.
    /// `None`: the state leaves that as it was.
    pub fn gravity(self) -> Option<bool> {
        match self {
            Self::Not | Self::Fall => Some(true),
            Self::Critical | Self::Shake => Some(false),
            Self::Hit | Self::Captured | Self::BrokenFree => None,
        }
    }
}

/// What an `EmptyPokeBallEntity` syncs that vanilla's readers stop at: its data from the first entry of
/// a serializer of Cobblemon's on (`horizon_protocol::adapter::UNREAD_ENTITY_DATA`). A field is `None`
/// when the packet does not carry it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BallData {
    /// `HIT_TARGET_POSITION`: where the Ball hit the Pokémon, which it then faces.
    pub hit_target: Option<[f64; 3]>,
    /// `HIT_VELOCITY`: the way it was flying, a unit vector.
    pub hit_velocity: Option<[f64; 3]>,
    /// `SHAKE`: flips at each shake, its value means nothing.
    pub shake: Option<bool>,
    /// `ASPECTS`.
    pub aspects: Option<Vec<String>>,
}

impl BallData {
    /// `Entity.DATA_NO_GRAVITY`'s index.
    pub const NO_GRAVITY: u8 = 5;
    /// `CAPTURE_STATE`'s index, a byte, which vanilla's readers know: after Entity's 8 entries and
    /// `ThrowableItemProjectile`'s item. The indexes follow the order of the companion's `defineId`s.
    pub const CAPTURE_STATE: u8 = 9;
    /// `HIT_TARGET_POSITION`'s index, the first of a serializer of Cobblemon's.
    pub const FIRST: u8 = 10;
    /// `SHAKE`'s index, a boolean: read by vanilla's readers when it travels alone, as it does at each
    /// shake, and here when one of the Ball's vectors is before it in the packet.
    pub const SHAKE: u8 = 12;

    /// The entries from the first one vanilla's readers do not know, as the server wrote them: for
    /// each an index, the serializer's number, the value, then 0xFF. Read by the entity's layout, as
    /// [`crate::wire::PokemonData::read_rest`] is, and an entity laid out otherwise is an error.
    pub fn read_rest(rest: &[u8]) -> Result<Self> {
        // Vanilla 1.21.1's boolean, and how many serializers it has: a mod's are numbered after them.
        const BOOLEAN: i32 = 8;
        const VANILLA: i32 = 31;
        let r = &mut Reader::new(rest);
        let mut data = Self::default();
        loop {
            let index = r.u8()?;
            if index == 0xFF {
                break;
            }
            let wire = r.varint()?;
            let fits = match index {
                10 | 11 | 13 => wire >= VANILLA,
                12 => wire == BOOLEAN,
                _ => false,
            };
            if !fits {
                return Err(Error::new(format!(
                    "a Ball's data {index} has serializer {wire}: not the layout of Cobblemon 1.8.1"
                )));
            }
            // `Vec3DataSerializer`: three doubles.
            let vector = |r: &mut Reader| Ok::<_, Error>([r.f64()?, r.f64()?, r.f64()?]);
            match index {
                10 => data.hit_target = Some(vector(r)?),
                11 => data.hit_velocity = Some(vector(r)?),
                12 => data.shake = Some(r.bool()?),
                // `StringSetDataSerializer`: a count in one byte, then the strings.
                _ => {
                    data.aspects = Some((0..r.u8()?).map(|_| string(r)).collect::<Result<_>>()?);
                }
            }
        }
        if r.remaining() != 0 {
            return Err(Error::new(format!(
                "a Ball's data: {} bytes after its end",
                r.remaining()
            )));
        }
        Ok(data)
    }
}

/// One of a Ball's animations, as `poke_ball.animation.json` and `ancient_poke_ball.animation.json`
/// name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clip {
    /// Tumbling in flight.
    Throw,
    /// The Beast Ball's own flight.
    ThrowBeast,
    ShutIdle,
    OpenIdle,
    Open,
    Shut,
    /// Landing after the Pokémon went in.
    Bounce,
    /// One of the six wobbles of a shake, from 1.
    Bob(u8),
    Critical,
    Capture,
    Break,
}

impl Clip {
    /// Every clip, the order [`Self::index`] numbers them in.
    pub const ALL: [Self; 16] = [
        Self::Throw,
        Self::ThrowBeast,
        Self::ShutIdle,
        Self::OpenIdle,
        Self::Open,
        Self::Shut,
        Self::Bounce,
        Self::Bob(1),
        Self::Bob(2),
        Self::Bob(3),
        Self::Bob(4),
        Self::Bob(5),
        Self::Bob(6),
        Self::Critical,
        Self::Capture,
        Self::Break,
    ];

    /// The animation's name after its group: `animation.poke_ball.<name>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Throw => "throw",
            Self::ThrowBeast => "throw_beast",
            Self::ShutIdle => "shut_idle",
            Self::OpenIdle => "open_idle",
            Self::Open => "open",
            Self::Shut => "shut",
            Self::Bounce => "bounce",
            Self::Bob(1) => "bob1",
            Self::Bob(2) => "bob2",
            Self::Bob(3) => "bob3",
            Self::Bob(4) => "bob4",
            Self::Bob(5) => "bob5",
            Self::Bob(_) => "bob6",
            Self::Critical => "critical",
            Self::Capture => "capture",
            Self::Break => "break",
        }
    }

    /// The clip's place in [`Self::ALL`].
    pub fn index(self) -> usize {
        match self {
            Self::Throw => 0,
            Self::ThrowBeast => 1,
            Self::ShutIdle => 2,
            Self::OpenIdle => 3,
            Self::Open => 4,
            Self::Shut => 5,
            Self::Bounce => 6,
            Self::Bob(n) => 6 + usize::from(n.clamp(1, 6)),
            Self::Critical => 13,
            Self::Capture => 14,
            Self::Break => 15,
        }
    }
}

/// `PokeBallPosableState`'s HIT: the Ball opens this long after it hit...
pub const OPENS_AFTER: f32 = 0.2;
/// ... and shuts again this long after it opened.
pub const SHUTS_AFTER: f32 = 1.75;
/// How long the `open` and `shut` animations last, during which nothing else poses the Ball (a
/// `PrimaryAnimation` silences the pose it leaves).
pub const LID: f32 = 0.5;

/// What poses a Ball at one moment: each clip with the seconds it has run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    /// The pose's own animation, which loops: flying, shut, open. None while the lid moves.
    pub idle: Option<(Clip, f32)>,
    /// The animation on top of it: the lid moving, the bounce, a shake, the capture, the break. It is
    /// kept after its end, for whoever samples it to tell from the animation's length.
    pub over: Option<(Clip, f32)>,
}

/// `PokeBallPosableState`: which animations a Ball plays, from the capture states and the shakes the
/// server tells. Times are the Ball's own seconds (`PosableState.animationSeconds`: its age).
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    state: CaptureState,
    /// The state before this one.
    before: CaptureState,
    beast: bool,
    /// When the Ball hit, and when it left that state.
    hit: Option<(f32, Option<f32>)>,
    /// The animation over the pose's, and when it started.
    active: Option<(Clip, f32)>,
}

impl Display {
    /// A Ball just thrown. `beast`: the Beast Ball, which flies its own way.
    pub fn thrown(beast: bool) -> Self {
        Self {
            state: CaptureState::Not,
            before: CaptureState::Not,
            beast,
            hit: None,
            active: None,
        }
    }

    /// `ClientBallDisplay`: the Ball a battle's tile shows, which starts shut with the Pokémon in.
    pub fn in_battle() -> Self {
        Self {
            state: CaptureState::Fall,
            ..Self::thrown(false)
        }
    }

    pub fn state(&self) -> CaptureState {
        self.state
    }

    /// `stateEmitter`: the server said `state`, at `now`.
    pub fn set(&mut self, state: CaptureState, now: f32) {
        if state == self.state {
            return;
        }
        if let Some((_, left @ None)) = &mut self.hit {
            *left = Some(now);
        }
        match state {
            CaptureState::Hit => self.hit = Some((now, None)),
            CaptureState::Critical => self.active = Some((Clip::Critical, now)),
            // The critical rattle stands for the bounce.
            CaptureState::Shake if self.state != CaptureState::Critical => {
                self.active = Some((Clip::Bounce, now));
            }
            CaptureState::Captured => self.active = Some((Clip::Capture, now)),
            CaptureState::BrokenFree => self.active = Some((Clip::Break, now)),
            _ => {}
        }
        (self.before, self.state) = (self.state, state);
    }

    /// `shakeEmitter`: the Ball shakes, with the `bob`th of its six wobbles (Cobblemon draws one at
    /// random). Only a Ball on the ground shakes.
    pub fn shake(&mut self, now: f32, bob: u8) {
        if self.state == CaptureState::Shake {
            self.active = Some((Clip::Bob(bob.clamp(1, 6)), now));
        }
    }

    /// Whether nothing is left of the Ball at `now`: burst open, its `break` animation has shrunk it
    /// away (the server removes it a moment later).
    pub fn gone(&self, now: f32) -> bool {
        /// When `animation.poke_ball.break` reaches a scale of nothing, in both shapes of Ball.
        const SHRUNK: f32 = 1.125;
        self.state == CaptureState::BrokenFree
            && self
                .active
                .is_some_and(|(_, started)| now - started >= SHRUNK)
    }

    /// What poses the Ball at `now`.
    pub fn shown(&self, now: f32) -> Shown {
        let over = self.active.map(|(clip, started)| (clip, now - started));
        if self.state == CaptureState::Not {
            let clip = if self.beast {
                Clip::ThrowBeast
            } else {
                Clip::Throw
            };
            return Shown {
                idle: Some((clip, now)),
                over,
            };
        }
        // The lid: it opens if the Ball is still in HIT when its time comes, and shuts whatever the
        // state is by then.
        let opened = self
            .hit
            .map(|(hit, left)| (hit + OPENS_AFTER, left))
            .filter(|(opens, left)| left.is_none_or(|left| left >= *opens))
            .map(|(opens, _)| now - opens);
        match opened {
            Some(t) if (0.0..LID).contains(&t) => Shown {
                idle: None,
                over: Some((Clip::Open, t)),
            },
            Some(t) if (LID..SHUTS_AFTER).contains(&t) => Shown {
                idle: Some((Clip::OpenIdle, now)),
                over,
            },
            Some(t) if (SHUTS_AFTER..SHUTS_AFTER + LID).contains(&t) => Shown {
                idle: None,
                over: Some((Clip::Shut, t - SHUTS_AFTER)),
            },
            _ => Shown {
                idle: Some((Clip::ShutIdle, now)),
                over,
            },
        }
    }
}

/// `ClientBallDisplay.start` and `BattleOverlay.drawBattleTile`: what a battle's tile shows `seconds`
/// after the capture started, and at what part of its size. The Pokémon's portrait shrinks away after a
/// second, then the Ball grows in its place and stays until the capture ends.
pub fn tile(seconds: f32) -> (TileShows, f32) {
    /// The portrait stays this long, then each change of size takes [`RESIZE`].
    const WAIT: f32 = 1.0;
    const RESIZE: f32 = 0.3;
    if seconds < WAIT + RESIZE {
        (
            TileShows::Pokemon,
            1.0 - ((seconds - WAIT) / RESIZE).clamp(0.0, 1.0),
        )
    } else {
        (
            TileShows::Ball,
            ((seconds - WAIT - RESIZE) / RESIZE).clamp(0.0, 1.0),
        )
    }
}

/// When a battle's tile swaps the portrait for the Ball: [`tile`]'s second answer, and the moment
/// [`Display::set`] is told `Shake`.
pub const TILE_BALL_AT: f32 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileShows {
    Pokemon,
    Ball,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a Cobblemon 1.8.1 server (Fabric, Minecraft 1.21.1) sent a client whose player threw two
    /// Balls, as it came off the wire: a Poké Ball thrown at the sky, a Master Ball thrown at a Wooloo
    /// three blocks away, and what vanilla's readers left of the second one's data as it hit.
    const POKE_BALL: &[u8] = include_bytes!("../tests/fixtures/spawn_pokeball_1.bin");
    const MASTER_BALL: &[u8] = include_bytes!("../tests/fixtures/spawn_pokeball_2.bin");
    const HIT: &[u8] = include_bytes!("../tests/fixtures/pokeball_data_hit.bin");

    #[test]
    fn a_thrown_ball_decodes_to_its_last_byte() {
        let ball = SpawnPokeBall::read(POKE_BALL).unwrap();
        assert_eq!(ball.ball, "cobblemon:poke_ball");
        assert!(ball.aspects.is_empty());
        // The vanilla packet at the end: the server's own number for cobblemon:empty_pokeball, and in
        // its data the entity that threw it, as every projectile says.
        assert_eq!((ball.entity.id, ball.entity.kind), (1280, 131));
        assert_eq!(ball.entity.data, 1058);
        // A block in front of the thrower's eyes, going up and south at the Ball's throwing power.
        let [x, y, z] = ball.entity.position;
        assert!(
            (x + 389.489).abs() < 0.001
                && (y - 121.947).abs() < 0.001
                && (z + 59.596).abs() < 0.001
        );
        let [vx, vy, vz] = ball.entity.velocity;
        assert!(vy > 0.5 && vz > 1.1);
        let speed = (vx * vx + vy * vy + vz * vz).sqrt();
        assert!((speed - 1.25).abs() < 0.01, "{speed}");

        let master = SpawnPokeBall::read(MASTER_BALL).unwrap();
        assert_eq!(master.ball, "cobblemon:master_ball");
        assert_eq!((master.entity.id, master.entity.data), (1313, 1058));
        assert!(master.entity.velocity[1] < 0.0);
    }

    #[test]
    fn a_ball_spawn_laid_out_otherwise_is_an_error() {
        for cut in [0, 1, 19, 21, POKE_BALL.len() - 1] {
            assert!(
                SpawnPokeBall::read(&POKE_BALL[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut longer = POKE_BALL.to_vec();
        longer.push(0);
        let e = SpawnPokeBall::read(&longer).unwrap_err();
        assert!(e.0.contains("1 bytes left over"), "{}", e.0);
        // One aspect more than the server sent: the count in the byte after the Ball's id.
        let mut aspect = POKE_BALL[..20].to_vec();
        aspect.extend_from_slice(&[1, 5]);
        aspect.extend_from_slice(b"shiny");
        aspect.extend_from_slice(&POKE_BALL[21..]);
        assert_eq!(SpawnPokeBall::read(&aspect).unwrap().aspects, ["shiny"]);
    }

    #[test]
    fn a_ball_that_hit_says_where_and_how_it_flew() {
        let data = BallData::read_rest(HIT).unwrap();
        // The place is the Pokémon's own, its feet: `EntityHitResult`'s location.
        assert_eq!(data.hit_target, Some([-389.5, 120.0, -57.5]));
        let [x, y, z] = data.hit_velocity.unwrap();
        assert!(((x * x + y * y + z * z).sqrt() - 1.0).abs() < 1.0e-9);
        assert!(z > 0.9 && y < 0.0);
        assert_eq!((data.shake, data.aspects), (None, None));
        for cut in [0, 1, 20, HIT.len() - 1] {
            assert!(BallData::read_rest(&HIT[..cut]).is_err(), "cut at {cut}");
        }
        // The first of a mod's serializers on that server, which vanilla has 31 of.
        assert_eq!((HIT[0], HIT[1]), (BallData::FIRST, 31));
    }

    #[test]
    fn a_capture_state_is_its_ordinal_and_says_whether_the_ball_falls() {
        assert_eq!(CaptureState::of(0), Some(CaptureState::Not));
        assert_eq!(CaptureState::of(4), Some(CaptureState::Shake));
        assert_eq!(CaptureState::of(6), Some(CaptureState::BrokenFree));
        assert_eq!((CaptureState::of(7), CaptureState::of(-1)), (None, None));
        assert_eq!(CaptureState::Fall.gravity(), Some(true));
        assert_eq!(CaptureState::Shake.gravity(), Some(false));
        assert_eq!(CaptureState::Hit.gravity(), None);
    }

    #[test]
    fn a_ball_s_own_data_reads_by_its_layout() {
        // As Cobblemon's serializers write them: where it hit, the way it flew, a shake, an aspect.
        let mut rest = vec![10, 33];
        for v in [1.5f64, 64.0, -2.25] {
            rest.extend_from_slice(&v.to_be_bytes());
        }
        rest.extend_from_slice(&[11, 33]);
        for v in [0.0f64, -1.0, 0.0] {
            rest.extend_from_slice(&v.to_be_bytes());
        }
        rest.extend_from_slice(&[12, 8, 1, 13, 32, 1, 5]);
        rest.extend_from_slice(b"shiny");
        rest.push(0xFF);
        assert_eq!(
            BallData::read_rest(&rest).unwrap(),
            BallData {
                hit_target: Some([1.5, 64.0, -2.25]),
                hit_velocity: Some([0.0, -1.0, 0.0]),
                shake: Some(true),
                aspects: Some(vec!["shiny".to_owned()]),
            }
        );
        assert_eq!(BallData::read_rest(&[0xFF]).unwrap(), BallData::default());
        // Cut short, longer than it says, or laid out otherwise.
        for cut in [0, 1, 5, rest.len() - 1] {
            assert!(BallData::read_rest(&rest[..cut]).is_err(), "cut at {cut}");
        }
        let e = BallData::read_rest(&[0xFF, 0]).unwrap_err();
        assert!(e.0.contains("1 bytes after its end"), "{}", e.0);
        let e = BallData::read_rest(&[12, 33, 1, 0xFF]).unwrap_err();
        assert!(e.0.contains("data 12 has serializer 33"), "{}", e.0);
        let e = BallData::read_rest(&[10, 8, 1, 0xFF]).unwrap_err();
        assert!(e.0.contains("data 10 has serializer 8"), "{}", e.0);
        assert!(BallData::read_rest(&[14, 8, 1, 0xFF]).is_err());
    }

    #[test]
    fn every_clip_has_its_place_and_its_name() {
        for (i, clip) in Clip::ALL.into_iter().enumerate() {
            assert_eq!(clip.index(), i, "{clip:?}");
        }
        assert_eq!(Clip::Bob(3).name(), "bob3");
        assert_eq!(Clip::ThrowBeast.name(), "throw_beast");
    }

    #[test]
    fn a_ball_tumbles_opens_on_the_pokemon_and_shuts_before_it_falls() {
        let mut ball = Display::thrown(false);
        assert_eq!(
            ball.shown(0.3),
            Shown {
                idle: Some((Clip::Throw, 0.3)),
                over: None
            }
        );
        // The hit, half a second into the flight: shut for a moment, then the lid opens by itself.
        ball.set(CaptureState::Hit, 0.5);
        assert_eq!(ball.shown(0.6).idle, Some((Clip::ShutIdle, 0.6)));
        let opening = ball.shown(0.9);
        assert_eq!(opening.idle, None);
        let (clip, t) = opening.over.unwrap();
        assert!(clip == Clip::Open && (t - 0.2).abs() < 1.0e-5);
        assert_eq!(ball.shown(1.5).idle, Some((Clip::OpenIdle, 1.5)));
        // It shuts 1.75 s after it opened, falling or not.
        ball.set(CaptureState::Fall, 2.7);
        let shutting = ball.shown(2.7);
        let (clip, t) = shutting.over.unwrap();
        assert!(
            clip == Clip::Shut && (t - 0.25).abs() < 1.0e-5,
            "{clip:?} {t}"
        );
        assert_eq!(shutting.idle, None);
        assert_eq!(
            ball.shown(3.0),
            Shown {
                idle: Some((Clip::ShutIdle, 3.0)),
                over: None
            }
        );
        // The Beast Ball flies its own way.
        assert_eq!(
            Display::thrown(true).shown(0.1).idle,
            Some((Clip::ThrowBeast, 0.1))
        );
    }

    #[test]
    fn a_ball_that_leaves_the_hit_too_soon_never_opens() {
        let mut ball = Display::thrown(false);
        ball.set(CaptureState::Hit, 1.0);
        ball.set(CaptureState::BrokenFree, 1.1);
        let shown = ball.shown(1.5);
        assert_eq!(shown.idle, Some((Clip::ShutIdle, 1.5)));
        let (clip, t) = shown.over.unwrap();
        assert!(clip == Clip::Break && (t - 0.4).abs() < 1.0e-5);
    }

    #[test]
    fn on_the_ground_it_bounces_shakes_then_shuts_for_good_or_breaks() {
        let mut ball = Display::thrown(false);
        ball.set(CaptureState::Hit, 0.0);
        ball.set(CaptureState::Fall, 2.2);
        // A shake before it lands is none.
        ball.shake(2.5, 2);
        assert_eq!(ball.shown(2.6).over, None);
        ball.set(CaptureState::Shake, 3.0);
        assert_eq!(ball.shown(3.5).over, Some((Clip::Bounce, 0.5)));
        ball.shake(4.0, 4);
        assert_eq!(ball.shown(4.25).over, Some((Clip::Bob(4), 0.25)));
        // Each shake starts over, and a wobble outside the six is the nearest.
        ball.shake(5.25, 9);
        assert_eq!(ball.shown(5.25).over, Some((Clip::Bob(6), 0.0)));
        ball.set(CaptureState::Captured, 6.5);
        assert_eq!(ball.shown(7.0).over, Some((Clip::Capture, 0.5)));
        assert_eq!(ball.state(), CaptureState::Captured);
        // Saying a state again changes nothing.
        ball.set(CaptureState::Captured, 7.0);
        assert_eq!(ball.shown(7.0).over, Some((Clip::Capture, 0.5)));

        // A critical capture rattles once, and that stands for the bounce.
        let mut critical = Display::thrown(false);
        critical.set(CaptureState::Hit, 0.0);
        critical.set(CaptureState::Fall, 2.2);
        critical.set(CaptureState::Critical, 3.0);
        assert_eq!(critical.shown(3.5).over, Some((Clip::Critical, 0.5)));
        critical.set(CaptureState::Shake, 4.0);
        assert_eq!(critical.shown(4.5).over, Some((Clip::Critical, 1.5)));
        critical.set(CaptureState::BrokenFree, 5.0);
        assert_eq!(critical.shown(5.5).over, Some((Clip::Break, 0.5)));
        // Burst open, it shrinks away; shut for good, it stays.
        assert!(!critical.gone(6.0) && critical.gone(6.2));
        assert!(!ball.gone(60.0));
    }

    #[test]
    fn a_battle_s_tile_swaps_the_portrait_for_the_ball() {
        assert_eq!(tile(0.5), (TileShows::Pokemon, 1.0));
        let (shows, size) = tile(1.15);
        assert!(shows == TileShows::Pokemon && (size - 0.5).abs() < 1.0e-5);
        assert_eq!(tile(TILE_BALL_AT), (TileShows::Ball, 0.0));
        let (shows, size) = tile(1.45);
        assert!(shows == TileShows::Ball && (size - 0.5).abs() < 1.0e-5);
        assert_eq!(tile(9.0), (TileShows::Ball, 1.0));
        // The tile's Ball is shut from the start, and bounces as it shows.
        let mut ball = Display::in_battle();
        assert_eq!(ball.shown(0.5).idle, Some((Clip::ShutIdle, 0.5)));
        ball.set(CaptureState::Shake, TILE_BALL_AT);
        assert_eq!(ball.shown(1.5).over.map(|o| o.0), Some(Clip::Bounce));
        ball.shake(3.0, 1);
        assert_eq!(ball.shown(3.0).over, Some((Clip::Bob(1), 0.0)));
    }
}
