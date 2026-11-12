//! The PC's screen on a Cobblemon server (CobbleLegacy's fork): Cobblemon's `PCGUI`.
//!
//! The server keeps its client told of what the player's PC holds, box by box, and opens the screen
//! itself when the player right-clicks a PC block (`cobblelegacy_cobblemon::pc`). The screen shows one
//! box at a time, thirty slots under the box's wallpaper, with the party's six slots beside it:
//!
//! - a click on a Pokémon takes it in hand, where it follows the pointer; a click on another slot then
//!   puts it there, swapping it with the Pokémon of that slot when there is one, and a click on its own
//!   slot puts it back. With Shift, a click sends the Pokémon straight to the other side's first free
//!   slot. Every one of these is a request: nothing moves until the server answers;
//! - the arrows beside the box's name, the left and right keys and the wheel over the box go from box to
//!   box, round both ends;
//! - the panel on the left shows the Pokémon under the pointer, or the one in hand: its model, level,
//!   name, sex, the ball it was caught in, its status, its types, then its nature, ability and moves, or
//!   its IVs, or its EVs (the arrow beside that box and the wheel over it go from one to the next);
//! - a Pokémon in hand can be released for good, after a confirmation, unless it is the party's last;
//! - the options button shows the buttons that sort the box (Shift: the other way round).
//!
//! Escape, the inventory key and the exit button close the screen, and tell the server so: it holds a
//! link between the player and the PC for as long as the screen is open, and refuses any request
//! without one.
//!
//! Not here yet: the search box, renaming a box, choosing its wallpaper (the one the server says is
//! drawn), the markings, the cosmetic item, the tooltips, the PC's sounds, the pasture's mode of this
//! screen, models that move (Cobblemon's default leaves the box's still but animates the party's and
//! the previewed one), and Cobblemon's own font: the game's is used, narrowed where it runs wide.
use super::super::super::{Game, Screen};
use super::party::small_text;
use super::{Assets, Look, anim, label::species_name, path_of};
use cobblelegacy_cobblemon::{
    moves,
    party::PartyPokemon,
    pc::{self, Around, Change, Click, PcPosition, Place, Request, SortMode, Storage},
    wire::{Gender, PoseType},
};
use horizon_client::{
    hud::{HudAtlas, HudBatch, Rect, rgba},
    items::ItemStack,
    mobs::Mob,
    text::Text,
    ui, widgets,
};
use horizon_core::geometry::Vec3;
use horizon_world::nbt::Tag;
use std::{collections::HashMap, time::Instant};
use winit::keyboard::KeyCode;

/// `PCGUI`'s measures, in GUI pixels.
const BASE_WIDTH: f32 = 349.0;
const BASE_HEIGHT: f32 = 205.0;
/// `StorageSlot.SIZE`.
const SLOT: f32 = 25.0;
/// What the small texts and icons are drawn at.
const SCALE: f32 = 0.5;
/// The party's slots the screen shows, whatever the party's length.
const PARTY_SLOTS: usize = 6;
/// The ticks the box's screen takes to light up, a twentieth of a second each.
const LOADING_TICKS: f32 = 10.0;

/// The screen's own textures, by the names the atlas gives a content mod's GUI textures.
const TEXTURES: &str = "cobblemon:gui/pc/";

/// The stats in the order the screen lists them, each with its label's key.
const STATS: [(&str, &str); 6] = [
    ("cobblemon:hp", "cobblemon.ui.stats.hp"),
    ("cobblemon:attack", "cobblemon.ui.stats.atk"),
    ("cobblemon:defence", "cobblemon.ui.stats.def"),
    ("cobblemon:special_attack", "cobblemon.ui.stats.sp_atk"),
    ("cobblemon:special_defence", "cobblemon.ui.stats.sp_def"),
    ("cobblemon:speed", "cobblemon.ui.stats.speed"),
];

/// What the window knows of the player's PC.
#[derive(Default)]
pub(super) struct Pcs {
    storage: Storage,
    /// The screen, while it is open.
    open: Option<Open>,
    /// `CobblemonClient.lastPcBoxViewed`: the box the screen opens on.
    last_box: usize,
    /// The look of each Pokémon the screen shows, by the Pokémon's id.
    shown: HashMap<u128, Shown>,
    /// A species' types by species and form. `None`: its file is not there, or does not say.
    types: HashMap<(String, String), Option<(String, Option<String>)>>,
}

/// The look a Pokémon of the screen was given, and what it was made for.
struct Shown {
    species: String,
    aspects: Vec<String>,
    look: Look,
}

/// The screen's own state (`PCGUI` and its `StorageWidget`).
struct Open {
    /// The PC it shows.
    store: u128,
    /// The box it shows.
    current: usize,
    /// The Pokémon in hand and where it was taken from (`selectedPosition`, `grabbedSlot`).
    held: Option<(Place, u128)>,
    /// `displayConfirmRelease`: the release button gave way to its yes and no.
    confirm: bool,
    /// `previewPokemon`: the Pokémon the left panel shows.
    preview: Option<u128>,
    /// `currentStatIndex`: what the panel's lower box lists: 0 the nature, ability and moves, 1 the
    /// IVs, 2 the EVs.
    page: u8,
    /// `displayOptions`: the sorting buttons show.
    options: bool,
    /// When it opened: the box lights up, and the pointer over the previewed Pokémon bobs.
    since: Instant,
}

impl Open {
    /// `resetSelected`.
    fn reset(&mut self) {
        self.held = None;
        self.preview = None;
    }

    /// `PCGUI.ticksElapsed`.
    fn ticks(&self) -> f32 {
        self.since.elapsed().as_secs_f32() * 20.0
    }
}

impl Pcs {
    /// The textures the screen's Pokémon wear, which keep their slots in the atlas.
    pub(super) fn textures(&self) -> impl Iterator<Item = String> + '_ {
        self.shown.values().map(|p| p.look.kind.texture.clone())
    }

    /// `texture` lost its slot in the atlas: the looks that wore it are made again.
    pub(super) fn forget_texture(&mut self, texture: &str) {
        self.shown.retain(|_, p| p.look.kind.texture != texture);
    }

    /// The Pokémon `uuid` of the PC the screen shows, wherever it is in it.
    fn stored(&self, uuid: u128) -> Option<&PartyPokemon> {
        let held = self.storage.pc(self.open.as_ref()?.store)?;
        held.get(held.position(uuid)?)
    }
}

/// What a slot shows of its Pokémon.
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    uuid: u128,
    species: String,
    aspects: Vec<String>,
    level: u32,
    gender: Gender,
    held_item: Option<String>,
}

impl Slot {
    fn of(pokemon: &PartyPokemon) -> Self {
        Self {
            uuid: pokemon.uuid,
            species: pokemon.species.clone(),
            aspects: pokemon.aspects.clone(),
            level: pokemon.level,
            gender: pokemon.gender,
            held_item: pokemon.held_item.clone(),
        }
    }
}

/// What the screen shows now, taken out of the stores so that the looks can be made while it draws.
#[derive(Clone, Debug)]
struct View {
    store: u128,
    current: usize,
    boxes: usize,
    name: Option<String>,
    wallpaper: String,
    /// The box's slots, then the party's.
    box_slots: Vec<Option<Slot>>,
    party: Vec<Option<Slot>>,
    held: Option<(Place, u128)>,
    confirm: bool,
    preview: Option<u128>,
    page: u8,
    options: bool,
    ticks: f32,
    /// `ServerSettings.preventCompletePartyDeposit`.
    keep_one: bool,
}

impl View {
    /// The Pokémon in the slot `place`, when the screen shows that slot.
    fn at(&self, place: Place) -> Option<&Slot> {
        match place {
            Place::Pc(at) if usize::from(at.pc_box) == self.current => {
                self.box_slots.get(usize::from(at.slot))?.as_ref()
            }
            Place::Pc(_) => None,
            Place::Party(slot) => self.party.get(usize::from(slot))?.as_ref(),
        }
    }

    fn party_count(&self) -> usize {
        self.party.iter().flatten().count()
    }

    /// What decides what a click on a slot may ask.
    fn around(&self) -> Around {
        Around {
            party_count: self.party_count(),
            party_free: self
                .party
                .iter()
                .position(Option::is_none)
                .map(|slot| slot as u16),
            box_free: self
                .box_slots
                .iter()
                .position(Option::is_none)
                .map(|slot| PcPosition::new(self.current, slot)),
            keep_one_in_party: self.keep_one,
        }
    }

    /// `canDeleteSelected`: a Pokémon is in hand, and may be released.
    fn releasable(&self) -> bool {
        self.held
            .is_some_and(|(from, _)| pc::can_release(from, self.party_count()))
    }

    /// The box's screen has lit up, and shows its Pokémon.
    fn loaded(&self) -> bool {
        self.ticks >= LOADING_TICKS
    }
}

/// What takes a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Exit,
    Next,
    Previous,
    Release,
    /// The release's confirmation: yes or no.
    Confirm(bool),
    Slot(Place),
    Options,
    Sort(SortMode),
    /// The arrow that turns the lower box's page.
    Page,
}

impl Target {
    /// The name a test reads it by.
    fn name(self) -> String {
        match self {
            Self::Exit => "exit".into(),
            Self::Next => "next".into(),
            Self::Previous => "previous".into(),
            Self::Release => "release".into(),
            Self::Confirm(true) => "yes".into(),
            Self::Confirm(false) => "no".into(),
            Self::Slot(Place::Pc(at)) => format!("box-{}", at.slot),
            Self::Slot(Place::Party(slot)) => format!("party-{slot}"),
            Self::Options => "options".into(),
            Self::Sort(mode) => format!("sort-{}", mode.name()),
            Self::Page => "page".into(),
        }
    }
}

/// The screen's top left corner on a GUI `width` by `height` (`PCGUI.init`, in whole pixels).
fn origin((width, height): (f32, f32)) -> (f32, f32) {
    (
        ((width - BASE_WIDTH) / 2.0).trunc(),
        ((height - BASE_HEIGHT) / 2.0).trunc(),
    )
}

/// The storage widget's corner: the box's screen, with the party's panel at its right.
fn storage((x, y): (f32, f32)) -> (f32, f32) {
    (x + 85.0, y + 27.0)
}

/// The slot `slot` of the box, six a row; five pixels lower while the options show.
fn box_slot(origin: (f32, f32), slot: usize, options: bool) -> [f32; 4] {
    let (x, y) = storage(origin);
    let (column, row) = ((slot % 6) as f32, (slot / 6) as f32);
    let lower = if options { 5.0 } else { 0.0 };
    [
        x + 7.0 + column * (SLOT + 2.0),
        y + 11.0 + lower + row * (SLOT + 2.0),
        SLOT,
        SLOT,
    ]
}

/// The slot `slot` of the party: two columns, the right one eight pixels lower.
fn party_slot(origin: (f32, f32), slot: usize) -> [f32; 4] {
    let (x, y) = storage(origin);
    let (right, row) = (slot % 2 == 1, (slot / 2) as f32);
    let (across, down) = if right { (SLOT + 6.0, 8.0) } else { (0.0, 0.0) };
    [
        x + 193.0 + across,
        y + 8.0 + down + row * (SLOT + 6.0),
        SLOT,
        SLOT,
    ]
}

/// Everything of the screen that takes a click and where it is, in the order Cobblemon asks them.
fn targets(view: &View, origin: (f32, f32)) -> Vec<(Target, [f32; 4])> {
    let (x, y) = origin;
    let (sx, sy) = storage(origin);
    let mut targets = vec![
        (Target::Exit, [x + 320.0, y + 186.0, 26.0, 13.0]),
        (Target::Next, [x + 220.0, y + 16.0, 7.0, 7.0]),
        (Target::Previous, [x + 117.0, y + 16.0, 7.0, 7.0]),
    ];
    if view.releasable() {
        if view.confirm {
            targets.push((Target::Confirm(true), [sx + 190.0, sy + 131.0, 30.0, 13.0]));
            targets.push((Target::Confirm(false), [sx + 226.0, sy + 131.0, 30.0, 13.0]));
        } else {
            targets.push((Target::Release, [sx + 194.0, sy + 124.0, 58.0, 16.0]));
        }
    }
    for slot in 0..pc::POKEMON_PER_BOX {
        let place = Place::Pc(PcPosition::new(view.current, slot));
        targets.push((Target::Slot(place), box_slot(origin, slot, view.options)));
    }
    for slot in 0..PARTY_SLOTS {
        let place = Place::Party(slot as u16);
        targets.push((Target::Slot(place), party_slot(origin, slot)));
    }
    targets.push((Target::Options, [x + 218.0, y + 186.0, 8.0, 8.0]));
    if view.options {
        for (index, mode) in SortMode::ALL.into_iter().enumerate() {
            let rect = [x + 92.0 + 12.0 * index as f32, y + 31.0, 10.0, 10.0];
            targets.push((Target::Sort(mode), rect));
        }
    }
    targets.push((Target::Page, [x + 1.0, y + 157.0, 5.0, 8.0]));
    targets
}

/// The slot under `cursor`, as `StorageSlot.isHovered` finds it: its far edges count.
fn slot_under(view: &View, origin: (f32, f32), cursor: (f32, f32)) -> Option<Place> {
    let within =
        |[x, y, w, h]: [f32; 4]| (x..=x + w).contains(&cursor.0) && (y..=y + h).contains(&cursor.1);
    (0..pc::POKEMON_PER_BOX)
        .map(|slot| {
            (
                Place::Pc(PcPosition::new(view.current, slot)),
                box_slot(origin, slot, view.options),
            )
        })
        .chain((0..PARTY_SLOTS).map(|slot| (Place::Party(slot as u16), party_slot(origin, slot))))
        .find(|&(_, rect)| within(rect))
        .map(|(place, _)| place)
}

/// How far up, in half pixels, the pointer over the previewed Pokémon is at `ticks`
/// (`selectPointerOffsetY`: a step every three ticks, between one down and one up).
fn pointer_offset(ticks: f32) -> f32 {
    [0.0, -1.0, 0.0, 1.0][(ticks / 3.0) as usize % 4]
}

/// A texture's sprite in the atlas (`cobblemon:textures/gui/pc/pc_base.png` is
/// `cobblemon:gui/pc/pc_base`).
fn sprite_of(texture: &str) -> Option<String> {
    let (namespace, path) = texture.split_once(':')?;
    let path = path.strip_prefix("textures/")?.strip_suffix(".png")?;
    Some(format!("{namespace}:{path}"))
}

/// The glow of a wallpaper: the texture of its name in the `glow` folder beside it, or beside the
/// folder of its other look (`PCBoxWallpaperRepository.findWallpapers`).
fn glow_of(wallpaper: &str) -> Option<String> {
    let (folder, file) = wallpaper.rsplit_once('/')?;
    let folder = folder.strip_suffix("/alt").unwrap_or(folder);
    Some(format!("{folder}/glow/{file}"))
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

const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
/// A texture that holds two states one over the other: at rest, and under the pointer.
const UPPER: [f32; 4] = [0.0, 0.0, 1.0, 0.5];
const LOWER: [f32; 4] = [0.0, 0.5, 1.0, 1.0];

/// One of the screen's textures, or the share `of` of it, stretched to `rect` as `blitk` does. A name
/// with a namespace is the sprite's own.
fn image(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    name: &str,
    [x, y, w, h]: [f32; 4],
    of: [f32; 4],
    colour: u32,
) {
    let named;
    let name = if name.contains(':') {
        name
    } else {
        named = format!("{TEXTURES}{name}");
        &named
    };
    if let Some(sprite) = atlas.sprites.get(name).copied() {
        batch.rect_uv(x, y, w, h, part(sprite, of), colour);
    }
}

/// A text in the place of Cobblemon's large font: the game's, from `x` or centred on it, narrowed to
/// `most` pixels when it is wider.
fn large(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    text: &Text,
    (x, y): (f32, f32),
    centered: bool,
    most: f32,
    colour: u32,
    shadow: bool,
) {
    let width = atlas.font.width_text(text);
    let narrow = if width > most { most / width } else { 1.0 };
    let left = if centered {
        x - width * narrow / 2.0
    } else {
        x
    };
    let start = batch.vertices.len();
    batch.text_styled(atlas, text, left, y, colour, shadow);
    if narrow < 1.0 {
        batch.scale_since(start, left, y, narrow, 1.0);
    }
}

/// White at opacity `alpha`, 0 to 1.
fn white(alpha: f32) -> u32 {
    rgba(255, 255, 255, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

fn text_of(tag: &Tag, key: &str) -> Option<String> {
    match tag.get(key) {
        Some(Tag::String(value)) => Some(value.to_string_lossy()),
        _ => None,
    }
}

/// The number a compound of stats holds for `stat`.
fn stat_of(stats: Option<&Tag>, stat: &str) -> Option<i32> {
    match stats?.get(stat) {
        Some(&Tag::Int(value)) => Some(value),
        _ => None,
    }
}

/// What the left panel shows of a Pokémon, read from what the server sent of it.
#[derive(Clone, Debug, PartialEq)]
struct Preview {
    slot: Slot,
    name: Text,
    shiny: bool,
    /// The sprite of the ball it was caught in: the item's, or the party overlay's.
    ball: [String; 2],
    /// The status' Showdown name, `fnt` for one that fainted.
    status: Option<&'static str>,
    /// The nature's name, and whether a mint changed what it does.
    nature: Option<(String, bool)>,
    ability: Option<String>,
    moves: Vec<String>,
    /// Each stat's IV, the hyper-trained one when it has one (written in italics), then its EV.
    ivs: [(i32, bool); 6],
    evs: [i32; 6],
}

impl Preview {
    fn of(pokemon: &PartyPokemon) -> Self {
        let tag = &pokemon.tag;
        let path = |id: String| path_of(&id).to_owned();
        let ball = path_of(&pokemon.caught_ball);
        let namespace = pokemon
            .caught_ball
            .split_once(':')
            .map_or("cobblemon", |(namespace, _)| namespace);
        let ivs = tag.get("IVs");
        let (base, trained) = (
            ivs.and_then(|ivs| ivs.get("Base")),
            ivs.and_then(|ivs| ivs.get("HyperTrained")),
        );
        Self {
            slot: Slot::of(pokemon),
            // `Pokemon.getDisplayName`: the nickname, unless it is empty.
            name: pokemon
                .nickname
                .as_ref()
                .map(horizon_client::entity_nbt::name_text)
                .filter(|name| !name.plain().is_empty())
                .unwrap_or_else(|| species_name(&pokemon.species)),
            shiny: pokemon.shiny,
            ball: [
                format!("{namespace}:item/poke_balls/{ball}"),
                format!("{namespace}:gui/ball/{ball}"),
            ],
            status: if pokemon.fainted() {
                Some("fnt")
            } else {
                pokemon.status_icon()
            },
            nature: text_of(tag, "Nature")
                .map(|nature| (path(nature), tag.get("MintedNature").is_some())),
            ability: tag
                .get("Ability")
                .and_then(|ability| text_of(ability, "AbilityName")),
            moves: match tag.get("MoveSet") {
                Some(Tag::List(moves)) => moves
                    .iter()
                    .filter_map(|known| text_of(known, "MoveName"))
                    .take(4)
                    .collect(),
                _ => Vec::new(),
            },
            ivs: STATS.map(|(stat, _)| match stat_of(trained, stat) {
                Some(trained) => (trained, true),
                None => (stat_of(base, stat).unwrap_or(0), false),
            }),
            evs: STATS.map(|(stat, _)| stat_of(tag.get("EVs"), stat).unwrap_or(0)),
        }
    }
}

/// A species' types from its file, the form's own where the form `form` (its Showdown id, `normal`
/// for the species' own) gives them: a form with a first type of its own has no second unless it says.
fn types_of(assets: &Assets, species: &str, form: &str) -> Option<(String, Option<String>)> {
    let json = std::fs::read_to_string(assets.species.get(species)?).ok()?;
    let data: serde_json::Value = serde_json::from_str(&json).ok()?;
    let id = |name: &str| -> String {
        name.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let of_form = data["forms"].as_array().and_then(|forms| {
        forms
            .iter()
            .find(|f| f["name"].as_str().is_some_and(|n| id(n) == form))
    });
    let kind =
        |from: &serde_json::Value, key: &str| from[key].as_str().map(str::to_ascii_lowercase);
    let own =
        of_form.and_then(|form| Some((kind(form, "primaryType")?, kind(form, "secondaryType"))));
    own.or_else(|| Some((kind(&data, "primaryType")?, kind(&data, "secondaryType"))))
}

impl Game<'_> {
    /// A message of the server that may be about a PC. Whether it was the PC's alone: the two a party
    /// and a PC share go on to the party's reader, which takes those about a party.
    pub(super) fn remote_pokemon_pc(&mut self, channel: &str, data: &[u8]) -> bool {
        let (alone, shared) = (pc::reads(channel), pc::shares(channel));
        if !(alone || shared) {
            return false;
        }
        let Some(pcs) = self.remote.as_mut().map(|r| &mut r.pokemon.pcs) else {
            return alone;
        };
        let change = match pcs.storage.read(channel, data) {
            Ok(change) => change,
            Err(e) => {
                log::warn!("Horizon: Cobblemon's {channel} does not decode: {e}");
                return alone;
            }
        };
        if change != Change::Nothing && std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
            log::info!("Horizon: the PC's {channel}: {}", trace_of(&change));
        }
        match change {
            Change::Nothing | Change::Stores => {}
            Change::Open { store, open_on, .. } => self.open_pokemon_pc_screen(store, open_on),
            // `ClosePCHandler`: for the PC the screen shows, and no other. The server sends none
            // when it refuses a request for want of a link, which closes nothing.
            Change::Close { store } => {
                let shown = pcs.open.as_ref().map(|open| open.store);
                if self.screen == Screen::PokemonPc && store.is_some() && store == shown {
                    self.close_pokemon_pc_screen();
                }
            }
            // `RequestPCBoxWallpapersHandler`: the wallpapers the client has textures for, but for
            // their other looks and their glows, which go with them.
            Change::WallpapersAsked => {
                let mut wallpapers: Vec<String> = self
                    .gui
                    .atlas
                    .sprites
                    .keys()
                    .filter_map(|sprite| {
                        let (namespace, path) = sprite.split_once(':')?;
                        let folder = path.strip_prefix("gui/pc/wallpaper/")?.rsplit_once('/')?.0;
                        (!matches!(folder.rsplit('/').next(), Some("alt" | "glow")))
                            .then(|| format!("{namespace}:textures/{path}.png"))
                    })
                    .collect();
                wallpapers.sort();
                self.pokemon_pc_ask(Request::Wallpapers(wallpapers));
            }
        }
        alone
    }

    /// Sends a request of the PC's screen.
    fn pokemon_pc_ask(&self, request: Request) {
        if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
            log::info!("Horizon: asked the PC's server for {request:?}");
        }
        self.pokemon_send(request.channel(), request.payload());
    }

    /// `OpenPCHandler`: the screen opens on the PC `store`, on the box the server says or the one
    /// last looked at.
    fn open_pokemon_pc_screen(&mut self, store: u128, open_on: Option<usize>) {
        // Over the game alone: behind another screen the PC would open unseen, so its link ends.
        if !matches!(
            self.screen,
            Screen::Playing | Screen::Chat | Screen::PokemonPc
        ) {
            self.pokemon_pc_ask(Request::Unlink);
            return;
        }
        let Some(pcs) = self.remote.as_mut().map(|r| &mut r.pokemon.pcs) else {
            return;
        };
        let boxes = pcs.storage.pc(store).map_or(0, |held| held.boxes.len());
        let current = open_on.unwrap_or(pcs.last_box);
        pcs.open = Some(Open {
            store,
            current: if current < boxes { current } else { 0 },
            held: None,
            confirm: false,
            preview: None,
            page: 0,
            options: false,
            since: Instant::now(),
        });
        self.screen = Screen::PokemonPc;
        self.keys.clear();
        self.left = false;
        self.right = false;
        self.mining = None;
        self.capture(false);
    }

    /// `PCGUI.closeNormally`: the screen goes, and the server is told its link is over.
    pub(in super::super::super) fn close_pokemon_pc_screen(&mut self) {
        if let Some(pcs) = self.remote.as_mut().map(|r| &mut r.pokemon.pcs) {
            pcs.open = None;
            pcs.shown.clear();
        }
        self.pokemon_pc_ask(Request::Unlink);
        self.screen = Screen::Playing;
        self.capture(true);
    }

    /// What the screen shows now; none when it is not open, or its PC is not there any more.
    fn pokemon_pc_view(&self) -> Option<View> {
        let pokemon = &self.remote.as_ref()?.pokemon;
        let open = pokemon.pcs.open.as_ref()?;
        let held = pokemon.pcs.storage.pc(open.store)?;
        let current = open.current.min(held.boxes.len().checked_sub(1)?);
        let shown = &held.boxes[current];
        Some(View {
            store: open.store,
            current,
            boxes: held.boxes.len(),
            name: shown.name.clone(),
            wallpaper: shown.wallpaper.clone(),
            box_slots: shown
                .slots
                .iter()
                .map(|slot| slot.as_ref().map(Slot::of))
                .collect(),
            party: (0..PARTY_SLOTS)
                .map(|slot| self.pokemon_party_member(slot).map(Slot::of))
                .collect(),
            held: open.held,
            confirm: open.confirm,
            preview: open.preview,
            page: open.page,
            options: open.options,
            ticks: open.ticks(),
            keep_one: pokemon.labels.keeps_one_in_party(),
        })
    }

    fn pokemon_pc_open(&mut self) -> Option<&mut Open> {
        self.remote.as_mut()?.pokemon.pcs.open.as_mut()
    }

    /// The Pokémon `uuid` the screen may show: one of the PC's, or of the party's.
    fn pokemon_pc_pokemon(&self, uuid: u128) -> Option<&PartyPokemon> {
        let stored = self.remote.as_ref()?.pokemon.pcs.stored(uuid);
        stored.or_else(|| {
            (0..PARTY_SLOTS)
                .filter_map(|slot| self.pokemon_party_member(slot))
                .find(|pokemon| pokemon.uuid == uuid)
        })
    }

    /// The Pokémon in hand, while it still is where it was taken from: the server may have moved
    /// it meanwhile, and then nothing is in hand any more.
    fn pokemon_pc_held(&mut self, view: &View) -> Option<(Place, u128)> {
        let (from, pokemon) = view.held?;
        let there = match from {
            // It may be in a box the screen has left.
            Place::Pc(at) => self
                .remote
                .as_ref()
                .and_then(|r| r.pokemon.pcs.storage.pc(view.store)?.get(at))
                .map(|p| p.uuid),
            Place::Party(_) => view.at(from).map(|slot| slot.uuid),
        };
        if there == Some(pokemon) {
            return Some((from, pokemon));
        }
        if let Some(open) = self.pokemon_pc_open() {
            open.reset();
            open.confirm = false;
        }
        None
    }

    /// The box `step` boxes after the one the screen shows (`StorageWidget.box`).
    fn pokemon_pc_turn(&mut self, step: i32) {
        let Some(pcs) = self.remote.as_mut().map(|r| &mut r.pokemon.pcs) else {
            return;
        };
        let Some(open) = pcs.open.as_mut() else {
            return;
        };
        let boxes = pcs
            .storage
            .pc(open.store)
            .map_or(0, |held| held.boxes.len());
        open.current = pc::box_after(open.current, step, boxes);
        pcs.last_box = open.current;
    }

    /// A left click on the PC's screen.
    pub(in super::super::super) fn pokemon_pc_click(&mut self) {
        let origin = origin(self.gui_size());
        let cursor = self.cursor;
        let shift = self.has_shift_down();
        let Some(mut view) = self.pokemon_pc_view() else {
            return;
        };
        view.held = self.pokemon_pc_held(&view);
        // The pointer as it is now, not as the last frame drew it: a script clicks between frames.
        let hit = targets(&view, origin)
            .into_iter()
            .find(|(_, [x, y, w, h])| ui::hovered(cursor, *x, *y, *w, *h));
        let Some((target, _)) = hit else {
            return;
        };
        match target {
            Target::Exit => self.close_pokemon_pc_screen(),
            Target::Next => self.pokemon_pc_turn(1),
            Target::Previous => self.pokemon_pc_turn(-1),
            Target::Release => {
                if let Some(open) = self.pokemon_pc_open() {
                    open.confirm = true;
                }
            }
            Target::Confirm(yes) => {
                if let (true, Some((from, pokemon))) = (yes, view.held) {
                    self.pokemon_pc_ask(pc::release(from, pokemon));
                }
                if let Some(open) = self.pokemon_pc_open() {
                    open.confirm = false;
                    if yes {
                        open.reset();
                    }
                }
            }
            Target::Slot(place) => {
                let there = view.at(place).map(|slot| slot.uuid);
                // The box's slots take no click until its screen has lit up.
                if matches!(place, Place::Pc(_)) && !view.loaded() {
                    return;
                }
                let click = pc::slot_click(view.held, place, there, shift, view.around());
                if let Click::Ask { request, .. } = &click {
                    self.pokemon_pc_ask(request.clone());
                }
                let Some(open) = self.pokemon_pc_open() else {
                    return;
                };
                open.confirm = false;
                match click {
                    Click::Nothing | Click::Ask { dropped: false, .. } => {}
                    Click::Grab => {
                        open.held = there.map(|pokemon| (place, pokemon));
                        open.preview = there;
                    }
                    Click::PutBack | Click::Ask { dropped: true, .. } => open.reset(),
                }
            }
            Target::Options => {
                if let Some(open) = self.pokemon_pc_open() {
                    open.options = !open.options;
                }
            }
            Target::Sort(mode) => {
                self.pokemon_pc_ask(Request::SortBox {
                    store: view.store,
                    pc_box: view.current as u16,
                    mode,
                    descending: shift,
                });
                if let Some(open) = self.pokemon_pc_open() {
                    open.reset();
                }
            }
            Target::Page => {
                if let Some(open) = self.pokemon_pc_open() {
                    open.page = (open.page + 1) % 3;
                }
            }
        }
    }

    /// The wheel turned by `steps` over the PC's screen, up when positive: over the box it goes from
    /// box to box, over the panel's lower box from page to page (`PCGUI.mouseScrolled`).
    pub(in super::super::super) fn pokemon_pc_scroll(&mut self, steps: f32) {
        let origin = origin(self.gui_size());
        let cursor = self.cursor;
        let whole = steps.trunc() as i32;
        if whole == 0 {
            return;
        }
        let (sx, sy) = storage(origin);
        if ui::hovered(cursor, sx, sy, 174.0, 155.0) {
            self.pokemon_pc_turn(-whole);
        }
        if ui::hovered(cursor, origin.0 + 9.0, origin.1 + 128.0, 63.0, 69.0)
            && let Some(open) = self.pokemon_pc_open()
        {
            open.page = (i32::from(open.page) + whole).rem_euclid(3) as u8;
        }
    }

    /// A key pressed on the PC's screen: whether it took it. The arrows go from box to box, and the
    /// inventory's key closes the screen as Escape does (`PCGUI.keyPressed`).
    pub(in super::super::super) fn pokemon_pc_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::ArrowRight => self.pokemon_pc_turn(1),
            KeyCode::ArrowLeft => self.pokemon_pc_turn(-1),
            KeyCode::KeyE => self.close_pokemon_pc_screen(),
            _ => return false,
        }
        true
    }

    /// The look of a Pokémon the screen shows: the one it has, or a new one when its species or
    /// aspects changed. None while its files are read.
    fn pokemon_pc_look(&mut self, slot: &Slot) -> Option<Look> {
        let shown = &self.remote.as_ref()?.pokemon.pcs.shown;
        if let Some(shown) = shown.get(&slot.uuid)
            && shown.species == slot.species
            && shown.aspects == slot.aspects
        {
            return Some(shown.look.clone());
        }
        let template = self.res.mob_types().get(super::STAND_IN).cloned()?;
        let look = self.pokemon_look(&slot.species, &slot.aspects, &template)?;
        self.remote.as_mut()?.pokemon.pcs.shown.insert(
            slot.uuid,
            Shown {
                species: slot.species.clone(),
                aspects: slot.aspects.clone(),
                look: look.clone(),
            },
        );
        Some(look)
    }

    /// The types of the species `species` in its form `form`, read from its file the first time.
    fn pokemon_pc_types(&mut self, species: &str, form: &str) -> Option<(String, Option<String>)> {
        let pokemon = &mut self.remote.as_mut()?.pokemon;
        let key = (species.to_owned(), form.to_owned());
        if let Some(types) = pokemon.pcs.types.get(&key) {
            return types.clone();
        }
        // Without the resources nothing is kept: they may be there by the next frame.
        let assets = pokemon.assets.get_or_insert_with(Assets::load).clone()?;
        let types = types_of(&assets, path_of(species), form);
        pokemon.pcs.types.insert(key, types.clone());
        types
    }

    /// `drawProfilePokemon`: the whole model in its profile pose, turned `turn` degrees and tilted
    /// toward the viewer, `by` pixels a block from `at`, cut to `clip` (left, top, right, bottom).
    fn draw_pokemon_profile(
        &self,
        batch: &mut HudBatch,
        look: &Look,
        at: (f32, f32),
        by: f32,
        turn: f32,
        clip: [f32; 4],
    ) {
        use glam::{Mat4, Vec3 as V};
        // Stood at the origin, facing the viewer, at rest.
        let mut mob = Mob::new(0, look.kind.clone(), Vec3::new(0.0, 0.0, 0.0), 180.0);
        mob.previous = mob.position;
        (mob.body_yaw, mob.previous_body_yaw) = (180.0, 180.0);
        (mob.head_yaw, mob.previous_head_yaw) = (180.0, 180.0);
        (mob.head_pitch, mob.previous_head_pitch) = (0.0, 0.0);
        mob.extra.model_scale = 1.0;
        mob.extra.variant = anim::with_pose(0, PoseType::Profile);
        let atlas = &self.gui.atlas;
        let lookup = |name: &str| atlas.sprites.get(name).map(|r| [r.u0, r.v0, r.u1, r.v1]);
        let mut vertices = Vec::new();
        horizon_client::mobs::cem_anim::in_gui(|| {
            horizon_client::mobs::render::render_with(
                &mob,
                0.0,
                glam::DVec3::ZERO,
                &lookup,
                240 | 240 << 8,
                self.res.mob_data(),
                &[],
                &mut vertices,
            );
        });
        let (scale, [x, y, z]) = (look.profile_scale, look.profile_translation);
        // The mob renderer stood the model in the world, y up and sized by its species; the frame
        // is laid out around the model itself, y down and unsized.
        let unsized_model = 1.0 / look.base_scale;
        let gui = Mat4::from_translation(V::new(at.0, at.1, 0.0))
            * Mat4::from_scale(V::new(by, by, -by))
            * Mat4::from_translation(V::new(x, y + 1.5 * scale, z - 4.0))
            * Mat4::from_scale(V::new(scale, scale, 1.0 / scale))
            * Mat4::from_rotation_x(13f32.to_radians())
            * Mat4::from_rotation_y(turn.to_radians())
            * Mat4::from_scale(V::new(-unsized_model, -unsized_model, unsized_model));
        // The faces turned to the viewer, far to near, each at its own depth: as the portraits.
        let mut quads: Vec<(f32, [[f32; 3]; 4], [[f32; 2]; 4], u32)> = Vec::new();
        for quad in vertices.as_chunks::<4>().0 {
            let p: [V; 4] =
                std::array::from_fn(|i| gui.transform_point3(V::from_array(quad[i].pos)));
            let normal = (p[1] - p[0]).cross(p[2] - p[0]);
            if normal.z >= 0.0 {
                continue;
            }
            let normal = normal.normalize_or_zero();
            let shade = (0.55 + 0.45 * -normal.z - 0.15 * normal.y).clamp(0.4, 1.0);
            let colour = quad[0].color;
            let channel = |shift: u32| ((((colour >> shift) & 255) as f32 * shade) as u32) << shift;
            quads.push((
                p.iter().map(|v| v.z).sum::<f32>() / 4.0,
                p.map(|v| v.to_array()),
                std::array::from_fn(|i| quad[i].uv),
                (colour & 0xFF00_0000) | channel(0) | channel(8) | channel(16),
            ));
        }
        quads.sort_by(|a, b| a.0.total_cmp(&b.0));
        let start = batch.vertices.len();
        for (_, corners, uv, colour) in quads {
            batch.quad_3d(corners, uv, colour);
        }
        batch.depth_since(start, clip);
    }

    /// `StorageSlot.renderSlot`: a slot's Pokémon with its corner at (`x`, `y`). One in hand shows
    /// neither its level nor its sex; the one the panel shows has the pointer over it.
    fn draw_pokemon_pc_slot(
        &self,
        batch: &mut HudBatch,
        slot: &Slot,
        look: Option<&Look>,
        (x, y): (f32, f32),
        in_hand: bool,
        pointed: Option<f32>,
    ) {
        if let Some(look) = look {
            let clip = [x - 2.0, y + 2.0, x + SLOT + 4.0, y + SLOT + 4.0];
            // 2.5 on the slot by `drawProfilePokemon`'s 4.5.
            self.draw_pokemon_profile(batch, look, (x + SLOT / 2.0, y + 1.0), 11.25, 35.0, clip);
        }
        let atlas = &self.gui.atlas;
        if !in_hand {
            let level = Text::tr("cobblemon.ui.lv.number", &[&slot.level]);
            small_text(batch, atlas, &level, x + 1.0, y + 1.0, false, true);
            let gender = match slot.gender {
                Gender::Male => Some("gender_icon_male"),
                Gender::Female => Some("gender_icon_female"),
                Gender::Genderless => None,
            };
            if let Some(gender) = gender {
                let rect = [x + 21.0, y + 1.0, 6.0 * SCALE, 8.0 * SCALE];
                image(batch, atlas, gender, rect, WHOLE, u32::MAX);
            }
            if let Some(item) = &slot.held_item {
                let start = batch.vertices.len();
                self.draw_item_model(batch, &ItemStack::new(item, 1), x + 16.0, y + 16.0);
                batch.scale_since(start, x + 16.0, y + 16.0, SCALE, SCALE);
            }
        }
        if let Some(offset) = pointed {
            let rect = [
                x + 10.0,
                y - 3.0 - offset * SCALE,
                11.0 * SCALE,
                8.0 * SCALE,
            ];
            image(batch, atlas, "pc_pointer", rect, WHOLE, u32::MAX);
        }
    }

    /// The PC's screen (`PCGUI.render`, then its widgets in the order it adds them).
    pub(in super::super::super) fn draw_pokemon_pc_screen(&mut self, batch: &mut HudBatch) {
        widgets::begin_screen(|| widgets::ScreenInfo::new("pokemon_pc", "cobblemon.ui.pc.title"));
        let origin = origin(self.gui_size());
        let (x, y) = origin;
        let cursor = self.cursor;
        let Some(mut view) = self.pokemon_pc_view() else {
            // The PC is gone, or was never told: nothing to show.
            self.close_pokemon_pc_screen();
            return;
        };
        view.held = self.pokemon_pc_held(&view);
        // `StorageWidget.renderWidget`: with nothing in hand, the Pokémon under the pointer is the
        // one the panel shows; the box's only once its screen has lit up.
        if view.held.is_none()
            && let Some(place) = slot_under(&view, origin, cursor)
            && (view.loaded() || matches!(place, Place::Party(_)))
            && let Some(under) = view.at(place).map(|slot| slot.uuid)
            && view.preview != Some(under)
        {
            view.preview = Some(under);
            if let Some(open) = self.pokemon_pc_open() {
                open.preview = Some(under);
            }
        }
        // The Pokémon the panel shows, while the stores still have it.
        let previewed = view
            .preview
            .and_then(|uuid| self.pokemon_pc_pokemon(uuid))
            .map(|pokemon| (Preview::of(pokemon), pokemon.form.clone()));
        let (preview, types) = match previewed {
            Some((preview, form)) => {
                let types = self.pokemon_pc_types(&preview.slot.species, &form);
                (Some(preview), types)
            }
            None => (None, None),
        };
        // Every look the screen needs, made before anything is drawn; those of Pokémon it shows no
        // more give their textures' places back.
        let mut looks: HashMap<u128, Look> = HashMap::new();
        let slots = view.box_slots.iter().chain(&view.party).flatten();
        for slot in slots.chain(preview.as_ref().map(|preview| &preview.slot)) {
            if let Some(look) = self.pokemon_pc_look(slot) {
                looks.insert(slot.uuid, look);
            }
        }
        if let Some(pcs) = self.remote.as_mut().map(|r| &mut r.pokemon.pcs) {
            pcs.shown.retain(|uuid, _| looks.contains_key(uuid));
        }
        let hovered = |[x, y, w, h]: [f32; 4]| ui::hovered(cursor, x, y, w, h);
        let state = |rect: [f32; 4]| if hovered(rect) { LOWER } else { UPPER };
        let atlas = &self.gui.atlas;

        // The panel's portrait, behind the frame.
        let portrait = [x + 6.0, y + 27.0, 66.0, 66.0];
        image(
            batch,
            atlas,
            "portrait_background",
            portrait,
            WHOLE,
            u32::MAX,
        );
        if let Some(look) = preview.as_ref().and_then(|p| looks.get(&p.slot.uuid)) {
            // `ModelWidget`: twice `drawProfilePokemon`'s 20, ten pixels above the frame's top.
            let clip = [x + 6.0, y + 27.0, x + 72.0, y + 93.0];
            self.draw_pokemon_profile(batch, look, (x + 39.0, y + 17.0), 40.0, 325.0, clip);
        }
        image(
            batch,
            atlas,
            "pc_base",
            [x, y, BASE_WIDTH, BASE_HEIGHT],
            WHOLE,
            u32::MAX,
        );
        let info = if view.page > 0 {
            "info_box_stats"
        } else {
            "info_box"
        };
        image(
            batch,
            atlas,
            info,
            [x + 9.0, y + 128.0, 63.0, 69.0],
            WHOLE,
            u32::MAX,
        );
        let label_x = x + 9.0 + 31.0;
        let title = |batch: &mut HudBatch, key: &str, at: f32| {
            let label = Text::tr(key, &[]).bold();
            small_text(batch, atlas, &label, label_x, y + at, true, false);
        };
        match view.page {
            0 => {
                title(batch, "cobblemon.ui.info.nature", 129.5);
                title(batch, "cobblemon.ui.info.ability", 146.5);
                title(batch, "cobblemon.ui.moves", 163.5);
            }
            1 => title(batch, "cobblemon.ui.stats.ivs", 129.5),
            _ => title(batch, "cobblemon.ui.stats.evs", 129.5),
        }
        match &preview {
            Some(preview) => {
                if let Some(status) = preview.status {
                    // The right half of the battle's strip, then its trim.
                    let strip = format!("cobblemon:gui/battle/battle_status_{status}");
                    let of = [35.0 / 74.0, 0.0, 1.0, 1.0];
                    image(
                        batch,
                        atlas,
                        &strip,
                        [x + 34.0, y + 1.0, 39.0, 7.0],
                        of,
                        u32::MAX,
                    );
                    let trim = "cobblemon:gui/summary/status_trim";
                    image(
                        batch,
                        atlas,
                        trim,
                        [x + 34.0, y + 2.0, 3.0, 6.0],
                        WHOLE,
                        u32::MAX,
                    );
                    let label = Text::tr(&format!("cobblemon.ui.status.{status}"), &[]).bold();
                    large(
                        batch,
                        atlas,
                        &label,
                        (x + 39.0, y),
                        false,
                        32.0,
                        u32::MAX,
                        false,
                    );
                }
                let level = Text::tr("cobblemon.ui.lv", &[]).bold();
                large(
                    batch,
                    atlas,
                    &level,
                    (x + 6.0, y + 1.5),
                    false,
                    12.0,
                    u32::MAX,
                    true,
                );
                let number = Text::literal(preview.slot.level.to_string()).bold();
                large(
                    batch,
                    atlas,
                    &number,
                    (x + 19.0, y + 1.5),
                    false,
                    14.0,
                    u32::MAX,
                    true,
                );
                let ball = [x + 3.5, y + 12.0, 8.0, 8.0];
                if atlas.sprites.contains_key(&preview.ball[0]) {
                    image(batch, atlas, &preview.ball[0], ball, WHOLE, u32::MAX);
                } else {
                    // The party overlay's, whose upper half is the ball closed.
                    image(batch, atlas, &preview.ball[1], ball, UPPER, u32::MAX);
                }
                let name = preview.name.clone().bold();
                large(
                    batch,
                    atlas,
                    &name,
                    (x + 12.0, y + 11.5),
                    false,
                    55.0,
                    u32::MAX,
                    true,
                );
                let sign = match preview.slot.gender {
                    Gender::Male => Some(("\u{2642}", rgba(0x32, 0xCB, 0xFF, 255))),
                    Gender::Female => Some(("\u{2640}", rgba(0xFC, 0x54, 0x54, 255))),
                    Gender::Genderless => None,
                };
                if let Some((sign, colour)) = sign {
                    let sign = Text::literal(sign).bold();
                    large(
                        batch,
                        atlas,
                        &sign,
                        (x + 69.0, y + 11.5),
                        false,
                        8.0,
                        colour,
                        true,
                    );
                }
                if let Some(item) = &preview.slot.held_item {
                    self.draw_item(batch, &ItemStack::new(item, 1), x + 3.0, y + 98.0);
                }
                let held_item = Text::tr("cobblemon.held_item", &[]);
                small_text(batch, atlas, &held_item, x + 24.0, y + 108.5, false, false);
                if preview.shiny {
                    let shiny = "cobblemon:gui/summary/icon_shiny";
                    image(
                        batch,
                        atlas,
                        shiny,
                        [x + 62.5, y + 28.5, 8.0, 8.0],
                        WHOLE,
                        u32::MAX,
                    );
                }
                let second = types.as_ref().and_then(|(_, second)| second.as_deref());
                let spacer = if second.is_some() {
                    "type_spacer_double"
                } else {
                    "type_spacer_single"
                };
                image(
                    batch,
                    atlas,
                    spacer,
                    [x + 9.0, y + 118.5, 63.0, 6.0],
                    WHOLE,
                    u32::MAX,
                );
                // `TypeIcon`, small and centred: nine pixels each, the second ten to the right of
                // the first, which is drawn over it.
                let icon = |batch: &mut HudBatch, kind: &str, at: f32| {
                    if let Some(cell) = moves::type_icon(kind) {
                        let of = [cell as f32 / 18.0, 0.0, (cell + 1) as f32 / 18.0, 1.0];
                        let rect = [at, y + 117.0, 9.0, 9.0];
                        image(
                            batch,
                            atlas,
                            "cobblemon:gui/types_small",
                            rect,
                            of,
                            u32::MAX,
                        );
                    }
                };
                let left = x + 40.5 - 4.5 - if second.is_some() { 5.0 } else { 0.0 };
                if let Some(second) = second {
                    icon(batch, second, left + 10.0);
                }
                if let Some((first, _)) = &types {
                    icon(batch, first, left);
                }
                let line = |batch: &mut HudBatch, text: &Text, at: f32| {
                    small_text(batch, atlas, text, label_x, y + at, true, true);
                };
                match view.page {
                    0 => {
                        if let Some((nature, minted)) = &preview.nature {
                            let nature = Text::tr(&format!("cobblemon.nature.{nature}"), &[]);
                            let nature = if *minted { nature.italic() } else { nature };
                            line(batch, &nature, 137.0);
                        }
                        if let Some(ability) = &preview.ability {
                            let ability = Text::tr(&format!("cobblemon.ability.{ability}"), &[]);
                            line(batch, &ability, 154.0);
                        }
                        for index in 0..4 {
                            let known = match preview.moves.get(index) {
                                Some(known) => Text::tr(&format!("cobblemon.move.{known}"), &[]),
                                None => Text::literal("\u{2014}"),
                            };
                            line(batch, &known, 170.5 + 7.0 * index as f32);
                        }
                    }
                    page => {
                        for (index, (_, key)) in STATS.iter().enumerate() {
                            let down = y + 139.0 + 10.0 * index as f32;
                            let label = Text::tr(key, &[]);
                            small_text(batch, atlas, &label, x + 13.0, down, false, true);
                            let value = if page == 1 {
                                let (iv, trained) = preview.ivs[index];
                                let value = Text::literal(iv.to_string());
                                if trained { value.italic() } else { value }
                            } else {
                                Text::literal(preview.evs[index].to_string())
                            };
                            small_text(batch, atlas, &value, x + 65.0, down, true, true);
                        }
                    }
                }
            }
            None => image(
                batch,
                atlas,
                "type_spacer",
                [x + 7.0, y + 118.5, 64.0, 6.0],
                WHOLE,
                u32::MAX,
            ),
        }
        for (spacer, rect) in [
            ("pc_spacer_top", [x + 86.5, y + 13.0, 171.0, 7.0]),
            ("pc_spacer_bottom", [x + 86.5, y + 189.0, 171.0, 7.0]),
            ("pc_spacer_right", [x + 275.5, y + 184.0, 32.0, 12.0]),
        ] {
            image(batch, atlas, spacer, rect, WHOLE, u32::MAX);
        }

        // The widgets, in the order `PCGUI.init` adds them.
        let all = targets(&view, origin);
        let rect_of = |target: Target| all.iter().find(|(t, _)| *t == target).map(|(_, r)| *r);
        if let Some(rect) = rect_of(Target::Exit) {
            let button = "cobblemon:gui/common/back_button";
            image(batch, atlas, button, rect, state(rect), u32::MAX);
            let icon = "cobblemon:gui/common/back_button_icon";
            let at = [rect[0] + 7.0, rect[1] + 4.0, 10.5, 5.5];
            image(batch, atlas, icon, at, WHOLE, u32::MAX);
            widgets::record("exit", rect, true);
        }
        for (target, texture) in [
            (Target::Next, "pc_arrow_next"),
            (Target::Previous, "pc_arrow_previous"),
        ] {
            if let Some(rect) = rect_of(target) {
                image(batch, atlas, texture, rect, state(rect), u32::MAX);
                widgets::record(&target.name(), rect, true);
            }
        }
        let (sx, sy) = storage(origin);
        let panel = [sx + 182.0, sy - 19.0, 82.0, 169.0];
        image(batch, atlas, "party_panel", panel, WHOLE, u32::MAX);
        let party = Text::tr("cobblemon.ui.party", &[]).bold();
        large(
            batch,
            atlas,
            &party,
            (sx + 213.0, sy - 15.5),
            true,
            60.0,
            u32::MAX,
            true,
        );
        let release = Text::tr("cobblemon.ui.pc.release", &[]).bold();
        if let Some(rect) = rect_of(Target::Release) {
            image(
                batch,
                atlas,
                "pc_release_button",
                rect,
                state(rect),
                u32::MAX,
            );
            let at = (rect[0] + 29.0, rect[1] + 3.5);
            large(batch, atlas, &release, at, true, 54.0, u32::MAX, true);
            widgets::record("release", rect, true);
        }
        if rect_of(Target::Confirm(true)).is_some() {
            large(
                batch,
                atlas,
                &release,
                (sx + 223.0, sy + 119.0),
                true,
                70.0,
                u32::MAX,
                false,
            );
        }
        for (yes, key) in [
            (true, "cobblemon.ui.generic.yes"),
            (false, "cobblemon.ui.generic.no"),
        ] {
            if let Some(rect) = rect_of(Target::Confirm(yes)) {
                image(
                    batch,
                    atlas,
                    "pc_release_button_confirm",
                    rect,
                    state(rect),
                    u32::MAX,
                );
                let label = Text::tr(key, &[]).bold();
                let at = (rect[0] + 15.0, rect[1] + 2.0);
                large(batch, atlas, &label, at, true, 26.0, u32::MAX, true);
                widgets::record(&Target::Confirm(yes).name(), rect, true);
            }
        }
        // The box's screen: its wallpaper, then the grid and the frame's shine over it. A wallpaper
        // the client has no texture for is the default one.
        let lit = white((view.ticks / LOADING_TICKS).clamp(0.0, 1.0));
        let wallpaper = Some(view.wallpaper.as_str())
            .filter(|texture| texture.contains("/pc/wallpaper/"))
            .filter(|texture| {
                sprite_of(texture).is_some_and(|sprite| atlas.sprites.contains_key(&sprite))
            })
            .unwrap_or(pc::DEFAULT_WALLPAPER);
        if let Some(sprite) = sprite_of(wallpaper) {
            image(batch, atlas, &sprite, [sx, sy, 174.0, 155.0], WHOLE, lit);
        }
        if let Some(glow) = glow_of(wallpaper).and_then(|glow| sprite_of(&glow)) {
            let rect = [sx - 17.0, sy - 17.0, 208.0, 189.0];
            image(batch, atlas, &glow, rect, WHOLE, lit);
        }
        let lower = if view.options { 5.0 } else { 0.0 };
        let grid = [sx + 7.0, sy + 11.0 + lower, 160.0, 133.0];
        image(batch, atlas, "pc_screen_grid", grid, WHOLE, u32::MAX);
        let overlay = [sx, sy, 174.0, 155.0];
        image(batch, atlas, "pc_screen_overlay", overlay, WHOLE, u32::MAX);
        let offset = pointer_offset(view.ticks);
        let in_hand = view.held.map(|(_, pokemon)| pokemon);
        for (target, rect) in &all {
            let Target::Slot(place) = *target else {
                continue;
            };
            if matches!(place, Place::Pc(_)) && !view.loaded() {
                continue;
            }
            let slot = view.at(place);
            let name = match slot {
                Some(slot) => format!("{} {}", target.name(), path_of(&slot.species)),
                None => target.name(),
            };
            widgets::record(&name, *rect, true);
            // The Pokémon in hand is drawn at the pointer, not in its slot.
            let Some(slot) = slot.filter(|slot| in_hand != Some(slot.uuid)) else {
                continue;
            };
            let pointed = (view.preview == Some(slot.uuid)).then_some(offset);
            let at = (rect[0], rect[1]);
            self.draw_pokemon_pc_slot(batch, slot, looks.get(&slot.uuid), at, false, pointed);
        }
        // The box's name, or its number.
        let name = match &view.name {
            Some(name) => Text::literal(name.clone()),
            None => Text::tr("cobblemon.ui.pc.box.title", &[&(view.current + 1)]),
        }
        .bold();
        large(
            batch,
            atlas,
            &name,
            (x + 126.0 + 45.0, y + 15.0),
            true,
            88.0,
            u32::MAX,
            false,
        );
        if let Some(rect) = rect_of(Target::Options) {
            let of = if view.options { LOWER } else { state(rect) };
            image(batch, atlas, "pc_icon_options", rect, of, u32::MAX);
            widgets::record("options", rect, true);
        }
        let reversed = self.has_shift_down();
        for mode in SortMode::ALL {
            if let Some(rect) = rect_of(Target::Sort(mode)) {
                let way = if reversed { "_reverse" } else { "" };
                let texture = format!("pc_button_sort_{}{way}", mode.name());
                image(batch, atlas, &texture, rect, state(rect), u32::MAX);
                widgets::record(&Target::Sort(mode).name(), rect, true);
            }
        }
        if let Some(rect) = rect_of(Target::Page) {
            image(batch, atlas, "info_arrow", rect, state(rect), u32::MAX);
            widgets::record("page", rect, true);
        }
        // The Pokémon in hand, over everything, its middle at the pointer.
        let held = view.held.and_then(|(_, pokemon)| {
            let slot = view.box_slots.iter().chain(&view.party).flatten();
            slot.chain(preview.as_ref().map(|preview| &preview.slot))
                .find(|slot| slot.uuid == pokemon)
        });
        if let Some(slot) = held {
            let at = (cursor.0 - 12.0, cursor.1 - 12.0);
            self.draw_pokemon_pc_slot(batch, slot, looks.get(&slot.uuid), at, true, Some(offset));
        }
    }

    /// The PC as `horizon-ctl status` tells it, for a test to read: every Pokémon the PC holds, and
    /// while the screen is open what it shows and where each thing that takes a click is. Null
    /// before the server names a PC.
    pub(in super::super::super) fn pokemon_pc_status(&self) -> serde_json::Value {
        use serde_json::{Value, json};
        let Some(pcs) = self.remote.as_ref().map(|r| &r.pokemon.pcs) else {
            return Value::Null;
        };
        // The open PC, or the player's own: the one the server named.
        let shown = pcs
            .open
            .as_ref()
            .and_then(|open| pcs.storage.pc(open.store));
        let Some(held) = shown.or_else(|| pcs.storage.pcs().next()) else {
            return Value::Null;
        };
        let mut stored = Vec::new();
        for (number, pc_box) in held.boxes.iter().enumerate() {
            for (slot, pokemon) in pc_box.slots.iter().enumerate() {
                if let Some(pokemon) = pokemon {
                    stored.push(json!({
                        "box": number,
                        "slot": slot,
                        "species": pokemon.species,
                        "level": pokemon.level,
                    }));
                }
            }
        }
        let names: Vec<Value> = held
            .boxes
            .iter()
            .enumerate()
            .filter_map(|(number, pc_box)| Some(json!([number, pc_box.name.as_ref()?])))
            .collect();
        let mut status = json!({
            "open": false,
            "boxes": held.boxes.len(),
            "named": names,
            "stored": stored,
        });
        let Some(view) = self.pokemon_pc_view() else {
            return status;
        };
        let origin = origin(self.gui_size());
        let describe = |slot: &Slot| {
            json!({
                "species": slot.species,
                "level": slot.level,
                "look": pcs.shown.contains_key(&slot.uuid),
            })
        };
        let buttons: Vec<Value> = targets(&view, origin)
            .into_iter()
            .map(|(target, [x, y, w, h])| {
                let slot = match target {
                    Target::Slot(place) => view.at(place).map(describe),
                    _ => None,
                };
                json!({
                    "act": target.name(),
                    "x": x + w / 2.0,
                    "y": y + h / 2.0,
                    "pokemon": slot,
                })
            })
            .collect();
        let preview = view
            .preview
            .and_then(|uuid| self.pokemon_pc_pokemon(uuid))
            .map(|pokemon| {
                let preview = Preview::of(pokemon);
                let key = (pokemon.species.clone(), pokemon.form.clone());
                json!({
                    "species": pokemon.species,
                    "name": preview.name.plain(),
                    "level": pokemon.level,
                    "shiny": preview.shiny,
                    "status": preview.status,
                    "nature": preview.nature.as_ref().map(|(nature, _)| nature),
                    "ability": preview.ability,
                    "moves": preview.moves,
                    "ivs": preview.ivs.map(|(iv, _)| iv),
                    "evs": preview.evs,
                    "types": pcs.types.get(&key).cloned().flatten(),
                })
            });
        let in_hand = view.held.map(|(from, pokemon)| {
            json!({
                "from": Target::Slot(from).name(),
                "box": match from {
                    Place::Pc(at) => Some(at.pc_box),
                    Place::Party(_) => None,
                },
                "species": self.pokemon_pc_pokemon(pokemon).map(|p| p.species.as_str()),
            })
        });
        status["open"] = true.into();
        status["box"] = view.current.into();
        status["name"] = view.name.clone().into();
        status["wallpaper"] = view.wallpaper.clone().into();
        status["loaded"] = view.loaded().into();
        status["held"] = in_hand.unwrap_or(Value::Null);
        status["confirm_release"] = (view.confirm && view.releasable()).into();
        status["page"] = view.page.into();
        status["options"] = view.options.into();
        status["preview"] = preview.unwrap_or(Value::Null);
        status["buttons"] = buttons.into();
        status
    }
}

/// A change as a test reads it in the log.
fn trace_of(change: &Change) -> String {
    match change {
        Change::Nothing => "nothing".into(),
        Change::Stores => "its boxes changed".into(),
        Change::Open { open_on, .. } => format!("opened on {open_on:?}"),
        Change::Close { store } => format!("closed ({})", store.is_some()),
        Change::WallpapersAsked => "the wallpapers asked for".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view of the first box, empty but for what a test puts in it.
    fn view() -> View {
        View {
            store: 1,
            current: 0,
            boxes: 40,
            name: None,
            wallpaper: pc::DEFAULT_WALLPAPER.into(),
            box_slots: vec![None; pc::POKEMON_PER_BOX],
            party: vec![None; PARTY_SLOTS],
            held: None,
            confirm: false,
            preview: None,
            page: 0,
            options: false,
            ticks: 100.0,
            keep_one: true,
        }
    }

    fn slot(uuid: u128, species: &str) -> Option<Slot> {
        Some(Slot {
            uuid,
            species: format!("cobblemon:{species}"),
            aspects: Vec::new(),
            level: 5,
            gender: Gender::Male,
            held_item: None,
        })
    }

    #[test]
    fn the_screen_is_centred_and_its_slots_are_where_cobblemon_puts_them() {
        // A window of 1280 by 720 at GUI scale 3: 427 by 240, as whole pixels.
        let at = origin((427.0, 240.0));
        assert_eq!(at, (39.0, 17.0));
        assert_eq!(origin((426.0, 241.0)), (38.0, 18.0));
        assert_eq!(storage(at), (124.0, 44.0));
        // Six a row, 27 apart, from 7 and 11 inside the box's screen.
        assert_eq!(box_slot(at, 0, false), [131.0, 55.0, 25.0, 25.0]);
        assert_eq!(box_slot(at, 5, false), [131.0 + 135.0, 55.0, 25.0, 25.0]);
        assert_eq!(box_slot(at, 6, false), [131.0, 82.0, 25.0, 25.0]);
        assert_eq!(box_slot(at, 29, false), [266.0, 55.0 + 108.0, 25.0, 25.0]);
        // The options push the slots five pixels down.
        assert_eq!(box_slot(at, 0, true)[1], 60.0);
        // The party: two columns 31 apart, the right one 8 lower, rows 31 apart.
        assert_eq!(party_slot(at, 0), [317.0, 52.0, 25.0, 25.0]);
        assert_eq!(party_slot(at, 1), [348.0, 60.0, 25.0, 25.0]);
        assert_eq!(party_slot(at, 2), [317.0, 83.0, 25.0, 25.0]);
        assert_eq!(party_slot(at, 5), [348.0, 60.0 + 62.0, 25.0, 25.0]);
    }

    #[test]
    fn what_takes_a_click_follows_what_is_in_hand() {
        let at = origin((427.0, 240.0));
        let mut shown = view();
        shown.party[0] = slot(1, "pikachu");
        shown.party[1] = slot(2, "eevee");
        shown.box_slots[3] = slot(3, "mew");
        let names = |view: &View| -> Vec<String> {
            targets(view, at)
                .into_iter()
                .map(|(target, _)| target.name())
                .filter(|name| !name.starts_with("box-") && !name.starts_with("party-"))
                .collect()
        };
        // Nothing in hand: no release.
        assert_eq!(
            names(&shown),
            ["exit", "next", "previous", "options", "page"]
        );
        assert_eq!(targets(&shown, at).len(), 5 + 30 + 6);
        // A Pokémon in hand may be released; once asked, yes or no.
        shown.held = Some((Place::Pc(PcPosition::new(0, 3)), 3));
        assert!(names(&shown).contains(&"release".to_owned()));
        shown.confirm = true;
        let confirming = names(&shown);
        assert!(confirming.contains(&"yes".to_owned()) && confirming.contains(&"no".to_owned()));
        assert!(!confirming.contains(&"release".to_owned()));
        // The party's last Pokémon is not released.
        shown.party[1] = None;
        shown.held = Some((Place::Party(0), 1));
        assert!(!shown.releasable());
        assert!(
            !names(&shown)
                .iter()
                .any(|name| name == "yes" || name == "release")
        );
        // The options show the five sorting buttons, twelve pixels apart.
        shown.options = true;
        let sorts: Vec<_> = targets(&shown, at)
            .into_iter()
            .filter(|(target, _)| matches!(target, Target::Sort(_)))
            .collect();
        assert_eq!(sorts.len(), 5);
        assert_eq!(sorts[0].1, [39.0 + 92.0, 17.0 + 31.0, 10.0, 10.0]);
        assert_eq!(sorts[4].0.name(), "sort-gender");
        assert_eq!(sorts[4].1[0], 39.0 + 92.0 + 48.0);
    }

    #[test]
    fn a_view_finds_its_pokemon_and_what_is_around_a_click() {
        let mut shown = view();
        shown.current = 2;
        shown.party[0] = slot(1, "pikachu");
        shown.party[2] = slot(2, "eevee");
        shown.box_slots[0] = slot(3, "mew");
        assert_eq!(shown.at(Place::Party(2)).unwrap().uuid, 2);
        assert_eq!(shown.at(Place::Pc(PcPosition::new(2, 0))).unwrap().uuid, 3);
        // A slot of a box the screen does not show, or one with no Pokémon.
        assert!(shown.at(Place::Pc(PcPosition::new(0, 0))).is_none());
        assert!(shown.at(Place::Party(1)).is_none() && shown.at(Place::Party(9)).is_none());
        assert_eq!(
            shown.around(),
            Around {
                party_count: 2,
                party_free: Some(1),
                box_free: Some(PcPosition::new(2, 1)),
                keep_one_in_party: true,
            }
        );
        // The slot under the pointer, its far edges included; between two slots there is none.
        let at = origin((427.0, 240.0));
        let under = |x: f32, y: f32| slot_under(&shown, at, (x, y));
        assert_eq!(under(131.0, 55.0), Some(Place::Pc(PcPosition::new(2, 0))));
        assert_eq!(under(156.0, 80.0), Some(Place::Pc(PcPosition::new(2, 0))));
        assert_eq!(under(157.0, 60.0), None);
        assert_eq!(under(158.0, 60.0), Some(Place::Pc(PcPosition::new(2, 1))));
        assert_eq!(under(350.0, 70.0), Some(Place::Party(1)));
        assert_eq!(under(10.0, 10.0), None);
        // The box's slots show once its screen has lit up.
        assert!(shown.loaded());
        shown.ticks = 9.0;
        assert!(!shown.loaded());
    }

    #[test]
    fn textures_are_named_as_the_atlas_names_them() {
        assert_eq!(
            sprite_of(pc::DEFAULT_WALLPAPER).as_deref(),
            Some("cobblemon:gui/pc/wallpaper/basic/wallpaper_basic_05")
        );
        assert_eq!(sprite_of("cobblemon:gui/pc/pc_base.png"), None);
        assert_eq!(sprite_of("textures/a.png"), None);
        // A wallpaper's glow is beside it, and beside its other look's folder.
        assert_eq!(
            glow_of(pc::DEFAULT_WALLPAPER).as_deref(),
            Some("cobblemon:textures/gui/pc/wallpaper/basic/glow/wallpaper_basic_05.png")
        );
        assert_eq!(
            glow_of("cobblemon:textures/gui/pc/wallpaper/biome/alt/wallpaper_biome_cave.png")
                .as_deref(),
            Some("cobblemon:textures/gui/pc/wallpaper/biome/glow/wallpaper_biome_cave.png")
        );
    }

    #[test]
    fn the_pointer_bobs_a_half_pixel_either_way() {
        // `PCGUI.tick`: down at tick 3, level at 6, up at 9, level at 12, and round.
        let at: Vec<f32> = [0.0, 3.0, 6.0, 9.0, 12.0, 15.0, 17.9]
            .map(pointer_offset)
            .to_vec();
        assert_eq!(at, [0.0, -1.0, 0.0, 1.0, 0.0, -1.0, -1.0]);
    }

    #[test]
    fn the_panel_reads_what_the_server_sent_of_a_pokemon() {
        let (tag, _) = horizon_world::nbt::parse_snbt(
            r#"{UUID:[I;1,2,3,4],Species:"cobblemon:mew",FormId:"normal",Level:40,Experience:0,
            Health:0,Gender:"GENDERLESS",Shiny:1b,CaughtBall:"cobblemon:ultra_ball",
            Nature:"cobblemon:brave",MintedNature:"cobblemon:jolly",
            Ability:{AbilityName:"synchronize",AbilityIndex:0},
            MoveSet:[{MoveName:"ancientpower",MovePP:5},{MoveName:"amnesia",MovePP:20}],
            IVs:{Base:{"cobblemon:hp":14,"cobblemon:speed":3},HyperTrained:{"cobblemon:speed":31}},
            EVs:{"cobblemon:hp":4,"cobblemon:attack":252},HeldItem:{id:"cobblemon:oran_berry",count:1}}"#,
        )
        .unwrap();
        let preview = Preview::of(&PartyPokemon::from_tag(tag).unwrap());
        assert_eq!(preview.name, species_name("cobblemon:mew"));
        assert!(preview.shiny);
        assert_eq!(preview.status, Some("fnt"));
        assert_eq!(
            preview.ball,
            [
                "cobblemon:item/poke_balls/ultra_ball",
                "cobblemon:gui/ball/ultra_ball"
            ]
        );
        assert_eq!(preview.nature, Some(("brave".into(), true)));
        assert_eq!(preview.ability.as_deref(), Some("synchronize"));
        assert_eq!(preview.moves, ["ancientpower", "amnesia"]);
        // HP, attack, defence, special attack, special defence, speed.
        assert_eq!(preview.ivs[0], (14, false));
        assert_eq!(preview.ivs[5], (31, true));
        assert_eq!(preview.ivs[1], (0, false));
        assert_eq!(preview.evs, [4, 252, 0, 0, 0, 0]);
        assert_eq!(
            preview.slot.held_item.as_deref(),
            Some("cobblemon:oran_berry")
        );
        assert_eq!(
            (preview.slot.level, preview.slot.gender),
            (40, Gender::Genderless)
        );
    }
}
