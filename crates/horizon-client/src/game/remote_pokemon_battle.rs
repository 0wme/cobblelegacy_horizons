//! A battle on a Cobblemon server (CobbleLegacy's fork): Cobblemon's `BattleOverlay` and `BattleGUI`.
//!
//! The server runs the battle and tells it a message at a time (`cobblelegacy_cobblemon::battle`); the
//! window shows it and sends the player's choices. Two things draw it:
//!
//! - the overlay, part of the HUD in the party overlay's place: a tile for each Pokémon in the fight, the
//!   player's on the left and the opponent's on the right (portrait, name, sex, level, health), the
//!   battle's log at the bottom right, and a line that asks for a choice while the screen is away;
//! - the screen, over the world with the pointer free: the actions (fight, switch, catch, run), then the
//!   moves or the team. In a single battle the choice leaves on the click that makes it.
//!
//! A battle starts when the throwing key is let go with a wild Pokémon in the way
//! (`BattleChallengePacket`): the server starts it only if its own trace of the player's view agrees.
//! The same key puts the screen away and brings it back, so the player can walk (and flee: a wild
//! battle ends once the player is far enough, the Run button only says so).
//!
//! A move's tile takes its type's colour and icons from the server's own moves
//! (`cobblemon:moves_sync`): the type the move has there, not the one an ability or Hidden Power would
//! give it in this Pokémon's hands, which Cobblemon works out.
//!
//! Not here yet: battles of more than one Pokémon a side (their tiles and target choice), the gimmicks
//! (mega evolution, Z-moves, dynamax, terastallization), challenges between players, the tiles sliding
//! in and out (but for a caught Pokémon's, which slides away), scrolling the log, and Cobblemon's own
//! font for names.
//!
//! A capture in battle is thrown in the world like any other (`remote_pokemon_capture.rs`); the tile of
//! the Pokémon it hit shows it too, its portrait shrinking away and the Ball growing in its place.
use super::super::super::{Game, Screen};
use super::capture::{TILE_LEAVES, TileBall};
use super::party::{Framing, health_colour, small_text};
use cobblelegacy_cobblemon::{
    battle::{self, ActorType, Battle, BattlePokemon, Battles, Change, Response},
    capture::TileShows,
    moves, party,
};
use horizon_client::{
    hud::{HudAtlas, HudBatch, Rect, rgba},
    text::Text,
    ui, widgets,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// The battle's own textures, by the names the atlas gives a content mod's GUI textures.
const TEXTURES: &str = "cobblemon:gui/battle/";

/// How long a health bar takes to reach what the server said (`HealthChangeAnimation`).
const HEALTH_CHANGE: Duration = Duration::from_secs(1);

/// How far a wild Pokémon may be challenged from (`battleWildMaxDistance`).
const CHALLENGE_REACH: f64 = 12.0;

/// What the screen shows under the general actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Menu {
    #[default]
    Root,
    Moves,
    Team,
    Forfeit,
}

/// What the screen shows now (`BattleGUI.deriveRootActionSelection` and the selections under it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    /// Nothing to choose: the server works, or waits for the other side.
    Waiting,
    Root,
    Moves,
    /// The team; `forced` when a Pokémon has to come in, and there is no way back.
    Team {
        forced: bool,
    },
    Forfeit,
}

/// What a click on one of the screen's buttons does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Act {
    Fight,
    Team,
    Catch,
    Run,
    Forfeit,
    /// The move of this rank in the request.
    Move(usize),
    /// Send this Pokémon of the team in.
    Switch(u128),
    Back,
    /// The forfeit's confirmation: yes or no.
    Confirm(bool),
}

impl Act {
    /// The name a test reads it by.
    fn name(&self) -> String {
        match self {
            Self::Fight => "fight".into(),
            Self::Team => "team".into(),
            Self::Catch => "catch".into(),
            Self::Run => "run".into(),
            Self::Forfeit => "forfeit".into(),
            Self::Move(index) => format!("move-{index}"),
            Self::Switch(_) => "switch".into(),
            Self::Back => "back".into(),
            Self::Confirm(yes) => format!("confirm-{yes}"),
        }
    }
}

/// One thing of the screen that takes a click: where it is, what it does, and whether it does.
#[derive(Clone, Debug, PartialEq)]
struct Button {
    /// Left, top, width, height, in GUI pixels.
    rect: [f32; 4],
    act: Act,
    enabled: bool,
}

/// A health bar on its way from what it showed to what the server last said.
#[derive(Clone, Copy, Debug)]
struct Shown {
    from: f32,
    to: f32,
    since: Instant,
}

impl Shown {
    fn at(&self, now: Instant) -> f32 {
        let done =
            now.saturating_duration_since(self.since).as_secs_f32() / HEALTH_CHANGE.as_secs_f32();
        self.from + (self.to - self.from) * done.clamp(0.0, 1.0)
    }
}

/// What the window knows of the battle the player is in.
#[derive(Default)]
pub(super) struct Fight {
    battles: Battles,
    /// The player put the screen away to move around (`ClientBattle.minimised`).
    minimised: bool,
    menu: Menu,
    /// What each Pokémon's health bar shows, by the Pokémon's id.
    shown: HashMap<u128, Shown>,
    /// The log's lines, wrapped to its width, and how many of the battle's messages are in them.
    lines: Vec<Text>,
    wrapped: usize,
    /// The Ball thrown at one of the battle's Pokémon, which its tile shows.
    ball: Option<TileBall>,
    /// The Pokémon that was caught, and when: its tile slides off the screen
    /// (`MoveTileOffscreenAnimation`).
    caught: Option<(u128, Instant)>,
}

impl Fight {
    /// Whether the player is in a battle.
    pub(super) fn fighting(&self) -> bool {
        self.battles.current().is_some()
    }

    fn reset(&mut self) {
        self.minimised = false;
        self.menu = Menu::Root;
        self.shown.clear();
        self.lines.clear();
        self.wrapped = 0;
        self.ball = None;
        self.caught = None;
    }

    /// The share of its health a Pokémon's bar shows now, moving to its real one.
    fn health(&mut self, pokemon: &BattlePokemon, now: Instant) -> f32 {
        let real = pokemon.health_ratio();
        let shown = self.shown.entry(pokemon.uuid).or_insert(Shown {
            from: real,
            to: real,
            since: now,
        });
        if (shown.to - real).abs() > 1e-4 {
            *shown = Shown {
                from: shown.at(now),
                to: real,
                since: now,
            };
        }
        shown.at(now)
    }
}

/// The general actions a battle offers (`BattleGeneralActionSelection`): a wild Pokémon faced alone can
/// be caught or fled, any other battle forfeited; a trapped Pokémon cannot be switched.
fn options(battle: &Battle) -> Vec<Act> {
    let trapped = battle
        .request
        .as_ref()
        .and_then(|r| r.active.first())
        .is_some_and(|moves| moves.trapped);
    let opponents = battle.opponents();
    let wild = opponents.len() == 1
        && opponents[0].kind == ActorType::Wild
        && opponents[0].active.len() == 1;
    let mut options = vec![Act::Fight];
    if !trapped {
        options.push(Act::Team);
    }
    if wild {
        options.extend([Act::Catch, Act::Run]);
    } else {
        options.push(Act::Forfeit);
    }
    options
}

/// What the screen shows for `battle`, with `menu` what the player last opened.
fn view(battle: &Battle, menu: Menu) -> View {
    let Some(request) = battle.request.as_ref().filter(|_| battle.must_choose) else {
        return View::Waiting;
    };
    if request.forced_switch(0) {
        View::Team { forced: true }
    } else if request.active.is_empty() {
        View::Waiting
    } else {
        match menu {
            Menu::Root => View::Root,
            Menu::Moves => View::Moves,
            Menu::Team => View::Team { forced: false },
            Menu::Forfeit => View::Forfeit,
        }
    }
}

/// The tile of the general action of rank `rank`: two columns of two, at the bottom left.
fn option_tile(height: f32, rank: usize) -> [f32; 4] {
    let x = if rank % 2 == 0 { 12.0 } else { 105.0 };
    let y = if rank > 1 {
        height - 56.0
    } else {
        height - 85.0
    };
    [x, y, 90.0, 26.0]
}

/// The tile of the move of rank `index`.
fn move_tile(height: f32, index: usize) -> [f32; 4] {
    let x = if index % 2 == 0 { 20.0 } else { 125.0 };
    let y = if index > 1 {
        height - 55.0
    } else {
        height - 84.0
    };
    [x, y, 92.0, 24.0]
}

/// The top of the team's panel: centred on a screen high enough, else above the actions.
fn team_top(height: f32) -> f32 {
    if height > 304.0 {
        (height / 2.0).floor() - 74.0
    } else {
        height - 226.0
    }
}

/// The tile of the team's Pokémon of rank `index`: two columns in the middle of the screen.
fn team_tile(width: f32, height: f32, index: usize) -> [f32; 4] {
    let x = (width / 2.0).floor() - 95.0 + (index % 2) as f32 * 98.0;
    let y = team_top(height) + 34.0 + (index / 2) as f32 * 31.0;
    [x, y, 94.0, 29.0]
}

/// The back button, at the bottom left.
fn back_button(height: f32) -> [f32; 4] {
    [9.0, height - 22.0, 29.0, 17.0]
}

/// The log's frame, at the bottom right.
fn log_frame(width: f32, height: f32) -> [f32; 4] {
    [width - 181.0, height - 85.0, 169.0, 55.0]
}

/// The screen's buttons for what it shows now.
fn buttons(battle: &Battle, menu: Menu, (width, height): (f32, f32)) -> Vec<Button> {
    let mut buttons = Vec::new();
    let back = |buttons: &mut Vec<Button>| {
        buttons.push(Button {
            rect: back_button(height),
            act: Act::Back,
            enabled: true,
        });
    };
    match view(battle, menu) {
        View::Waiting => {}
        View::Root => {
            for (rank, act) in options(battle).into_iter().enumerate() {
                buttons.push(Button {
                    rect: option_tile(height, rank),
                    act,
                    enabled: true,
                });
            }
        }
        View::Moves => {
            let moves = battle.request.iter().flat_map(|r| r.active.first());
            for (index, offered) in moves.flat_map(|m| m.moves.iter()).enumerate().take(4) {
                buttons.push(Button {
                    rect: move_tile(height, index),
                    act: Act::Move(index),
                    enabled: offered.usable(),
                });
            }
            back(&mut buttons);
        }
        View::Team { forced } => {
            for (index, pokemon) in battle.team.iter().enumerate().take(6) {
                buttons.push(Button {
                    rect: team_tile(width, height, index),
                    act: Act::Switch(pokemon.uuid),
                    enabled: battle.can_switch_to(pokemon),
                });
            }
            if !forced {
                back(&mut buttons);
            }
        }
        View::Forfeit => {
            let (x, y) = ((width / 2.0).floor(), (height / 2.0).floor());
            for (yes, left) in [(true, x - 104.0), (false, x + 4.0)] {
                buttons.push(Button {
                    rect: [left, y, 100.0, 20.0],
                    act: Act::Confirm(yes),
                    enabled: true,
                });
            }
        }
    }
    buttons
}

/// Part of a sprite: from (`u0`, `v0`) to (`u1`, `v1`) of it, each a share of its width or height.
fn part(mut sprite: Rect, [u0, v0, u1, v1]: [f32; 4]) -> Rect {
    let (left, top) = (sprite.u0, sprite.v0);
    let (wide, high) = (sprite.u1 - sprite.u0, sprite.v1 - sprite.v0);
    sprite.u0 = left + wide * u0;
    sprite.u1 = left + wide * u1;
    sprite.v0 = top + high * v0;
    sprite.v1 = top + high * v1;
    sprite
}

/// One of the battle's textures, or the share `of` of it, stretched to `rect` as `blitk` does.
fn image(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    name: &str,
    [x, y, w, h]: [f32; 4],
    of: [f32; 4],
    colour: u32,
) {
    let name = if name.contains(':') {
        name.to_owned()
    } else {
        format!("{TEXTURES}{name}")
    };
    if let Some(sprite) = atlas.sprites.get(&name).copied() {
        batch.rect_uv(x, y, w, h, part(sprite, of), colour);
    }
}

const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
/// A texture that holds two states one over the other: at rest, and under the pointer.
const UPPER: [f32; 4] = [0.0, 0.0, 1.0, 0.5];
const LOWER: [f32; 4] = [0.0, 0.5, 1.0, 1.0];

/// `text` from (`x`, `y`), narrowed to `most` pixels when it is wider: Cobblemon writes its tiles in
/// a font narrower than the game's, which is the one used here.
fn fitted(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    text: &Text,
    (x, y): (f32, f32),
    most: f32,
    colour: u32,
) {
    let start = batch.vertices.len();
    let width = atlas.font.width_text(text);
    batch.text_styled(atlas, text, x, y, colour, true);
    if width > most {
        batch.scale_since(start, x, y, most / width, 1.0);
    }
}

/// White at opacity `alpha`, 0 to 1.
fn white(alpha: f32) -> u32 {
    rgba(255, 255, 255, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// A Pokémon's sex as its tile writes it, from its properties (`gender=MALE`).
fn gender_sign(properties: &str) -> Option<(&'static str, u32)> {
    let gender = properties
        .split(' ')
        .find_map(|pair| pair.strip_prefix("gender="))?;
    match gender {
        "MALE" => Some(("\u{2642}", rgba(0x32, 0xCB, 0xFF, 255))),
        "FEMALE" => Some(("\u{2640}", rgba(0xFC, 0x54, 0x54, 255))),
        _ => None,
    }
}

/// A move's name: Cobblemon's translation of its id, or what Showdown calls it.
fn move_name(offered: &battle::Move) -> Text {
    let key = format!("cobblemon.move.{}", offered.id);
    let translated = Text::tr(&key, &[]);
    if translated.plain() == key {
        Text::literal(offered.name.clone())
    } else {
        translated
    }
}

/// The health Showdown gives a Pokémon of the team in a request: points left and at most
/// (`33/33 par`), none for one that fainted (`0 fnt`).
fn condition(text: &str) -> Option<(u32, u32)> {
    let (health, most) = text.split(' ').next()?.split_once('/')?;
    Some((health.parse().ok()?, most.parse().ok()?))
}

impl Game<'_> {
    fn pokemon_battle(&self) -> Option<&Battle> {
        self.remote.as_ref()?.pokemon.fight.battles.current()
    }

    fn pokemon_fight(&mut self) -> Option<&mut Fight> {
        Some(&mut self.remote.as_mut()?.pokemon.fight)
    }

    /// A message of the server on one of the battle's channels.
    pub(super) fn remote_pokemon_battle(&mut self, channel: &str, data: &[u8]) {
        super::capture::record(channel, data);
        let me = self.own_uuid();
        let Some(fight) = self.pokemon_fight() else {
            return;
        };
        match fight.battles.read(channel, data, me) {
            // `BattleInitializeHandler`: the screen opens with the battle.
            Ok(Change::Started) => {
                fight.reset();
                if self.screen == Screen::Playing && !self.ui.hide_gui {
                    self.open_pokemon_battle_screen();
                }
            }
            Ok(Change::Ended) => {
                fight.reset();
                if self.screen == Screen::PokemonBattle {
                    self.screen = Screen::Playing;
                    self.capture(true);
                }
            }
            Ok(Change::MustChoose) => {
                fight.menu = Menu::Root;
                // A capture or an item the server started: it waits for this answer and no other
                // (`BattleApplyPassResponseHandler`).
                let pass = fight.battles.current().is_some_and(|b| b.pass_expected);
                if pass {
                    self.pokemon_battle_answer(Response::ForcePass);
                }
            }
            // `BattleCaptureStartHandler`: the tile of the Pokémon the Ball hit shows it.
            Ok(Change::CaptureStarted) => {
                let thrown = fight.battles.current().and_then(|battle| {
                    let capture = battle.capture.as_ref()?;
                    Some((battle.at(&capture.target)?.uuid, capture.ball.clone()))
                });
                fight.ball =
                    thrown.map(|(pokemon, ball)| TileBall::new(pokemon, &ball, Instant::now()));
                if let Some(name) = fight.ball.as_ref().map(|ball| ball.name().to_owned()) {
                    self.pokemon_ball(&name);
                }
            }
            Ok(Change::CaptureShook) => {
                if let Some(ball) = &mut fight.ball {
                    ball.shake(Instant::now());
                }
            }
            // `BattleCaptureEndHandler`: the Ball leaves the tile, and the tile the screen if the
            // Pokémon stayed in.
            Ok(Change::CaptureEnded(succeeded)) => {
                if let Some(ball) = fight.ball.take()
                    && succeeded
                {
                    fight.caught = Some((ball.pokemon(), Instant::now()));
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("Horizon: Cobblemon's {channel} does not decode: {e}"),
        }
    }

    /// Sends the player's choice for their active Pokémon; any other slot of the request lets
    /// Showdown choose, so that a battle of several a side goes on.
    fn pokemon_battle_answer(&mut self, response: Response) {
        let Some(battle) = self.pokemon_battle() else {
            return;
        };
        let slots = battle.request.as_ref().map_or(1, |r| r.slots()).max(1);
        let mut responses = vec![Response::Default; slots];
        if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
            log::info!("Horizon: answered the battle's request with {response:?}");
        }
        responses[0] = response;
        self.pokemon_send(
            battle::SELECT_ACTIONS,
            battle::select_actions(battle.id, &responses),
        );
        if let Some(fight) = self.pokemon_fight() {
            fight.battles.answered();
            fight.menu = Menu::Root;
        }
    }

    /// The throwing key let go with the entity `entity` in the way and the party's slot `slot`
    /// selected: a wild Pokémon near enough is challenged (`PartySendBinding.processEntityTarget`).
    pub(super) fn pokemon_battle_challenge(&mut self, entity: i32, slot: usize) {
        let Some(lead) = self.pokemon_party_member(slot).map(|p| p.uuid) else {
            return;
        };
        // `PokemonEntity.canBattle`: wild, not in a battle, not set apart from them.
        let wild = self.remote.as_ref().is_some_and(|r| {
            r.pokemon
                .seen
                .get(&entity)
                .is_some_and(|p| p.owner.is_none() && p.battle.is_none() && !p.unbattlable)
        });
        let near = self.aim().is_some_and(|(eye, _)| {
            self.mobs.iter().any(|m| {
                let at = [m.position.x, m.position.y, m.position.z];
                // From the player's feet, as Cobblemon measures.
                let feet = [eye[0], eye[1] - 1.62, eye[2]];
                m.id as i32 == entity
                    && (0..3).map(|i| (at[i] - feet[i]).powi(2)).sum::<f64>()
                        <= CHALLENGE_REACH * CHALLENGE_REACH
            })
        });
        if wild && near {
            if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
                log::info!("Horizon: challenged the wild Pokémon {entity} to a battle");
            }
            self.pokemon_send(battle::CHALLENGE, battle::challenge(entity, lead));
        }
    }

    /// Opens the battle's screen, or brings it back (`BattleGUI`).
    pub(in super::super::super) fn open_pokemon_battle_screen(&mut self) {
        let Some(fight) = self.pokemon_fight() else {
            return;
        };
        fight.minimised = false;
        fight.menu = Menu::Root;
        self.screen = Screen::PokemonBattle;
        self.keys.clear();
        self.left = false;
        self.right = false;
        self.mining = None;
        self.capture(false);
    }

    /// Puts the battle's screen away: the battle goes on, the player moves (`BattleGUI.onClose`).
    pub(in super::super::super) fn close_pokemon_battle_screen(&mut self) {
        if let Some(fight) = self.pokemon_fight() {
            fight.minimised = true;
        }
        self.screen = Screen::Playing;
        self.capture(true);
    }

    /// A left click on the battle's screen.
    pub(in super::super::super) fn pokemon_battle_click(&mut self) {
        let size = self.gui_size();
        let cursor = self.cursor;
        let Some(fight) = self.remote.as_ref().map(|r| &r.pokemon.fight) else {
            return;
        };
        let Some(battle) = fight.battles.current() else {
            return;
        };
        // The pointer as it is now, not as the last frame drew it: a script clicks between frames.
        let hit = buttons(battle, fight.menu, size)
            .into_iter()
            .find(|b| ui::hovered(cursor, b.rect[0], b.rect[1], b.rect[2], b.rect[3]));
        let Some(button) = hit.filter(|b| b.enabled) else {
            return;
        };
        self.audio.play_button_click();
        let menu = |game: &mut Self, menu| {
            if let Some(fight) = game.pokemon_fight() {
                fight.menu = menu;
            }
        };
        match button.act {
            Act::Fight => menu(self, Menu::Moves),
            Act::Team => menu(self, Menu::Team),
            Act::Forfeit => menu(self, Menu::Forfeit),
            Act::Back | Act::Confirm(false) => menu(self, Menu::Root),
            Act::Confirm(true) => self.pokemon_battle_answer(Response::Forfeit),
            // The ball is thrown in the world: the screen goes, and the server says when it lands.
            Act::Catch => {
                self.close_pokemon_battle_screen();
                self.system_message(Text::tr("cobblemon.battle.throw_pokeball_prompt", &[]));
            }
            // The server answers how to flee, and asks again.
            Act::Run => {
                self.close_pokemon_battle_screen();
                self.pokemon_battle_answer(Response::FleeAttempt);
            }
            Act::Move(index) => {
                let response = self
                    .pokemon_battle()
                    .and_then(|b| b.move_response(0, index));
                if let Some(response) = response {
                    self.pokemon_battle_answer(response);
                }
            }
            Act::Switch(pokemon) => self.pokemon_battle_answer(Response::Switch(pokemon)),
        }
    }

    /// The battle's tiles, log and prompt, on the HUD (`BattleOverlay.render`).
    pub(in super::super::super) fn draw_pokemon_battle_overlay(&mut self, batch: &mut HudBatch) {
        if !matches!(
            self.screen,
            Screen::Playing | Screen::Chat | Screen::PokemonBattle
        ) || self.debug
        {
            return;
        }
        let now = Instant::now();
        // What each tile shows, taken out of the battle so that the portraits can be made.
        let Some(fight) = self.pokemon_fight() else {
            return;
        };
        let Some(battle) = fight.battles.current().cloned() else {
            return;
        };
        let mut tiles: Vec<(bool, usize, BattlePokemon, f32)> = Vec::new();
        let sides = [
            (true, battle.own().map_or(&[][..], std::slice::from_ref)),
            (false, battle.opponents()),
        ];
        for (left, actors) in sides {
            let active = actors.iter().flat_map(|a| a.active.iter().flatten());
            for (rank, pokemon) in active.enumerate() {
                let health = fight.health(pokemon, now);
                // A fainted Pokémon's tile goes once its bar is empty.
                if !(pokemon.fainted && health <= pokemon.health_ratio() + 1e-4) {
                    tiles.push((left, rank, pokemon.clone(), health));
                }
            }
        }
        let alpha = if fight.minimised { 0.5 } else { 1.0 };
        let ball = fight.ball.as_mut().map(|ball| ball.frame(now));
        let caught = fight.caught;
        let Some(template) = self.res.mob_types().get(super::STAND_IN).cloned() else {
            return;
        };
        let looks: Vec<_> = tiles
            .iter()
            .map(|(_, _, p, _)| self.pokemon_portrait(p.uuid, &p.species, &p.aspects, &template))
            .collect();
        let (width, height) = self.gui_size();
        let atlas = &self.gui.atlas;
        for ((left, rank, pokemon, health), look) in tiles.iter().zip(&looks) {
            let (left, health) = (*left, *health);
            // A caught Pokémon's tile slides off its side of the screen, then is gone.
            let leaving = caught
                .filter(|(caught, _)| *caught == pokemon.uuid)
                .map(|(_, since)| now.saturating_duration_since(since).as_secs_f32() / TILE_LEAVES);
            if leaving.is_some_and(|done| done >= 1.0) {
                continue;
            }
            let slide = leaving.unwrap_or(0.0) * 160.0;
            let x = if left {
                12.0 - slide
            } else {
                width - 152.0 + slide
            };
            let y = 10.0 + *rank as f32 * 40.0;
            let portrait = if left { x + 5.0 } else { x + 107.0 };
            let info = if left { x + 40.0 } else { x + 7.0 };
            let tint = white(alpha);
            image(
                batch,
                atlas,
                "battle_info_underlay",
                [portrait, y + 8.0, 28.0, 28.0],
                WHOLE,
                tint,
            );
            // The status' strip goes under the frame.
            let status = pokemon.status.as_deref().and_then(party::status_name);
            if let Some(status) = status {
                let (at, of) = if left {
                    (x + 38.0, [0.5, 0.0, 1.0, 1.0])
                } else {
                    (x + 65.0, [0.0, 0.0, 0.5, 1.0])
                };
                let strip = format!("battle_status_{status}");
                image(batch, atlas, &strip, [at, y + 28.0, 37.0, 7.0], of, tint);
                let label = Text::tr(&format!("cobblemon.ui.status.{status}"), &[]).bold();
                let at = if left { x + 56.5 } else { x + 83.5 };
                small_text(batch, atlas, &label, at, y + 29.5, true, false);
            }
            // A Ball thrown at this Pokémon: its portrait shrinks away, then the Ball is there.
            let thrown = ball.as_ref().filter(|ball| ball.pokemon == pokemon.uuid);
            let clip = [portrait, y + 8.0, portrait + 28.0, y + 36.0];
            let origin = (portrait + 14.0, y + 3.0);
            match thrown {
                Some(thrown) if thrown.shows == TileShows::Ball => {
                    self.draw_pokemon_tile_ball(batch, thrown, origin, !left, clip);
                }
                _ => {
                    if let Some(look) = look {
                        let framing = Framing {
                            origin,
                            scale: 18.0 * thrown.map_or(1.0, |thrown| thrown.size),
                            reversed: !left,
                            clip,
                        };
                        self.draw_pokemon_portrait(batch, look, framing);
                    }
                }
            }
            let frame = if left {
                "battle_info_base"
            } else {
                "battle_info_base_flipped"
            };
            image(batch, atlas, frame, [x, y, 140.0, 40.0], WHOLE, tint);
            // The role's bar: the player's Pokémon in blue, the opponent's in red.
            let (role, at, colour) = if left {
                ("battle_info_role", x + 11.0, (0x29, 0x93, 0xF9))
            } else {
                ("battle_info_role_flipped", x + 102.0, (0xE4, 0x38, 0x38))
            };
            let colour = rgba(colour.0, colour.1, colour.2, (alpha * 255.0) as u8);
            image(
                batch,
                atlas,
                role,
                [at, y + 1.0, 27.0, 3.0],
                [0.0, 0.75, 1.0, 1.0],
                colour,
            );
            let name = horizon_client::entity_nbt::name_text(&pokemon.name).bold();
            fitted(batch, atlas, &name, (info, y + 7.0), 61.0, tint);
            if let Some((sign, colour)) = gender_sign(&pokemon.properties) {
                batch.text(atlas, sign, info + 63.0, y + 7.0, colour, true);
            }
            // "Lv." and the level, which Cobblemon writes 13 pixels apart: one text here, kept in
            // the tile.
            let level = Text::tr("cobblemon.ui.lv", &[])
                .append(Text::literal(pokemon.level.to_string()))
                .bold();
            fitted(batch, atlas, &level, (info + 69.0, y + 7.0), 27.0, tint);
            // The bar empties toward the tile's outer edge.
            let bar = health * 97.0;
            let at = if left {
                info - 2.0
            } else {
                info - 2.0 + (97.0 - bar)
            };
            if bar > 0.0 {
                batch.fill(atlas, at, y + 22.0, bar, 4.0, health_colour(health));
            }
            let amount = if pokemon.flat {
                let points = (health * pokemon.max_health) as u32;
                format!("{points}/{}", pokemon.max_health as u32)
            } else {
                format!("{}%", (health * 100.0).ceil() as u32)
            };
            let centre = info + if left { 39.5 } else { 44.5 };
            small_text(
                batch,
                atlas,
                &Text::literal(amount),
                centre,
                y + 22.0,
                true,
                true,
            );
        }
        // The log, faint while the screen is away; and then a line that asks for the choice.
        let away = self.screen != Screen::PokemonBattle;
        self.draw_pokemon_battle_log(batch, if away { 0.3 } else { 1.0 });
        if away && battle.must_choose {
            let atlas = &self.gui.atlas;
            let prompt = Text::tr("cobblemon.battle.ui.actions_label", &[&"R"]);
            let pulse = (self.started.elapsed().as_secs_f32() * std::f32::consts::FRAC_PI_2).sin();
            let x = (width / 2.0).floor() - atlas.font.width_text(&prompt) / 2.0;
            let y = (height / 5.0).floor();
            batch.text_styled(atlas, &prompt, x, y, white(pulse * 0.5 + 0.5), false);
        }
    }

    /// The battle's log, at the bottom right: its last lines, the newest at the foot.
    fn draw_pokemon_battle_log(&mut self, batch: &mut HudBatch, opacity: f32) {
        let (width, height) = self.gui_size();
        let Some(fight) = self.remote.as_mut().map(|r| &mut r.pokemon.fight) else {
            return;
        };
        let Some(battle) = fight.battles.current() else {
            return;
        };
        let atlas = &self.gui.atlas;
        // Each new message, wrapped to the log's width.
        for message in &battle.messages[fight.wrapped.min(battle.messages.len())..] {
            let text = horizon_client::entity_nbt::name_text(message).bold();
            fight.lines.extend(atlas.font.split(&text, 146.0));
        }
        fight.wrapped = battle.messages.len();
        let [x, y, w, h] = log_frame(width, height);
        image(
            batch,
            atlas,
            "battle_log",
            [x, y, w, h],
            WHOLE,
            white(opacity),
        );
        let (top, box_height) = (y + 6.0, 46.0);
        batch.fill(atlas, x + 5.0, top, 148.0, box_height, rgba(0, 0, 0, 112));
        // Ten pixels a line: scrolled to its foot once there are more lines than fit.
        let scroll = (10.0 * fight.lines.len() as f32 - (box_height - 4.0)).max(0.0);
        let start = batch.vertices.len();
        for (index, line) in fight.lines.iter().enumerate() {
            let line_top = y + 8.0 + 10.0 * index as f32 - scroll;
            if line_top + 10.0 >= top && line_top <= top + box_height {
                batch.text_styled(atlas, line, x + 9.0, line_top, white(opacity), false);
            }
        }
        batch.clip_since(start, x + 5.0, top, x + 158.0, top + box_height);
    }

    /// The battle's screen: what there is to choose now (`BattleGUI.render`).
    pub(in super::super::super) fn draw_pokemon_battle_screen(&mut self, batch: &mut HudBatch) {
        widgets::begin_screen(|| {
            widgets::ScreenInfo::new("pokemon_battle", "cobblemon.battle.ui.fight")
        });
        let size = self.gui_size();
        let (width, height) = size;
        let cursor = self.cursor;
        let Some((battle, menu)) = self.remote.as_ref().and_then(|r| {
            Some((
                r.pokemon.fight.battles.current()?.clone(),
                r.pokemon.fight.menu,
            ))
        }) else {
            // The battle is over, or was never told: nothing to show.
            self.screen = Screen::Playing;
            self.capture(true);
            return;
        };
        let shown = view(&battle, menu);
        let buttons = buttons(&battle, menu, size);
        // The team's portraits, made before anything is drawn.
        let looks: HashMap<u128, _> = if matches!(shown, View::Team { .. }) {
            let template = self.res.mob_types().get(super::STAND_IN).cloned();
            battle
                .team
                .iter()
                .filter_map(|p| {
                    let look =
                        self.pokemon_portrait(p.uuid, &p.species, &p.aspects, template.as_ref()?)?;
                    Some((p.uuid, look))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let atlas = &self.gui.atlas;
        let known = self.remote.as_ref().and_then(|r| r.pokemon.moves.as_ref());
        let hovered = |b: &Button| ui::hovered(cursor, b.rect[0], b.rect[1], b.rect[2], b.rect[3]);
        // `BattleGUI`'s own line: how to move around, or what a forfeit means.
        let line = match shown {
            View::Waiting | View::Root => Some(Text::tr("cobblemon.battle.ui.hide_label", &[&"R"])),
            View::Forfeit => Some(Text::tr(
                "cobblemon.battle.ui.forfeit_confirmation",
                &[&"R"],
            )),
            _ => None,
        };
        if let Some(line) = line {
            let x = (width / 2.0).floor() - atlas.font.width_text(&line) / 2.0;
            batch.text_styled(atlas, &line, x, (height / 5.0).floor(), white(0.75), false);
        }
        if let View::Team { .. } = shown {
            let top = team_top(height);
            image(
                batch,
                atlas,
                "selection_underlay",
                [0.0, top, width, 148.0],
                WHOLE,
                u32::MAX,
            );
            let title = Text::tr("cobblemon.ui.party", &[]);
            let x = (width - atlas.font.width_text(&title)) / 2.0;
            batch.text_styled(atlas, &title, x, top + 17.0, u32::MAX, true);
            // The six places, each empty under its Pokémon.
            for index in 0..6 {
                let [x, y, w, _] = team_tile(width, height, index);
                let rows = [0.0, 0.5, 1.0, 51.0 / 58.0];
                image(
                    batch,
                    atlas,
                    "party_select_disabled",
                    [x, y, w, 22.0],
                    rows,
                    u32::MAX,
                );
            }
        }
        for button in &buttons {
            let [x, y, w, h] = button.rect;
            let over = hovered(button);
            match &button.act {
                Act::Fight | Act::Team | Act::Catch | Act::Run | Act::Forfeit => {
                    let (texture, key) = match button.act {
                        Act::Fight => ("battle_menu_fight", "cobblemon.battle.ui.fight"),
                        Act::Team => ("battle_menu_switch", "cobblemon.battle.ui.switch"),
                        Act::Catch => ("battle_menu_bag", "cobblemon.battle.ui.capture"),
                        Act::Run => ("battle_menu_run", "cobblemon.battle.ui.run"),
                        _ => ("battle_menu_forfeit", "cobblemon.battle.ui.forfeit"),
                    };
                    let of = if over { LOWER } else { UPPER };
                    image(batch, atlas, texture, button.rect, of, u32::MAX);
                    let label = Text::tr(key, &[]);
                    batch.text_styled(atlas, &label, x + 6.0, y + 8.0, u32::MAX, true);
                    widgets::record(&label.plain(), button.rect, true);
                }
                Act::Move(index) => {
                    let Some(offered) = battle
                        .request
                        .as_ref()
                        .and_then(|r| r.active.first()?.moves.get(*index))
                    else {
                        continue;
                    };
                    let fade = if button.enabled { 1.0 } else { 0.5 };
                    let of = if over && button.enabled { LOWER } else { UPPER };
                    // The tile takes its move's type's colour; a move the server did not tell of
                    // is a normal one (`Moves.getByNameOrDummy`).
                    let template = known.and_then(|moves| moves.get(&offered.id));
                    let kind = template.map_or("normal", |m| m.elemental_type.as_str());
                    let hue = moves::type_hue(kind).unwrap_or(0xE8E8DA).to_be_bytes();
                    let tint = rgba(hue[1], hue[2], hue[3], (fade * 255.0) as u8);
                    image(batch, atlas, "battle_move", button.rect, of, tint);
                    image(
                        batch,
                        atlas,
                        "battle_move_overlay",
                        button.rect,
                        WHOLE,
                        u32::MAX,
                    );
                    // The type's icon, one of eighteen in a row, and the category's, one of three
                    // in a column, both at half their size.
                    if let Some(cell) = moves::type_icon(kind) {
                        let of = [cell as f32 / 18.0, 0.0, (cell + 1) as f32 / 18.0, 1.0];
                        let at = [x - 9.0, y + 2.0, 18.0, 18.0];
                        image(batch, atlas, "cobblemon:gui/types", at, of, u32::MAX);
                    }
                    if let Some(template) = template {
                        let cell = match template.category {
                            moves::DamageCategory::Physical => 0.0,
                            moves::DamageCategory::Special => 1.0,
                            moves::DamageCategory::Status => 2.0,
                        };
                        let of = [0.0, cell / 3.0, 1.0, (cell + 1.0) / 3.0];
                        let at = [x + 48.0, y + 14.5, 12.0, 8.0];
                        image(batch, atlas, "cobblemon:gui/categories", at, of, u32::MAX);
                    }
                    let name = move_name(offered).bold();
                    fitted(batch, atlas, &name, (x + 17.0, y + 2.0), 73.0, white(fade));
                    // A move the Pokémon is locked into comes without its own numbers.
                    let (left, colour) = if offered.must_be_used() {
                        ("\u{2014}/\u{2014}".to_owned(), u32::MAX)
                    } else {
                        let colour = if offered.pp == 0 {
                            rgba(0xFF, 0x55, 0x55, 255)
                        } else if offered.pp <= offered.max_pp / 2 {
                            rgba(0xFF, 0xAA, 0x00, 255)
                        } else {
                            u32::MAX
                        };
                        (format!("{}/{}", offered.pp, offered.max_pp), colour)
                    };
                    let left = Text::literal(left).bold();
                    let at = x + 75.0 - atlas.font.width_text(&left) / 2.0;
                    batch.text_styled(atlas, &left, at, y + 14.0, colour, false);
                    widgets::record(&name.plain(), button.rect, button.enabled);
                }
                Act::Switch(uuid) => {
                    let Some(pokemon) = battle.team.iter().find(|p| p.uuid == *uuid) else {
                        continue;
                    };
                    let fighting = battle.own().is_some_and(|a| {
                        a.active
                            .iter()
                            .flatten()
                            .any(|p| p.uuid == *uuid && !p.fainted)
                    });
                    let fainted = pokemon.fainted();
                    let frame = if button.enabled {
                        "party_select"
                    } else {
                        "party_select_disabled"
                    };
                    let of = if !fainted && (over || fighting) {
                        LOWER
                    } else {
                        UPPER
                    };
                    image(batch, atlas, frame, button.rect, of, u32::MAX);
                    let ball = match pokemon.caught_ball.split_once(':') {
                        Some((namespace, path)) => format!("{namespace}:gui/ball/{path}"),
                        None => format!("cobblemon:gui/ball/{}", pokemon.caught_ball),
                    };
                    let of = if fighting { LOWER } else { UPPER };
                    image(
                        batch,
                        atlas,
                        &ball,
                        [x + 85.0, y - 3.0, 9.0, 11.0],
                        of,
                        u32::MAX,
                    );
                    if let Some(look) = looks.get(uuid) {
                        // Its portrait at the tile's right, where Cobblemon stands the whole model.
                        let framing = Framing {
                            origin: (x + 77.0, y - 10.0),
                            scale: 14.0,
                            reversed: false,
                            clip: [x + 64.0, y + 1.0, x + 90.0, y + 27.0],
                        };
                        self.draw_pokemon_portrait(batch, look, framing);
                    }
                    let fade = white(if fainted || fighting { 0.7 } else { 1.0 });
                    let level = Text::tr("cobblemon.ui.lv", &[]).bold();
                    batch.text_styled(atlas, &level, x + 5.0, y + 4.0, fade, true);
                    let number = Text::literal(pokemon.level.to_string()).bold();
                    batch.text_styled(atlas, &number, x + 18.0, y + 4.0, fade, true);
                    let name = pokemon
                        .nickname
                        .as_ref()
                        .map(horizon_client::entity_nbt::name_text)
                        .filter(|name| !name.plain().is_empty())
                        .unwrap_or_else(|| super::label::species_name(&pokemon.species))
                        .bold();
                    fitted(batch, atlas, &name, (x + 5.0, y + 13.0), 58.0, fade);
                    // Its health as Showdown has it in the request; none once it has fainted.
                    let health = battle
                        .request
                        .as_ref()
                        .and_then(|r| r.team.iter().find(|m| m.uuid() == Some(*uuid)))
                        .and_then(|m| condition(&m.condition));
                    let (points, most) = health.unwrap_or((0, 0));
                    if points > 0 && most > 0 {
                        let ratio = (points as f32 / most as f32).clamp(0.0, 1.0);
                        batch.fill(
                            atlas,
                            x + 1.0,
                            y + 22.0,
                            ratio * 90.0,
                            1.0,
                            health_colour(ratio),
                        );
                    }
                    let amount = Text::literal(format!("{points}/{most}"));
                    small_text(batch, atlas, &amount, x + 14.0, y + 24.5, true, false);
                    widgets::record(&name.plain(), button.rect, button.enabled);
                }
                Act::Back => {
                    let of = if over { LOWER } else { UPPER };
                    image(batch, atlas, "battle_back", button.rect, of, u32::MAX);
                    widgets::record("back", button.rect, true);
                }
                Act::Confirm(yes) => {
                    let label = Text::tr(if *yes { "gui.yes" } else { "gui.no" }, &[]);
                    ui::button(batch, atlas, x, y, w, h, &label, true, over);
                }
            }
        }
    }

    /// The battle as `horizon-ctl status` tells it, for a test to read: who fights with how much
    /// health, what the screen offers and where each of its buttons is, and the log's last lines. Null
    /// out of a battle.
    pub(in super::super::super) fn pokemon_battle_status(&self) -> serde_json::Value {
        let Some(fight) = self.remote.as_ref().map(|r| &r.pokemon.fight) else {
            return serde_json::Value::Null;
        };
        let Some(battle) = fight.battles.current() else {
            return serde_json::Value::Null;
        };
        let pokemon = |p: &BattlePokemon| {
            serde_json::json!({
                "species": p.species,
                "level": p.level,
                "health": p.health,
                "max_health": p.max_health,
                "ratio": p.health_ratio(),
                "flat": p.flat,
                "status": p.status,
                "fainted": p.fainted,
            })
        };
        let active = |actors: &[battle::Actor]| -> Vec<serde_json::Value> {
            actors
                .iter()
                .flat_map(|a| a.active.iter().flatten())
                .map(pokemon)
                .collect()
        };
        let buttons: Vec<serde_json::Value> = buttons(battle, fight.menu, self.gui_size())
            .iter()
            .map(|b| {
                serde_json::json!({
                    "act": b.act.name(),
                    "enabled": b.enabled,
                    "x": b.rect[0] + b.rect[2] / 2.0,
                    "y": b.rect[1] + b.rect[3] / 2.0,
                })
            })
            .collect();
        let moves: Vec<&str> = battle
            .request
            .iter()
            .flat_map(|r| r.active.first())
            .flat_map(|m| m.moves.iter().map(|m| m.id.as_str()))
            .collect();
        let last: Vec<String> = battle
            .messages
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|m| horizon_client::entity_nbt::name_text(m).plain())
            .collect();
        serde_json::json!({
            "screen_away": fight.minimised,
            "must_choose": battle.must_choose,
            "view": format!("{:?}", view(battle, fight.menu)),
            "own": active(battle.own().map_or(&[][..], std::slice::from_ref)),
            "foe": active(battle.opponents()),
            "team": battle.team.iter().map(|p| p.species.as_str()).collect::<Vec<_>>(),
            "capture": battle.capture.as_ref().map(|c| serde_json::json!({
                "ball": c.ball,
                "target": c.target,
                "shakes": c.shakes,
            })),
            "moves": moves,
            "buttons": buttons,
            "messages": battle.messages.len(),
            "last_messages": last,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The player the battles were captured for, and their folder in the Cobblemon crate's fixtures.
    const ME: u128 = 0xc63d_c98f_3301_33bd_8ec0_c1a8_d3c6_f512;

    /// A captured battle played up to the `count`th message of `channel`.
    fn played(name: &str, channel: &str, count: usize) -> Battles {
        let folder = format!(
            "{}/../cobblelegacy-cobblemon/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut files: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        let mut battles = Battles::default();
        let mut seen = 0;
        for path in files {
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let message = format!("cobblemon:{}", &stem["flow-000-".len()..]);
            if battle::reads(&message) {
                battles
                    .read(&message, &std::fs::read(&path).unwrap(), ME)
                    .unwrap();
            }
            if message == channel {
                seen += 1;
                if seen == count {
                    return battles;
                }
            }
        }
        panic!("the capture has fewer than {count} {channel}");
    }

    const SCREEN: (f32, f32) = (427.0, 240.0);

    #[test]
    fn a_wild_battle_offers_its_four_actions_then_the_moves() {
        let battles = played("battle1", battle::MAKE_CHOICE, 1);
        let battle = battles.current().unwrap();
        assert_eq!(view(battle, Menu::Root), View::Root);
        assert_eq!(
            options(battle),
            [Act::Fight, Act::Team, Act::Catch, Act::Run]
        );
        // Two columns of two at the bottom left.
        let root = buttons(battle, Menu::Root, SCREEN);
        let rects: Vec<_> = root.iter().map(|b| b.rect).collect();
        assert_eq!(
            rects,
            [
                [12.0, 155.0, 90.0, 26.0],
                [105.0, 155.0, 90.0, 26.0],
                [12.0, 184.0, 90.0, 26.0],
                [105.0, 184.0, 90.0, 26.0]
            ]
        );
        // The moves, each where its rank puts it, and the way back.
        let moves = buttons(battle, Menu::Moves, SCREEN);
        let offered = &battle.request.as_ref().unwrap().active[0].moves;
        assert_eq!(moves.len(), offered.len() + 1);
        assert_eq!(moves[0].rect, [20.0, 156.0, 92.0, 24.0]);
        assert_eq!(moves[1].rect, [125.0, 156.0, 92.0, 24.0]);
        assert!(moves.iter().all(|b| b.enabled));
        assert_eq!(moves.last().unwrap().act, Act::Back);
        assert_eq!(moves.last().unwrap().rect, [9.0, 218.0, 29.0, 17.0]);
        // The team: the Pokémon that fights cannot come in, the others can.
        let team = buttons(battle, Menu::Team, SCREEN);
        let able: Vec<bool> = team.iter().map(|b| b.enabled).collect();
        assert_eq!(able, [false, true, true, true]);
        assert_eq!(team[0].rect, [118.0, 48.0, 94.0, 29.0]);
        assert_eq!(team[1].rect, [216.0, 48.0, 94.0, 29.0]);
        assert_eq!(team[2].rect, [118.0, 79.0, 94.0, 29.0]);
    }

    #[test]
    fn nothing_is_offered_until_the_server_asks_and_after_the_answer() {
        // The request is there before the server wants it answered.
        let mut battles = played("battle1", battle::QUEUE_REQUEST, 1);
        let battle = battles.current().unwrap();
        assert_eq!(view(battle, Menu::Moves), View::Waiting);
        assert!(buttons(battle, Menu::Root, SCREEN).is_empty());
        let mut battles2 = played("battle1", battle::MAKE_CHOICE, 1);
        battles2.answered();
        assert_eq!(view(battles2.current().unwrap(), Menu::Root), View::Waiting);
        battles.clear();
        assert!(battles.current().is_none());
    }

    #[test]
    fn a_fainted_pokemon_leaves_only_the_team_to_choose_from() {
        // The third time the server asks in the long capture, the player's Pokémon has fainted.
        let battles = played("battle2", battle::MAKE_CHOICE, 3);
        let battle = battles.current().unwrap();
        assert_eq!(view(battle, Menu::Root), View::Team { forced: true });
        assert_eq!(view(battle, Menu::Moves), View::Team { forced: true });
        let team = buttons(battle, Menu::Root, SCREEN);
        // No way back, and the one that fainted cannot come in.
        assert!(team.iter().all(|b| matches!(b.act, Act::Switch(_))));
        let able: Vec<bool> = team.iter().map(|b| b.enabled).collect();
        assert_eq!(able, [false, true, true]);
    }

    #[test]
    fn a_health_bar_takes_a_second_to_reach_what_the_server_said() {
        let battles = played("battle1", battle::MAKE_CHOICE, 1);
        let mut pokemon = battles.current().unwrap().opponents()[0].active[0]
            .clone()
            .unwrap();
        let mut fight = Fight::default();
        let start = Instant::now();
        assert_eq!(fight.health(&pokemon, start), 1.0);
        pokemon.health = 0.0;
        assert_eq!(fight.health(&pokemon, start), 1.0);
        let half = fight.health(&pokemon, start + HEALTH_CHANGE / 2);
        assert!((half - 0.5).abs() < 1e-3, "{half}");
        assert_eq!(fight.health(&pokemon, start + HEALTH_CHANGE * 2), 0.0);
    }

    #[test]
    fn the_small_things_a_tile_writes() {
        let male = "cobblemon:pikachu form=normal level=13 shiny=false gender=MALE";
        assert_eq!(gender_sign(male).map(|g| g.0), Some("\u{2642}"));
        assert_eq!(
            gender_sign(&male.replace("MALE", "FEMALE")).map(|g| g.0),
            Some("\u{2640}")
        );
        assert!(gender_sign(&male.replace("MALE", "GENDERLESS")).is_none());
        assert_eq!(condition("33/33"), Some((33, 33)));
        assert_eq!(condition("12/33 par"), Some((12, 33)));
        assert_eq!(condition("0 fnt"), None);
        // A tall screen centres the team's panel; a low one puts it above the actions.
        assert_eq!(team_top(400.0), 126.0);
        assert_eq!(team_top(240.0), 14.0);
        assert_eq!(log_frame(427.0, 240.0), [246.0, 155.0, 169.0, 55.0]);
    }
}
