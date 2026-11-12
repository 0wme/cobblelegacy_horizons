//! A Pokémon's summary (CobbleLegacy's fork): Cobblemon's `Summary` screen, which its M key opens on the
//! Pokémon selected in the party.
//!
//! On the left the Pokémon itself: level, status, name, the ball it was caught in, sex, its model in
//! the profile pose, markings, held item and types. In the middle one of four tabs: Info (number,
//! species, types, original trainer, nature, ability and its description, experience), Moves (the
//! move set with PP, and the selected move's power, accuracy, effect chance and description), Stats
//! (a chart of the stats, the IVs or the EVs, and the friendship and fullness bars) and Marks. On the
//! right the party, to go from one Pokémon to another.
//!
//! What the server does not send with a Pokémon comes from its species' file, read once a species
//! (`cobblelegacy_cobblemon::species`): base stats, types, number, weight and the moves it learns. Its
//! stats are worked out here as Cobblemon's client does (`cobblelegacy_cobblemon::stats`).
//!
//! The screen changes a Pokémon only by asking the server, which answers with the update of what
//! changed: two moves changing places (the arrows left of a move), a move given up for one learnt
//! before or forgotten (the button right of it, then the list in the party's place), a nickname (a
//! click on the name, then typing; sent once the name box is left), the markings (sent as the screen
//! closes or shows another Pokémon), the held item hidden or shown, and two slots of the party changing
//! places (the party's swap button, then a drag from one slot to another).
//!
//! Not here yet: Cobblemon's own font (the game's stands in, narrowed where it is wider); the Stats
//! tab's Ride view and the species' own bars under Other (steps...); the marks themselves (the server's
//! mark registry is not read: the Marks tab shows its empty slots); the evolve button and its list; the
//! cosmetic item's slot beyond its label; the model's cry on a click, its head following the pointer,
//! its held item and its layers; tooltips (the item's, the size's, a mint's); the type a move takes
//! from the Pokémon that knows it (Hidden Power, Tera Blast: the move's own type is shown); moving the
//! cursor inside the name box (it types and erases at its end); the screen's sounds beyond the click;
//! and a party of someone else's (the screen always shows the player's own, editable).
use super::super::super::{Game, Screen};
use super::{Assets, Look, label::species_name, party::health_colour, path_of};
use cobblelegacy_cobblemon::{
    moves::{self, DamageCategory, Moves},
    party::{self, ExperienceGroup, PartyPokemon, State},
    species::Species,
    stats::{self, Ivs, Stat},
    wire::{Gender, PoseType},
};
use horizon_client::{
    hud::{HudAtlas, HudBatch, Rect, rgba},
    text::Text,
    ui, widgets,
};
use horizon_world::nbt::Tag;
use std::{collections::HashMap, sync::Arc, time::Instant};
use winit::keyboard::KeyCode;

/// `Summary.BASE_WIDTH` and `BASE_HEIGHT`, in GUI pixels.
const BASE: (f32, f32) = (331.0, 161.0);
/// The screen's own textures, by the names the atlas gives a content mod's GUI textures.
const TEXTURES: &str = "cobblemon:gui/summary/";
/// The width and height of the tab in the middle (`InfoWidget`, `MovesWidget`, `StatWidget`).
const MAIN: (f32, f32) = (134.0, 148.0);
/// `PartySlotWidget.WIDTH` and `HEIGHT`.
const SLOT: (f32, f32) = (46.0, 27.0);
/// `MoveSlotWidget.MOVE_WIDTH` and `MOVE_HEIGHT`.
const MOVE: (f32, f32) = (108.0, 22.0);
/// `SummaryScrollList.WIDTH` and `HEIGHT`: the list that takes the party's place.
const LIST: (f32, f32) = (108.0, 112.0);
/// `MoveSwapScreen.SLOT_HEIGHT` and what a row takes with its spacing.
const LEARN_HEIGHT: f32 = 18.0;
const LEARN_STEP: f32 = 21.0;
/// `SummaryScrollList.SLOT_WIDTH`.
const LEARN_WIDTH: f32 = 91.0;
/// `MoveSet.MOVE_COUNT`.
const MOVE_COUNT: usize = 4;

const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
/// A texture that holds two states one over the other: at rest, and under the pointer.
const UPPER: [f32; 4] = [0.0, 0.0, 1.0, 0.5];
const LOWER: [f32; 4] = [0.0, 0.5, 1.0, 1.0];

const WHITE: u32 = u32::MAX;

/// `StatWidget`'s colours.
fn grey() -> u32 {
    rgba(0x8F, 0x8F, 0x8F, 255)
}
fn blue() -> u32 {
    rgba(0x54, 0x8B, 0xFB, 255)
}
fn red() -> u32 {
    rgba(0xFB, 0x54, 0x54, 255)
}

/// The tabs in the middle of the screen, in the order of their buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Info,
    Moves,
    Stats,
    Marks,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Info, Self::Moves, Self::Stats, Self::Marks];

    /// The name a test reads it by, which also ends its icon's texture (`summary_tab_icon_info`).
    fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Moves => "moves",
            Self::Stats => "stats",
            Self::Marks => "marks",
        }
    }
}

/// What the Stats tab charts (`StatWidget.statOptions`, without its Ride view).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StatView {
    #[default]
    Stats,
    Ivs,
    Evs,
    Other,
}

impl StatView {
    const ALL: [Self; 4] = [Self::Stats, Self::Ivs, Self::Evs, Self::Other];

    fn name(self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Ivs => "ivs",
            Self::Evs => "evs",
            Self::Other => "other",
        }
    }

    /// The translation of its label under the chart.
    fn key(self) -> &'static str {
        match self {
            Self::Stats => "cobblemon.ui.stats",
            Self::Ivs => "cobblemon.ui.stats.ivs",
            Self::Evs => "cobblemon.ui.stats.evs",
            Self::Other => "cobblemon.ui.stats.other",
        }
    }
}

/// The stats in the order of the chart's corners, clockwise from the top (`StatWidget.statLabels`),
/// each with the end of its label's translation.
const CHART: [(Stat, &str); 6] = [
    (Stat::Hp, "hp"),
    (Stat::Attack, "atk"),
    (Stat::Defence, "def"),
    (Stat::Speed, "speed"),
    (Stat::SpecialDefence, "sp_def"),
    (Stat::SpecialAttack, "sp_atk"),
];

/// Where each corner's label is in the tab (`hexagonVerticesOffset`).
const CORNERS: [(f32, f32); 6] = [
    (67.0, 10.5),
    (122.0, 42.5),
    (122.0, 93.5),
    (67.0, 124.5),
    (12.0, 93.5),
    (12.0, 42.5),
];

/// What is right of the tab: the party, or the moves the Pokémon may take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Side {
    #[default]
    Party,
    /// `MoveSwapScreen`, for the move `replaced`; none when it fills an empty place.
    Learn { replaced: Option<String> },
}

/// The name box while it is typed in (`NicknameEntryWidget`).
#[derive(Clone, Debug)]
struct Nickname {
    value: String,
    /// When it took the keyboard: its cursor blinks from then.
    since: Instant,
}

impl Nickname {
    /// What typing `text` adds: the characters a chat box takes, up to the longest nickname the
    /// server accepts.
    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            let allowed = c != '\u{a7}' && c >= ' ' && c != '\u{7f}';
            if allowed && self.value.chars().count() < party::MAX_NICKNAME {
                self.value.push(c);
            }
        }
    }

    /// `updateNickname` once the box is left: what to ask the server for a Pokémon whose species is
    /// called `species` and whose nickname is `current`. `None`: nothing changed. `Some(None)`: no
    /// nickname any more, which a blank name and the species' own both mean.
    fn settled(&self, species: &str, current: Option<&str>) -> Option<Option<String>> {
        let name = self.value.trim();
        let name = if name.is_empty() || name == species {
            None
        } else {
            Some(name)
        };
        (name != current.filter(|current| !current.is_empty() && *current != species))
            .then(|| name.map(str::to_owned))
    }
}

/// What the screen knows of a species: its file's facts, and what it learnt as it evolved (which the
/// file of the species it evolves from says).
#[derive(Clone, Debug, PartialEq)]
struct Facts {
    species: Species,
    evolution_moves: Vec<String>,
}

/// The facts of `species` (`cobblemon:charizard`) in the form `form`, read from the resources.
fn read_facts(assets: &Assets, species: &str, form: &str) -> Option<Facts> {
    let read = |name: &str, form: &str| -> Option<Species> {
        let json = std::fs::read_to_string(assets.species.get(name)?).ok()?;
        match Species::parse(&json, form) {
            Ok(species) => Some(species),
            Err(e) => {
                log::warn!("Horizon: Cobblemon species {name}: {e}");
                None
            }
        }
    };
    let name = path_of(species);
    let own = read(name, form)?;
    let evolution_moves = own
        .pre_evolution
        .as_deref()
        .and_then(|before| read(before, "normal"))
        .map(|before| {
            before
                .evolution_moves(name)
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Facts {
        species: own,
        evolution_moves,
    })
}

/// The party's reordering (`PartyWidget.swapEnabled` and `swapSource`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Swap {
    enabled: bool,
    /// The slot whose Pokémon is being dragged.
    source: Option<usize>,
}

/// What the window keeps of the summary screen.
#[derive(Default)]
pub(super) struct Summary {
    /// The Pokémon shown, which the screen follows wherever it goes in the party.
    selected: Option<u128>,
    tab: Tab,
    stat_view: StatView,
    side: Side,
    /// The move whose description shows, by its id.
    selected_move: Option<String>,
    swap: Swap,
    /// The markings as the player set them, until the server is told (`MarkingsWidget.markingStates`).
    markings: Option<Vec<i32>>,
    nickname: Option<Nickname>,
    /// The item slot shows the cosmetic item (`showCosmeticItem`).
    cosmetic: bool,
    /// How far the list of moves to take and the selected move's description are scrolled, in GUI
    /// pixels.
    learn_scroll: f32,
    description_scroll: f32,
    /// When the screen opened: the model's animations run from then.
    opened: Option<Instant>,
    /// By species and form. `None`: the species' file is not there, or does not read.
    facts: HashMap<(String, String), Option<Arc<Facts>>>,
}

impl Summary {
    /// `Summary.open`: a new screen on the Pokémon `selected`, on its Info tab with the party beside.
    fn open(&mut self, selected: u128) {
        *self = Self {
            selected: Some(selected),
            opened: Some(Instant::now()),
            facts: std::mem::take(&mut self.facts),
            ..Self::default()
        };
    }

    /// `displayMainScreen`: another tab, built anew; the list of moves to take goes with the Moves
    /// tab it belongs to.
    fn show(&mut self, tab: Tab) {
        self.tab = tab;
        self.side = Side::Party;
        self.selected_move = None;
        (self.learn_scroll, self.description_scroll) = (0.0, 0.0);
    }

    /// `switchSelection`: another Pokémon, on the same tab.
    fn select(&mut self, pokemon: u128) {
        self.selected = Some(pokemon);
        self.markings = None;
        self.nickname = None;
        self.show(self.tab);
    }

    /// `MovesWidget.selectMove`: the move whose description shows; the same one again shows none.
    fn select_move(&mut self, id: Option<&str>) {
        self.selected_move = match id {
            Some(id) if self.selected_move.as_deref() != Some(id) => Some(id.to_owned()),
            _ => None,
        };
        self.description_scroll = 0.0;
    }
}

/// Where the screen's parts are, from its top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    x: f32,
    y: f32,
}

impl Layout {
    /// Centred in a GUI of `size`, on whole pixels as `(width - BASE_WIDTH) / 2`.
    fn of((width, height): (f32, f32)) -> Self {
        Self {
            x: ((width as i32 - BASE.0 as i32) / 2) as f32,
            y: ((height as i32 - BASE.1 as i32) / 2) as f32,
        }
    }

    /// The top left corner of the tab in the middle.
    fn main(self) -> (f32, f32) {
        (self.x + 77.0, self.y + 12.0)
    }

    /// The button of a tab (`SummaryTab`).
    fn tab(self, index: usize) -> [f32; 4] {
        [
            self.x + 78.0 + 31.0 * index as f32,
            self.y - 1.0,
            39.0,
            13.0,
        ]
    }

    fn exit(self) -> [f32; 4] {
        [self.x + 302.0, self.y + 145.0, 26.0, 13.0]
    }

    /// The model's frame (`ModelWidget`).
    fn model(self) -> [f32; 4] {
        [self.x + 6.0, self.y + 32.0, 66.0, 66.0]
    }

    /// The name box, whose click takes the keyboard (`NicknameEntryWidget.mouseClicked`).
    fn nickname(self) -> [f32; 4] {
        [self.x + 12.0, self.y + 14.0, 51.0, 11.0]
    }

    /// One of the six markings (`MarkingButton`).
    fn marking(self, index: usize) -> [f32; 4] {
        [self.x + 29.0 + 7.0 * index as f32, self.y + 102.0, 6.0, 6.0]
    }

    /// The slot of the held item, which is also the button that hides it.
    fn item(self) -> [f32; 4] {
        [self.x + 3.0, self.y + 104.0, 16.0, 16.0]
    }

    /// The button that shows the cosmetic item in the held item's place.
    fn item_kind(self) -> [f32; 4] {
        [self.x + 67.0, self.y + 113.0, 6.0, 6.0]
    }

    /// The top left corner of the party (`PartyWidget`).
    fn party(self) -> (f32, f32) {
        (self.x + 216.0, self.y + 24.0)
    }

    /// The party's slots, two a row, the right one lower (`PartyWidget`'s `init`).
    fn party_slot(self, index: usize) -> [f32; 4] {
        let (x, y) = self.party();
        let (right, row) = (index % 2 == 1, (index / 2) as f32);
        let (dx, dy) = if right { (51.0, 8.0) } else { (0.0, 0.0) };
        [x + 6.0 + dx, y + 7.0 + 32.0 * row + dy, SLOT.0, SLOT.1]
    }

    /// The button that lets slots be dragged onto one another.
    fn party_swap(self) -> [f32; 4] {
        let (x, y) = self.party();
        [x + 80.0, y - 9.0, 26.0, 14.0]
    }

    /// The list that takes the party's place (`MoveSwapScreen`).
    fn list(self) -> [f32; 4] {
        [self.x + 216.0, self.y + 23.0, LIST.0, LIST.1]
    }

    /// Row `index` of that list, scrolled by `scroll` (`ObjectSelectionList.getRowTop`).
    fn learn_row(self, index: usize, scroll: f32) -> [f32; 4] {
        let [x, y, w, _] = self.list();
        let left = x + (w / 2.0).floor() - (LEARN_WIDTH / 2.0).floor() + 2.0;
        [
            left,
            y + 4.0 + LEARN_STEP * index as f32 - scroll,
            LEARN_WIDTH,
            LEARN_HEIGHT,
        ]
    }

    /// A move of the Moves tab (`MoveSlotWidget`).
    fn move_slot(self, index: usize) -> [f32; 4] {
        let (x, y) = self.main();
        [x + 13.0, y + 6.0 + 25.0 * index as f32, MOVE.0, MOVE.1]
    }

    /// The arrow left of a move that takes it up a place, or down (`ReorderMoveButton`).
    fn move_reorder(self, index: usize, up: bool) -> [f32; 4] {
        let [x, y, ..] = self.move_slot(index);
        [x - 11.5, y + if up { 6.0 } else { 13.0 }, 4.0, 3.0]
    }

    /// The button right of a move that opens the list of moves to take (`SwapMoveButton`).
    fn move_swap(self, index: usize) -> [f32; 4] {
        let [x, y, ..] = self.move_slot(index);
        [x + 114.5, y + 6.5, 6.0, 9.0]
    }

    /// Where a click picks view `index` of the Stats tab (`StatWidget.getTabIndexFromPos`).
    fn stat_view(self, index: usize) -> [f32; 4] {
        let (x, y) = self.main();
        [x + 16.0 + 24.0 * index as f32, y + 140.0, 24.0, 9.0]
    }

    /// The frame the selected move's description scrolls in (`MoveDescriptionScrollList`).
    fn description(self) -> [f32; 4] {
        let (x, y) = self.main();
        [x + 69.0, y + 113.0, 60.0, 30.0]
    }
}

/// How far the list of `rows` moves to take scrolls (`AbstractSelectionList.getMaxScroll`).
fn learn_scroll_limit(rows: usize) -> f32 {
    (LEARN_STEP * rows as f32 - (LIST.1 - 4.0)).max(0.0)
}

/// One move of the move set, as its tile shows it.
#[derive(Clone, Debug, PartialEq)]
struct MoveLine {
    id: String,
    /// The type's Showdown id, `normal` for a move the server did not tell of.
    kind: String,
    category: DamageCategory,
    power: f64,
    accuracy: f64,
    /// The chance of its first secondary effect, 0 without one.
    effect: f64,
    pp: u32,
    max_pp: u32,
}

/// What the registry says of the move `id`, with `raised` PP Ups; a move it does not have is
/// Cobblemon's dummy, a Normal status move without PP.
fn move_facts(known: Option<&Moves>, id: &str, pp: u32, raised: u32) -> MoveLine {
    let template = known.and_then(|moves| moves.get(id));
    MoveLine {
        id: id.to_owned(),
        kind: template.map_or_else(|| "normal".to_owned(), |m| m.elemental_type.clone()),
        category: template.map_or(DamageCategory::Status, |m| m.category),
        power: template.map_or(0.0, |m| m.power),
        accuracy: template.map_or(0.0, |m| m.accuracy),
        effect: template
            .and_then(|m| m.effect_chances.first().copied())
            .unwrap_or(0.0),
        pp,
        max_pp: party::max_pp(template.map_or(0, |m| m.pp), raised),
    }
}

/// `Pokemon.relearnableMoves`: the moves the Pokémon may take, in Cobblemon's order: what its levels
/// taught, what it gave up, what its evolution taught; without those it knows now, and only moves the
/// server has.
fn learnable(
    pokemon: &PartyPokemon,
    facts: Option<&Facts>,
    known: Option<&Moves>,
) -> Vec<MoveLine> {
    let mut ids: Vec<&str> = Vec::new();
    let mut add = |id: &'_ str| -> Option<()> {
        let id = known?.get(id)?.name.as_str();
        (!ids.contains(&id) && !pokemon.moves.iter().any(|m| m.id == id)).then(|| ids.push(id))
    };
    for id in facts
        .iter()
        .flat_map(|facts| facts.species.moves_up_to(pokemon.level))
    {
        add(id);
    }
    for benched in &pokemon.benched_moves {
        add(&benched.id);
    }
    for id in facts.iter().flat_map(|facts| &facts.evolution_moves) {
        add(id);
    }
    ids.into_iter()
        .map(|id| {
            let raised = pokemon
                .benched_moves
                .iter()
                .find(|benched| benched.id == id)
                .map_or(0, |benched| benched.raised_pp_stages);
            let mut line = move_facts(known, id, 0, raised);
            line.pp = line.max_pp;
            line
        })
        .collect()
}

/// `PokemonSizeCategory.fromScale`: which fifth of the server's range of sizes `scale` is in, as the
/// end of its icon's texture.
fn size_category(scale: f32, [least, most]: [f32; 2]) -> &'static str {
    let range = (most - least).max(0.0001);
    let step = range / 5.0;
    let index = ((scale - least).clamp(0.0, range) / step) as usize;
    ["xs", "s", "m", "l", "xl"][index.min(4)]
}

/// `MovesWidget.format`: a chance as a percentage with at most two decimals, rounded up; a dash for
/// none.
fn percent(value: f64) -> String {
    if value <= 0.0 {
        return "\u{2014}".to_owned();
    }
    // Through millionths, so that a tenth is not what a float makes of it.
    let hundredths = ((value * 1e6).round() as i64 + 9_999) / 10_000;
    let (whole, part) = (hundredths / 100, hundredths % 100);
    match part {
        0 => format!("{whole}%"),
        part if part % 10 == 0 => format!("{whole}.{}%", part / 10),
        part => format!("{whole}.{part:02}%"),
    }
}

/// A move's power as its tile writes it: a dash for none.
fn power(value: f64) -> String {
    match value as i64 {
        value if value > 0 => value.to_string(),
        _ => "\u{2014}".to_owned(),
    }
}

/// What the server's settings say that a page needs.
#[derive(Clone, Copy, Debug)]
struct Limits {
    max_level: u32,
    /// The least and greatest size of a Pokémon; none before the server said.
    sizes: Option<[f32; 2]>,
}

/// What the screen shows of the selected Pokémon.
#[derive(Clone, Debug, PartialEq)]
struct Page {
    uuid: u128,
    /// Where it is in the party.
    slot: usize,
    species: String,
    aspects: Vec<String>,
    /// `Pokemon.getDisplayName`: the nickname, unless it is empty.
    name: Text,
    species_name: Text,
    nickname: Option<String>,
    level: u32,
    gender: Gender,
    shiny: bool,
    /// The sprite of the ball it was caught in, the item's.
    ball: String,
    /// The Showdown name of what its status line says: `fnt` once fainted.
    status: Option<&'static str>,
    /// The form's types, when its species' file reads.
    types: Option<(String, Option<String>)>,
    held_item: Option<Tag>,
    cosmetic_item: Option<Tag>,
    held_item_visible: bool,
    dex: Option<u32>,
    /// The end of its size icon's texture: `xs` to `xl`, or `alpha`.
    size: &'static str,
    trainer: String,
    /// The nature's name, and whether a mint gave it.
    nature: String,
    minted: bool,
    ability: String,
    experience: u32,
    /// The experience its level spans, and how far through it is.
    to_next: u32,
    experience_ratio: f32,
    /// By `Stat as usize`; none without the species' base stats.
    stats: Option<[u32; 6]>,
    health: u32,
    ivs: Ivs,
    evs: [u32; 6],
    /// What the nature that counts raises and lowers.
    raised: Option<Stat>,
    lowered: Option<Stat>,
    friendship: u32,
    fullness: u32,
    max_fullness: Option<u32>,
    /// The Moves tab's places: four when there is a move to take, else only the moves it knows.
    moves: Vec<Option<MoveLine>>,
    learnable: Vec<MoveLine>,
    markings: Vec<i32>,
}

impl Page {
    fn of(
        pokemon: &PartyPokemon,
        slot: usize,
        facts: Option<&Facts>,
        known: Option<&Moves>,
        limits: Limits,
    ) -> Self {
        let species = facts.map(|facts| &facts.species);
        let species_name = species_name(&pokemon.species);
        let nickname = pokemon
            .nickname
            .as_ref()
            .map(horizon_client::entity_nbt::name_text)
            .filter(|name| !name.plain().is_empty());
        let group = species.and_then(|species| species.experience);
        // `InfoWidget`: the experience the level spans, and how much of it is had.
        let (to_next, experience_ratio) = group.map_or((0, 0.0), |group| {
            experience_span(group, pokemon.level, pokemon.experience, limits.max_level)
        });
        let nature = pokemon.effective_nature();
        let learnable = learnable(pokemon, facts, known);
        let mut moves: Vec<Option<MoveLine>> = pokemon
            .moves
            .iter()
            .map(|m| Some(move_facts(known, &m.id, m.pp, m.raised_pp_stages)))
            .collect();
        // `MovesWidget`: with a move to take, the empty places show too.
        if !learnable.is_empty() {
            moves.resize(MOVE_COUNT.max(moves.len()), None);
        }
        Self {
            uuid: pokemon.uuid,
            slot,
            species: pokemon.species.clone(),
            aspects: pokemon.aspects.clone(),
            name: nickname.clone().unwrap_or_else(|| species_name.clone()),
            species_name,
            nickname: nickname.map(|name| name.plain()),
            level: pokemon.level,
            gender: pokemon.gender,
            shiny: pokemon.shiny,
            ball: match pokemon.caught_ball.split_once(':') {
                Some((namespace, path)) => format!("{namespace}:item/poke_balls/{path}"),
                None => format!("cobblemon:item/poke_balls/{}", pokemon.caught_ball),
            },
            status: if pokemon.fainted() {
                Some("fnt")
            } else {
                pokemon.status_icon()
            },
            types: species.map(|s| (s.primary_type.clone(), s.secondary_type.clone())),
            held_item: pokemon.tag.get("HeldItem").cloned(),
            cosmetic_item: pokemon.tag.get("CosmeticItem").cloned(),
            held_item_visible: pokemon.held_item_visible,
            dex: species.map(|species| species.dex),
            size: match limits.sizes {
                _ if pokemon.alpha => "alpha",
                Some(sizes) => size_category(pokemon.scale_modifier, sizes),
                None => "m",
            },
            // `InfoWidget`: the trainer's name, else the start of who it is.
            trainer: match (&pokemon.original_trainer_name, &pokemon.original_trainer) {
                (Some(name), _) => name.clone(),
                (None, Some(id)) => format!("{}...", id.chars().take(15).collect::<String>()),
                (None, None) => String::new(),
            },
            nature: path_of(pokemon.minted_nature.as_deref().unwrap_or(&pokemon.nature)).to_owned(),
            minted: pokemon.minted_nature.is_some(),
            ability: pokemon.ability.clone(),
            experience: pokemon.experience,
            to_next,
            experience_ratio,
            stats: species.map(|s| Stat::ALL.map(|stat| pokemon.stat(stat, &s.base_stats))),
            health: pokemon.health,
            ivs: pokemon.ivs,
            evs: pokemon.evs,
            raised: nature.and_then(|nature| nature.increased),
            lowered: nature.and_then(|nature| nature.decreased),
            friendship: pokemon.friendship,
            fullness: pokemon.fullness,
            max_fullness: species.map(Species::max_fullness),
            moves,
            learnable,
            markings: pokemon.markings.clone(),
        }
    }

    /// How far each corner of the chart is from its middle, 0 to 1 (`StatWidget.renderWidget`).
    fn chart(&self, view: StatView) -> [f32; 6] {
        CHART.map(|(stat, _)| match view {
            StatView::Stats => self.stats.map_or(0.0, |s| s[stat as usize] as f32 / 400.0),
            StatView::Ivs => self.ivs.effective(stat) as f32 / Ivs::MAX as f32,
            StatView::Evs => self.evs[stat as usize] as f32 / stats::MAX_EV as f32,
            StatView::Other => 0.0,
        })
    }

    /// An IV as the chart writes it: the trained one in brackets after the one it was born with.
    fn iv(&self, stat: Stat) -> String {
        let base = self.ivs.base[stat as usize];
        match self.ivs.hyper_trained[stat as usize] {
            Some(trained) => format!("{base} ({trained})"),
            None => base.to_string(),
        }
    }

    /// What is written under each corner's label (`statValuesAsText`); with the pointer on a label,
    /// IVs and EVs as a share of the most they can be.
    fn chart_values(&self, view: StatView, hovered: bool) -> [String; 6] {
        CHART.map(|(stat, _)| match view {
            StatView::Stats => match (self.stats, stat) {
                (Some(stats), Stat::Hp) => format!("{} / {}", self.health, stats[0]),
                (Some(stats), stat) => stats[stat as usize].to_string(),
                (None, _) => "?".to_owned(),
            },
            StatView::Ivs if hovered => {
                format!("{}%", self.ivs.effective(stat) * 100 / Ivs::MAX)
            }
            StatView::Ivs => self.iv(stat),
            StatView::Evs if hovered => {
                format!("{}%", self.evs[stat as usize] * 100 / stats::MAX_EV)
            }
            StatView::Evs => self.evs[stat as usize].to_string(),
            StatView::Other => "0".to_owned(),
        })
    }

    /// What a corner's label reads in place of its stat's name while the pointer is on a label: an
    /// IV or an EV out of the most it can be.
    fn chart_label(&self, view: StatView, stat: Stat) -> Option<String> {
        match view {
            StatView::Ivs => Some(format!("{}/{}", self.iv(stat), Ivs::MAX)),
            StatView::Evs => Some(format!("{}/{}", self.evs[stat as usize], stats::MAX_EV)),
            _ => None,
        }
    }

    /// The move of the move set the description is of.
    fn move_line(&self, id: &str) -> Option<&MoveLine> {
        self.moves.iter().flatten().find(|line| line.id == id)
    }

    /// The rows of the list that takes the party's place: each move it may take, and last an empty
    /// one that forgets the move, when a move is replaced and is not its only one.
    fn learn_rows(&self, replaced: Option<&str>) -> Vec<Option<&MoveLine>> {
        let mut rows: Vec<Option<&MoveLine>> = self.learnable.iter().map(Some).collect();
        if replaced.is_some() && self.moves.iter().flatten().count() > 1 {
            rows.push(None);
        }
        rows
    }
}

/// `InfoWidget`'s two numbers under the experience bar: what the level `level` spans in `group`, and
/// how far through it `experience` is, 0 to 1.
fn experience_span(
    group: ExperienceGroup,
    level: u32,
    experience: u32,
    max_level: u32,
) -> (u32, f32) {
    let start = if level == 1 {
        0
    } else {
        group.experience(level)
    };
    let next = (level + 1).min(max_level);
    let span = group
        .experience(next)
        .saturating_sub(group.experience(level));
    let ratio = if span == 0 {
        0.0
    } else {
        (experience.saturating_sub(start) as f32 / span as f32).clamp(0.0, 1.0)
    };
    (span, ratio)
}

/// What a slot of the party shows of its Pokémon (`PartySlotWidget`).
#[derive(Clone, Debug, PartialEq)]
struct Member {
    uuid: u128,
    species: String,
    aspects: Vec<String>,
    name: Text,
    level: u32,
    /// How full its health bar is.
    health: f32,
    fainted: bool,
    /// The sprite of its ball, the party's.
    ball: String,
    status: Option<&'static str>,
    gender: Gender,
    /// The icon of one that is out of its ball.
    out: Option<&'static str>,
    held_item: Option<Tag>,
}

impl Member {
    fn of(pokemon: &PartyPokemon, facts: Option<&Facts>) -> Self {
        let full = facts.map(|facts| pokemon.stat(Stat::Hp, &facts.species.base_stats));
        Self {
            uuid: pokemon.uuid,
            species: pokemon.species.clone(),
            aspects: pokemon.aspects.clone(),
            name: pokemon
                .nickname
                .as_ref()
                .map(horizon_client::entity_nbt::name_text)
                .filter(|name| !name.plain().is_empty())
                .unwrap_or_else(|| species_name(&pokemon.species)),
            level: pokemon.level,
            health: match full {
                Some(full) if full > 0 => (pokemon.health as f32 / full as f32).clamp(0.0, 1.0),
                _ => f32::from(u8::from(!pokemon.fainted())),
            },
            fainted: pokemon.fainted(),
            ball: match pokemon.caught_ball.split_once(':') {
                Some((namespace, path)) => format!("{namespace}:gui/ball/{path}"),
                None => format!("cobblemon:gui/ball/{}", pokemon.caught_ball),
            },
            status: pokemon.status_icon(),
            gender: pokemon.gender,
            out: match pokemon.state {
                State::Inactive => None,
                State::SentOut { .. } => Some("cobblemon:gui/party/party_icon_released"),
                State::Shouldered { left: true } => {
                    Some("cobblemon:gui/party/party_icon_shoulder_left")
                }
                State::Shouldered { left: false } => {
                    Some("cobblemon:gui/party/party_icon_shoulder_right")
                }
            },
            held_item: pokemon.tag.get("HeldItem").cloned(),
        }
    }
}

/// What a click on the screen does.
#[derive(Clone, Debug, PartialEq)]
enum Act {
    Tab(Tab),
    /// The back button: from the list of moves to the party, from the party out of the screen.
    Exit,
    /// The name box takes the keyboard.
    Nickname,
    Marking(usize),
    /// The held item shows on the model, or does not.
    ItemVisibility,
    /// The item slot shows the held item, or the cosmetic one.
    ItemKind,
    /// A slot of the party: its Pokémon is shown, or with the swap button on, dragged.
    Slot(usize),
    PartySwap,
    StatView(StatView),
    /// A move's tile: its description shows.
    Move(usize),
    /// The move in this place goes up a place, or down.
    Reorder(usize, bool),
    /// The list of moves to take opens for this place, or closes.
    Swap(usize),
    /// A row of that list: the move at this rank is taken, or with none the replaced one forgotten.
    Learn(Option<String>),
}

impl Act {
    /// The name a test reads it by.
    fn name(&self) -> String {
        match self {
            Self::Tab(tab) => format!("tab-{}", tab.name()),
            Self::Exit => "exit".into(),
            Self::Nickname => "nickname".into(),
            Self::Marking(index) => format!("marking-{index}"),
            Self::ItemVisibility => "item-visibility".into(),
            Self::ItemKind => "item-kind".into(),
            Self::Slot(index) => format!("slot-{index}"),
            Self::PartySwap => "party-swap".into(),
            Self::StatView(view) => format!("view-{}", view.name()),
            Self::Move(index) => format!("move-{index}"),
            Self::Reorder(index, true) => format!("move-up-{index}"),
            Self::Reorder(index, false) => format!("move-down-{index}"),
            Self::Swap(index) => format!("move-swap-{index}"),
            Self::Learn(Some(id)) => format!("learn-{id}"),
            Self::Learn(None) => "forget".into(),
        }
    }
}

/// What the draw and the click both go by: the screen's state, taken out of the window once.
#[derive(Clone, Debug)]
struct View {
    layout: Layout,
    page: Page,
    members: Vec<Option<Member>>,
    tab: Tab,
    stat_view: StatView,
    side: Side,
    selected_move: Option<String>,
    swap: Swap,
    /// The markings as the buttons show them: the player's own changes over the server's.
    markings: Vec<i32>,
    /// The name box's text while it is typed in, and whether its cursor shows now.
    nickname: Option<(String, bool)>,
    cosmetic: bool,
    learn_scroll: f32,
    description_scroll: f32,
    /// How long the model has been shown, in ticks.
    age: f32,
}

impl View {
    /// Everything that takes a click, the first that holds the pointer being the one that does:
    /// where it is (left, top, width, height) and what it does.
    fn targets(&self) -> Vec<([f32; 4], Act)> {
        let layout = self.layout;
        let mut targets = Vec::new();
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            targets.push((layout.tab(index), Act::Tab(tab)));
        }
        targets.push((layout.exit(), Act::Exit));
        targets.push((layout.nickname(), Act::Nickname));
        for index in 0..self.markings.len().min(6) {
            targets.push((layout.marking(index), Act::Marking(index)));
        }
        if self.page.held_item.is_some() && !self.cosmetic {
            targets.push((layout.item(), Act::ItemVisibility));
        }
        targets.push((layout.item_kind(), Act::ItemKind));
        match &self.side {
            Side::Party => {
                targets.push((layout.party_swap(), Act::PartySwap));
                for index in 0..self.members.len().min(6) {
                    targets.push((layout.party_slot(index), Act::Slot(index)));
                }
            }
            Side::Learn { replaced } => {
                let frame = layout.list();
                for (index, row) in self.page.learn_rows(replaced.as_deref()).iter().enumerate() {
                    let [x, y, w, h] = layout.learn_row(index, self.learn_scroll);
                    // Only what of a row shows in the list's frame.
                    let top = y.max(frame[1]);
                    let bottom = (y + h).min(frame[1] + frame[3]);
                    if bottom > top {
                        let act = Act::Learn(row.map(|line| line.id.clone()));
                        targets.push(([x, top, w, bottom - top], act));
                    }
                }
            }
        }
        match self.tab {
            Tab::Stats => {
                for (index, view) in StatView::ALL.into_iter().enumerate() {
                    targets.push((layout.stat_view(index), Act::StatView(view)));
                }
            }
            Tab::Moves => {
                for (index, line) in self.page.moves.iter().enumerate() {
                    if line.is_some() {
                        targets.push((layout.move_reorder(index, true), Act::Reorder(index, true)));
                        targets.push((
                            layout.move_reorder(index, false),
                            Act::Reorder(index, false),
                        ));
                        targets.push((layout.move_slot(index), Act::Move(index)));
                    }
                    targets.push((layout.move_swap(index), Act::Swap(index)));
                }
            }
            Tab::Info | Tab::Marks => {}
        }
        targets
    }

    /// What a click at `cursor` does.
    fn hit(&self, cursor: (f32, f32)) -> Option<Act> {
        self.targets()
            .into_iter()
            .find(|(rect, _)| over(cursor, *rect))
            .map(|(_, act)| act)
    }

    /// The slot of the party under `cursor` (`PartyWidget.getIndexFromPos`).
    fn slot_at(&self, cursor: (f32, f32)) -> Option<usize> {
        (0..self.members.len().min(6)).find(|&index| over(cursor, self.layout.party_slot(index)))
    }
}

fn over(cursor: (f32, f32), [x, y, w, h]: [f32; 4]) -> bool {
    ui::hovered(cursor, x, y, w, h)
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

/// Where a text starts from the point it is given.
#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Centre,
    Right,
}

/// What the screen draws with: the frame's batch, and the atlas its textures and its font are in.
struct Pen<'a> {
    batch: &'a mut HudBatch,
    atlas: &'a HudAtlas,
}

impl Pen<'_> {
    /// One of the screen's textures (or another of Cobblemon's by its full name), or the share `of`
    /// of it, stretched to `rect` and tinted as `blitk` does.
    fn part(&mut self, name: &str, [x, y, w, h]: [f32; 4], of: [f32; 4], colour: u32) {
        let name = if name.contains(':') {
            name.to_owned()
        } else {
            format!("{TEXTURES}{name}")
        };
        if let Some(sprite) = self.atlas.sprites.get(&name).copied() {
            self.batch.rect_uv(x, y, w, h, part(sprite, of), colour);
        }
    }

    fn image(&mut self, name: &str, rect: [f32; 4]) {
        self.part(name, rect, WHOLE, WHITE);
    }

    /// A texture that holds two states one over the other: the lower one when `lit`.
    fn button(&mut self, name: &str, rect: [f32; 4], lit: bool) {
        self.part(name, rect, if lit { LOWER } else { UPPER }, WHITE);
    }

    fn fill(&mut self, [x, y, w, h]: [f32; 4], colour: u32) {
        if w > 0.0 {
            self.batch.fill(self.atlas, x, y, w, h, colour);
        }
    }

    /// `drawScaledText` at half size, in the game's font.
    fn small(&mut self, text: &Text, (x, y): (f32, f32), align: Align, colour: u32, shadow: bool) {
        let width = self.atlas.font.width_text(text) * 0.5;
        let left = match align {
            Align::Left => x,
            Align::Centre => x - width / 2.0,
            Align::Right => x - width,
        };
        let start = self.batch.vertices.len();
        self.batch
            .text_styled(self.atlas, text, left, y, colour, shadow);
        self.batch.scale_since(start, left, y, 0.5, 0.5);
    }

    /// A small text in white, without a shadow.
    fn text(&mut self, text: &Text, at: (f32, f32), align: Align) {
        self.small(text, at, align, WHITE, false);
    }

    /// A small text in white over its shadow.
    fn shadowed(&mut self, text: &Text, at: (f32, f32), align: Align) {
        self.small(text, at, align, WHITE, true);
    }

    /// A text Cobblemon writes in its own large font, bold over its shadow: in the game's font
    /// here, narrowed to `most` pixels where that is wider.
    fn large(&mut self, text: &Text, (x, y): (f32, f32), most: f32, align: Align, colour: u32) {
        let text = text.clone().bold();
        let width = self.atlas.font.width_text(&text);
        let by = if width > most { most / width } else { 1.0 };
        let left = match align {
            Align::Left => x,
            Align::Centre => x - width * by / 2.0,
            Align::Right => x - width * by,
        };
        let start = self.batch.vertices.len();
        self.batch
            .text_styled(self.atlas, &text, left, y, colour, true);
        if by < 1.0 {
            self.batch.scale_since(start, left, y, by, 1.0);
        }
    }

    /// A type's icon, one of eighteen in a row, at half its size (`TypeIcon`).
    fn type_icon(&mut self, kind: &str, x: f32, y: f32) {
        if let Some(cell) = moves::type_icon(kind) {
            let of = [cell as f32 / 18.0, 0.0, (cell + 1) as f32 / 18.0, 1.0];
            self.part("cobblemon:gui/types", [x, y, 18.0, 18.0], of, WHITE);
        }
    }

    /// A category's icon, one of three in a column, at half its size (`MoveCategoryIcon`).
    fn category_icon(&mut self, category: DamageCategory, x: f32, y: f32) {
        let cell = match category {
            DamageCategory::Physical => 0.0,
            DamageCategory::Special => 1.0,
            DamageCategory::Status => 2.0,
        };
        let of = [0.0, cell / 3.0, 1.0, (cell + 1.0) / 3.0];
        self.part("cobblemon:gui/categories", [x, y, 12.0, 8.0], of, WHITE);
    }
}

/// A type's colour over a texture, as `blitk`'s tint.
fn hue(kind: &str) -> u32 {
    let [_, r, g, b] = moves::type_hue(kind).unwrap_or(0xE8E8DA).to_be_bytes();
    rgba(r, g, b, 255)
}

/// A translated text.
fn tr(key: &str) -> Text {
    Text::tr(key, &[])
}

/// A move's name: Cobblemon's translation of its id, or the id itself.
fn move_name(id: &str) -> Text {
    let key = format!("cobblemon.move.{id}");
    let translated = tr(&key);
    if translated.plain() == key {
        Text::literal(id.to_owned())
    } else {
        translated
    }
}

/// The corners of the chart's polygon, clockwise from the top, for `ratios` of the way from the
/// middle to each corner (`StatWidget.drawStatPolygon`): the middle, then the corners.
fn polygon((x, y): (f32, f32), ratios: [f32; 6]) -> ((f32, f32), [(f32, f32); 6]) {
    let (radius, centre) = (48.0, (x + 67.0, y + 22.0 + 48.0));
    let corners = std::array::from_fn(|i| {
        let angle = (-90.0 + 60.0 * i as f32).to_radians();
        // A corner at the middle would leave its triangles without a surface.
        let ratio = ratios[i].clamp(5.0 / radius, 1.0);
        (
            centre.0 + radius * angle.cos() * ratio,
            centre.1 + radius * angle.sin() * ratio,
        )
    });
    (centre, corners)
}

/// `MoveDescriptionScrollList.setMoveDescription`: `text` in lines no wider than `width`, a word that
/// would cross it starting the next line.
fn wrap(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if measure(&format!("{line}{word}")) > width {
            lines.push(std::mem::replace(&mut line, word.to_owned()));
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The Info tab (`InfoWidget`).
fn draw_info(pen: &mut Pen, view: &View) {
    let page = &view.page;
    let (x, y) = view.layout.main();
    pen.image("summary_info_base", [x, y, MAIN.0, MAIN.1]);
    let dex = page
        .dex
        .map_or_else(|| "????".to_owned(), |dex| format!("{dex:04}"));
    let types = match &page.types {
        Some((first, Some(second))) => tr(&format!("cobblemon.type.{first}"))
            .append(Text::literal("/"))
            .append(tr(&format!("cobblemon.type.{second}"))),
        Some((first, None)) => tr(&format!("cobblemon.type.{first}")),
        None => Text::literal(String::new()),
    };
    let nature = tr(&format!("cobblemon.nature.{}", page.nature));
    let rows = [
        ("pokedex_number", Text::literal(dex)),
        ("species", page.species_name.clone()),
        ("type", types),
        ("original_trainer", Text::literal(page.trainer.clone())),
        // `reformatNatureTextIfMinted`: a mint's nature leans.
        ("nature", if page.minted { nature.italic() } else { nature }),
        (
            "ability",
            tr(&format!("cobblemon.ability.{}", page.ability)),
        ),
    ];
    for (index, (label, value)) in rows.into_iter().enumerate() {
        let top = y + 15.0 * index as f32 + 6.0;
        let label = tr(&format!("cobblemon.ui.info.{label}"));
        pen.large(&label, (x + 8.0, top), 43.0, Align::Left, WHITE);
        // The first row's value stops before the size icon.
        let most = if index == 0 { 52.0 } else { 76.0 };
        pen.large(&value, (x + 53.0, top), most, Align::Left, WHITE);
    }
    let size = format!("icon_size_{}", page.size);
    pen.image(&size, [x + 107.5, y + 6.5, 18.5, 8.0]);
    // The ability's description, three lines at most.
    let description = tr(&format!("cobblemon.ability.{}.desc", page.ability));
    let lines = pen.atlas.font.split(&description, 234.0);
    for (index, line) in lines.iter().take(3).enumerate() {
        pen.shadowed(line, (x + 8.0, y + 94.5 + 5.5 * index as f32), Align::Left);
    }
    let label = tr("cobblemon.ui.info.experience_points");
    pen.shadowed(&label, (x + 72.5, y + 125.0), Align::Left);
    let label = tr("cobblemon.ui.info.to_next_level");
    pen.shadowed(&label, (x + 72.5, y + 137.0), Align::Left);
    let experience = Text::literal(page.experience.to_string());
    pen.shadowed(&experience, (x + 127.0, y + 125.0), Align::Right);
    let to_next = Text::literal(page.to_next.to_string());
    pen.shadowed(&to_next, (x + 127.0, y + 137.0), Align::Right);
    let bar = [x + 72.0, y + 131.0, page.experience_ratio * 55.0, 1.0];
    pen.fill(bar, rgba(51, 166, 214, 255));
}

/// The Stats tab's Other view: the friendship and the fullness, one bar under the other
/// (`FriendshipFeatureRenderer`, `FullnessFeatureRenderer`).
fn draw_other(pen: &mut Pen, view: &View) {
    let page = &view.page;
    let (x, y) = view.layout.main();
    let friendship = if page.friendship >= 160 {
        rgba(255, 71, 102, 255)
    } else {
        rgba(255, 143, 163, 255)
    };
    let fullness_ratio = page
        .max_fullness
        .map_or(0.0, |most| page.fullness as f32 / most.max(1) as f32);
    let fullness = match fullness_ratio {
        ratio if ratio <= 0.33 => rgba(120, 200, 80, 255),
        ratio if ratio <= 0.66 => rgba(240, 200, 65, 255),
        _ => rgba(230, 80, 65, 255),
    };
    let friendship_ratio = page.friendship as f32 / 255.0;
    let bars = [
        ("friendship", page.friendship, friendship_ratio, friendship),
        ("fullness", page.fullness, fullness_ratio, fullness),
    ];
    for (index, (name, value, ratio, colour)) in bars.into_iter().enumerate() {
        let (bx, by) = (x + 9.0, y + 15.0 + 29.0 * index as f32);
        let ratio = ratio.min(1.0);
        pen.image("summary_stats_other_bar", [bx, by, 116.0, 24.0]);
        pen.fill([bx + 3.0, by + 13.0, (ratio * 110.0).ceil(), 10.0], colour);
        let overlay = format!("summary_stats_{name}_overlay");
        pen.image(&overlay, [bx + 3.0, by + 13.0, 110.0, 10.0]);
        let label = tr(&format!("cobblemon.ui.stats.{name}"));
        pen.large(&label, (bx + 58.0, by + 2.5), 80.0, Align::Centre, WHITE);
        let value = Text::literal(value.to_string());
        pen.text(&value, (bx + 9.0, by + 6.0), Align::Centre);
        let share = Text::literal(format!("{}%", (ratio * 100.0).floor()));
        pen.text(&share, (bx + 107.0, by + 6.0), Align::Centre);
    }
}

/// The Stats tab (`StatWidget`).
fn draw_stats(pen: &mut Pen, view: &View, cursor: (f32, f32)) {
    let page = &view.page;
    let (x, y) = view.layout.main();
    if view.stat_view == StatView::Other {
        pen.image("summary_stats_other_base", [x, y, MAIN.0, MAIN.1]);
        draw_other(pen, view);
    } else {
        pen.image("summary_stats_chart_base", [x, y, MAIN.0, MAIN.1]);
        pen.image("summary_stats_chart", [x + 25.5, y + 22.0, 83.0, 96.0]);
        let colour = match view.stat_view {
            StatView::Stats => rgba(50, 215, 255, 153),
            StatView::Ivs => rgba(216, 100, 255, 153),
            _ => rgba(255, 255, 100, 153),
        };
        let (centre, corners) = polygon((x, y), page.chart(view.stat_view));
        for index in 0..6 {
            let (a, b) = (corners[index], corners[(index + 1) % 6]);
            let triangle = [[a.0, a.1], [centre.0, centre.1], [b.0, b.1]];
            pen.batch.fill_triangle(pen.atlas, triangle, colour);
        }
        // `statLabelsHovered`: the pointer on any of the labels turns them all to numbers.
        let hovered = CORNERS.iter().any(|&(cx, cy)| {
            (x + cx - 12.0..=x + cx + 12.0).contains(&cursor.0)
                && (y + cy - 1.0..=y + cy + 10.0).contains(&cursor.1)
        });
        let values = page.chart_values(view.stat_view, hovered);
        for (index, (stat, key)) in CHART.into_iter().enumerate() {
            let at = (x + CORNERS[index].0, y + CORNERS[index].1);
            let numbers = hovered
                .then(|| page.chart_label(view.stat_view, stat))
                .flatten();
            let label = match numbers {
                Some(numbers) => Text::literal(numbers),
                None => tr(&format!("cobblemon.ui.stats.{key}")).bold(),
            };
            // `getModifiedStatColour`: what the nature raises is red, what it lowers blue.
            let colour = match view.stat_view {
                StatView::Stats if page.raised == Some(stat) => red(),
                StatView::Stats if page.lowered == Some(stat) => blue(),
                _ => WHITE,
            };
            pen.small(&label, at, Align::Centre, colour, false);
            let value = Text::literal(values[index].clone());
            pen.text(&value, (at.0, at.1 + 5.5), Align::Centre);
        }
        if view.stat_view == StatView::Stats {
            // `renderModifiedStatIcon`: an arrow beside each of the two stats the nature changes.
            let beside = |stat: Stat| match stat {
                Stat::Hp => (65.0, 6.0),
                Stat::SpecialAttack => (10.0, 38.0),
                Stat::Attack => (120.0, 38.0),
                Stat::SpecialDefence => (10.0, 89.0),
                Stat::Defence => (120.0, 89.0),
                Stat::Speed => (65.0, 120.0),
            };
            let arrows = [(page.raised, "increase"), (page.lowered, "decrease")];
            for (stat, way) in arrows {
                if let Some((dx, dy)) = stat.map(beside) {
                    let icon = format!("summary_stats_icon_{way}");
                    pen.image(&icon, [x + dx, y + dy, 4.0, 3.0]);
                }
            }
        }
    }
    // The views' names under the chart, and a marker over the one shown.
    for (index, option) in StatView::ALL.into_iter().enumerate() {
        let shown = option == view.stat_view;
        let colour = if shown { WHITE } else { grey() };
        let at = (x + 31.0 + 24.0 * index as f32, y + 143.0);
        pen.small(&tr(option.key()).bold(), at, Align::Centre, colour, false);
        if shown {
            pen.image(
                "summary_stats_tab_marker",
                [at.0 - 2.0, y + 140.0, 4.0, 2.0],
            );
        }
        widgets::record(option.name(), view.layout.stat_view(index), true);
    }
}

/// The Moves tab (`MovesWidget` and its `MoveSlotWidget`s).
fn draw_moves(pen: &mut Pen, view: &View, cursor: (f32, f32)) {
    let (page, layout) = (&view.page, view.layout);
    let (x, y) = layout.main();
    pen.image("summary_moves_base", [x, y, MAIN.0, MAIN.1]);
    let replaced = match &view.side {
        Side::Learn { replaced } => Some(replaced.as_deref()),
        Side::Party => None,
    };
    for (index, line) in page.moves.iter().enumerate() {
        let slot = layout.move_slot(index);
        let [sx, sy, w, h] = slot;
        if let Some(line) = line {
            if view.selected_move.as_deref() == Some(line.id.as_str()) {
                let around = [sx - 1.0, sy - 1.0, w + 2.0, h + 2.0];
                pen.image("summary_move_selected_overlay", around);
            }
            let of = if over(cursor, slot) { LOWER } else { UPPER };
            pen.part("summary_move", slot, of, hue(&line.kind));
            pen.image("summary_move_overlay", slot);
            pen.image(
                "summary_move_overlay_bar",
                [sx + 60.0, sy + 13.0, 47.0, 8.0],
            );
            // Half its PP or less left is gold, none red.
            let colour = if line.pp == 0 {
                rgba(0xFF, 0x55, 0x55, 255)
            } else if line.pp <= line.max_pp / 2 {
                rgba(0xFF, 0xAA, 0x00, 255)
            } else {
                WHITE
            };
            let left = Text::literal(format!("{}/{}", line.pp, line.max_pp)).bold();
            let at = sx + 93.0 - pen.atlas.font.width_text(&left) / 2.0;
            pen.batch
                .text_styled(pen.atlas, &left, at, sy + 13.0, colour, false);
            pen.type_icon(&line.kind, sx + 2.0, sy + 2.0);
            pen.category_icon(line.category, sx + 66.0, sy + 13.5);
            let name = move_name(&line.id);
            pen.large(&name, (sx + 28.0, sy + 2.0), 78.0, Align::Left, WHITE);
            for (up, way) in [(true, "up"), (false, "down")] {
                let arrow = layout.move_reorder(index, up);
                let texture = format!("summary_move_reorder_{way}");
                pen.button(&texture, arrow, over(cursor, arrow));
                widgets::record(&format!("{way} {}", line.id), arrow, true);
            }
            widgets::record(&name.plain(), slot, true);
        }
        let button = layout.move_swap(index);
        let texture = if line.is_some() {
            "summary_move_swap"
        } else {
            "summary_move_add"
        };
        // Lit while its list is open, as under the pointer.
        let open = replaced == Some(line.as_ref().map(|line| line.id.as_str()));
        pen.button(texture, button, over(cursor, button) || open);
        widgets::record(&format!("switch {index}"), button, true);
    }
    let selected = view
        .selected_move
        .as_deref()
        .and_then(|id| page.move_line(id));
    let rows = [
        ("power", selected.map(|line| power(line.power))),
        ("accuracy", selected.map(|line| percent(line.accuracy))),
        ("effect", selected.map(|line| percent(line.effect))),
    ];
    for (index, (name, value)) in rows.into_iter().enumerate() {
        let top = y + 115.0 + 11.0 * index as f32;
        let icon = format!("summary_moves_icon_{name}");
        pen.image(&icon, [x + 7.0, top - 0.5, 5.0, 5.0]);
        let label = tr(&format!("cobblemon.ui.{name}"));
        pen.shadowed(&label, (x + 14.0, top), Align::Left);
        let value = Text::literal(value.unwrap_or_else(|| "\u{2014}".to_owned()));
        pen.shadowed(&value, (x + 62.5, top), Align::Right);
    }
    if let Some(line) = selected {
        let description = tr(&format!("cobblemon.move.{}.desc", line.id)).plain();
        let [fx, fy, fw, fh] = layout.description();
        let start = pen.batch.vertices.len();
        let lines = wrap(&description, 100.0, |text| pen.atlas.font.width(text));
        for (index, text) in lines.into_iter().enumerate() {
            let top = fy + 4.0 + 5.0 * index as f32 - view.description_scroll;
            pen.shadowed(&Text::literal(text), (x + 73.0, top), Align::Left);
        }
        pen.batch.clip_since(start, fx, fy, fx + fw, fy + fh);
    }
}

/// How far a description of `lines` lines scrolls in its frame.
fn description_scroll_limit(lines: usize) -> f32 {
    (5.0 * lines as f32 - 26.0).max(0.0)
}

/// The Marks tab (`MarksWidget`), without the marks: the Pokémon's name and the grid's thirty slots.
fn draw_marks(pen: &mut Pen, view: &View) {
    let (x, y) = view.layout.main();
    pen.image("summary_marks_base", [x, y, MAIN.0, MAIN.1]);
    pen.text(&view.page.name, (x + 67.0, y + 38.0), Align::Centre);
    for row in 0..5 {
        for column in 0..6 {
            let (sx, sy) = (x + 9.0 + 20.0 * column as f32, y + 52.0 + 19.0 * row as f32);
            pen.button("summary_mark_slot", [sx, sy, 16.0, 16.0], false);
        }
    }
}

/// The list of moves to take, in the party's place (`MoveSwapScreen`).
fn draw_learn(pen: &mut Pen, view: &View, replaced: Option<&str>, cursor: (f32, f32)) {
    let layout = view.layout;
    let frame = layout.list();
    let [x, y, w, h] = frame;
    pen.image("summary_scroll_background", frame);
    let start = pen.batch.vertices.len();
    for (index, row) in view.page.learn_rows(replaced).into_iter().enumerate() {
        let tile = layout.learn_row(index, view.learn_scroll);
        let [rx, ry, rw, rh] = tile;
        if ry + rh < y || ry > y + h {
            continue;
        }
        let lit = over(cursor, frame) && over(cursor, tile);
        let Some(line) = row else {
            // The row that forgets the move.
            pen.button(
                "summary_move_condensed_empty",
                [rx - 9.0, ry, 100.0, rh],
                lit,
            );
            let icon = [rx + (rw / 2.0).floor() - 7.0, ry + 5.0, 7.0, 7.0];
            pen.button("summary_move_condensed_empty_icon", icon, lit);
            widgets::record("forget", tile, true);
            continue;
        };
        let of = if lit { LOWER } else { UPPER };
        pen.part("summary_move_condensed", tile, of, hue(&line.kind));
        pen.image("summary_move_overlay_condensed", tile);
        pen.type_icon(&line.kind, rx - 9.0, ry);
        pen.category_icon(line.category, rx + 77.0, ry + 1.5);
        let name = move_name(&line.id);
        pen.shadowed(&name, (rx + 14.0, ry + 3.5), Align::Left);
        let numbers = [
            ("power", 10.0, 16.5, power(line.power)),
            ("accuracy", 30.0, 37.0, percent(line.accuracy)),
            ("effect", 53.5, 60.5, percent(line.effect)),
        ];
        for (name, icon_x, text_x, value) in numbers {
            let icon = format!("summary_moves_icon_{name}");
            pen.image(&icon, [rx + icon_x, ry + 11.0, 5.0, 5.0]);
            pen.shadowed(&Text::literal(value), (rx + text_x, ry + 12.0), Align::Left);
        }
        let pp = Text::tr("cobblemon.ui.moves.pp", &[&line.max_pp]);
        pen.shadowed(&pp, (rx + 76.0, ry + 12.0), Align::Left);
        widgets::record(&name.plain(), tile, true);
    }
    pen.batch.clip_since(start, x, y, x + w, y + h);
    pen.image("summary_scroll_overlay", [x, y - 3.0, w, h + 6.0]);
    let label = tr("cobblemon.ui.moves.switch");
    pen.large(&label, (x + 32.5, y - 13.5), 62.0, Align::Centre, WHITE);
}

impl Game<'_> {
    fn pokemon_summary(&mut self) -> Option<&mut Summary> {
        Some(&mut self.remote.as_mut()?.pokemon.summary)
    }

    /// A key pressed in play on a server: `SummaryBinding`, M, opens the summary of the Pokémon
    /// selected in the party, once the party has one.
    pub(super) fn pokemon_summary_open_key(&mut self, key: KeyCode) -> bool {
        if key != KeyCode::KeyM {
            return false;
        }
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return false;
        };
        let storage = pokemon.party.storage();
        let party = storage.party();
        // `Summary`'s own choice: the selected slot's Pokémon, else the party's first.
        let selected = storage
            .selected_slot()
            .and_then(|slot| party.get(slot)?.as_ref())
            .or_else(|| party.iter().flatten().next())
            .map(|pokemon| pokemon.uuid);
        // `CobblemonPartyLockedKeyBinding`: without a Pokémon the key does nothing here.
        let Some(selected) = selected else {
            return false;
        };
        pokemon.summary.open(selected);
        self.screen = Screen::PokemonSummary;
        self.keys.clear();
        self.left = false;
        self.right = false;
        self.mining = None;
        self.capture(false);
        true
    }

    /// Closes the screen, telling the server what it kept for then (`Summary.onClose` and its exit
    /// button: the name being typed, the markings).
    pub(in super::super::super) fn close_pokemon_summary(&mut self) {
        self.pokemon_summary_settle();
        if let Some(summary) = self.pokemon_summary() {
            summary.selected = None;
            summary.opened = None;
        }
        self.screen = Screen::Playing;
        self.capture(true);
    }

    /// Whether the name box has the keyboard: it takes every key but Escape then.
    pub(in super::super::super) fn pokemon_summary_typing(&self) -> bool {
        self.screen == Screen::PokemonSummary
            && self
                .remote
                .as_ref()
                .is_some_and(|r| r.pokemon.summary.nickname.is_some())
    }

    /// A key pressed while the screen is open (`Summary.keyPressed`): the summary's own key and the
    /// inventory's close it, unless the name box has the keyboard, which Enter gives back.
    pub(in super::super::super) fn pokemon_summary_key(&mut self, key: KeyCode) -> bool {
        if self.pokemon_summary_typing() {
            match key {
                KeyCode::Enter | KeyCode::NumpadEnter => self.pokemon_summary_nickname_done(),
                KeyCode::Backspace => self.pokemon_summary_backspace(),
                _ => {}
            }
            return true;
        }
        if matches!(key, KeyCode::KeyM | KeyCode::KeyE) {
            self.close_pokemon_summary();
            return true;
        }
        false
    }

    /// Characters typed while the name box has the keyboard.
    pub(in super::super::super) fn pokemon_summary_type(&mut self, text: &str) {
        if let Some(nickname) = self.pokemon_summary().and_then(|s| s.nickname.as_mut()) {
            nickname.typed(text);
        }
    }

    pub(in super::super::super) fn pokemon_summary_backspace(&mut self) {
        if let Some(nickname) = self.pokemon_summary().and_then(|s| s.nickname.as_mut()) {
            nickname.value.pop();
        }
    }

    /// The name box gives the keyboard back: the server is asked for the name, when it changed
    /// (`NicknameEntryWidget.setFocused`).
    fn pokemon_summary_nickname_done(&mut self) {
        let Some(nickname) = self.pokemon_summary().and_then(|s| s.nickname.take()) else {
            return;
        };
        let Some(view) = self.pokemon_summary_view() else {
            return;
        };
        let species = view.page.species_name.plain();
        if let Some(name) = nickname.settled(&species, view.page.nickname.as_deref()) {
            self.pokemon_summary_send(
                party::SET_NICKNAME,
                party::set_nickname(view.page.uuid, true, name.as_deref()),
                &format!("the nickname {name:?}"),
            );
        }
    }

    /// What the screen kept to tell the server once: the name being typed, and the markings
    /// (`saveMarkings`).
    fn pokemon_summary_settle(&mut self) {
        self.pokemon_summary_nickname_done();
        let Some(markings) = self.pokemon_summary().and_then(|s| s.markings.take()) else {
            return;
        };
        let Some(view) = self.pokemon_summary_view() else {
            return;
        };
        if markings != view.page.markings {
            self.pokemon_summary_send(
                party::SET_MARKINGS,
                party::set_markings(view.page.uuid, &markings, true),
                &format!("the markings {markings:?}"),
            );
        }
    }

    /// A message of the screen for the server; a test reads what was asked in the log.
    fn pokemon_summary_send(&self, channel: &str, data: Vec<u8>, what: &str) {
        if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
            log::info!("Horizon: the summary asked the server for {what} ({channel})");
        }
        self.pokemon_send(channel, data);
    }

    /// The facts of the party's species, each read from its file the first time the screen shows it.
    fn pokemon_summary_learn(&mut self) {
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        let wanted: Vec<(String, String)> = pokemon
            .party
            .storage()
            .party()
            .iter()
            .flatten()
            .map(|member| (member.species.clone(), member.form.clone()))
            .filter(|key| !pokemon.summary.facts.contains_key(key))
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Without the resources nothing is kept: they may be there by the next frame.
        let Some(assets) = pokemon.assets.get_or_insert_with(Assets::load).clone() else {
            return;
        };
        for key in wanted {
            let facts = read_facts(&assets, &key.0, &key.1).map(Arc::new);
            if facts.is_none() {
                log::warn!(
                    "Horizon: no species file for {} ({}): its summary lacks its stats",
                    key.0,
                    key.1
                );
            }
            pokemon.summary.facts.insert(key, facts);
        }
    }

    /// The screen stays on its Pokémon wherever it goes in the party; once that one has left, it
    /// shows the party's first.
    fn pokemon_summary_follow(&mut self) {
        let Some(pokemon) = self.remote.as_mut().map(|r| &mut r.pokemon) else {
            return;
        };
        let party = pokemon.party.storage().party();
        let here = |uuid: u128| party.iter().flatten().any(|member| member.uuid == uuid);
        if pokemon.summary.selected.is_some_and(here) {
            return;
        }
        if let Some(first) = party.iter().flatten().next().map(|member| member.uuid) {
            pokemon.summary.select(first);
        }
    }

    /// What the screen shows now; none once the party has no Pokémon.
    fn pokemon_summary_view(&self) -> Option<View> {
        let layout = Layout::of(self.gui_size());
        let pokemon = &self.remote.as_ref()?.pokemon;
        let limits = Limits {
            max_level: pokemon.labels.max_level(),
            sizes: pokemon.labels.sizes(),
        };
        let party = pokemon.party.storage().party();
        let slot = pokemon
            .summary
            .selected
            .and_then(|uuid| {
                party
                    .iter()
                    .position(|slot| slot.as_ref().is_some_and(|p| p.uuid == uuid))
            })
            .or_else(|| party.iter().position(Option::is_some))?;
        let shown = party[slot].as_ref()?;
        let summary = &pokemon.summary;
        let facts = |member: &PartyPokemon| {
            summary
                .facts
                .get(&(member.species.clone(), member.form.clone()))
                .and_then(|facts| facts.as_deref())
        };
        let page = Page::of(shown, slot, facts(shown), pokemon.moves.as_ref(), limits);
        let members = party
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|member| Member::of(member, facts(member)))
            })
            .collect();
        let now = Instant::now();
        Some(View {
            layout,
            markings: summary
                .markings
                .clone()
                .unwrap_or_else(|| page.markings.clone()),
            page,
            members,
            tab: summary.tab,
            stat_view: summary.stat_view,
            side: summary.side.clone(),
            selected_move: summary.selected_move.clone(),
            swap: summary.swap,
            nickname: summary.nickname.as_ref().map(|nickname| {
                // `NicknameEntryWidget`: the cursor shows 300 milliseconds in every 600.
                let since = now.saturating_duration_since(nickname.since).as_millis();
                (nickname.value.clone(), since / 300 % 2 == 0)
            }),
            cosmetic: summary.cosmetic,
            learn_scroll: summary.learn_scroll,
            description_scroll: summary.description_scroll,
            age: summary.opened.map_or(0.0, |opened| {
                now.saturating_duration_since(opened).as_secs_f32() * 20.0
            }),
        })
    }

    /// The left button pressed or let go on the screen.
    pub(in super::super::super) fn pokemon_summary_mouse(&mut self, pressed: bool) {
        let cursor = self.cursor;
        let Some(view) = self.pokemon_summary_view() else {
            return;
        };
        if !pressed {
            // `PartyWidget.mouseReleased`: the dragged Pokémon is let go on another slot.
            let source = self.pokemon_summary().and_then(|s| s.swap.source.take());
            if let (Some(source), Some(target)) = (source, view.slot_at(cursor))
                && source != target
            {
                self.pokemon_summary_swap_slots(&view, source, target);
            }
            return;
        }
        let act = view.hit(cursor);
        // A click anywhere else takes the keyboard from the name box.
        if act != Some(Act::Nickname) && view.nickname.is_some() {
            self.pokemon_summary_nickname_done();
        }
        let Some(act) = act else {
            return;
        };
        if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
            log::info!("Horizon: the summary's {} was clicked", act.name());
        }
        self.pokemon_summary_act(&view, act);
    }

    /// `Summary.swapPartySlot`: the Pokémon of slot `source` goes to `target`, changing places with
    /// the one there.
    fn pokemon_summary_swap_slots(&mut self, view: &View, source: usize, target: usize) {
        let Some(moved) = view.members.get(source).and_then(Option::as_ref) else {
            return;
        };
        self.audio.play_button_click();
        match view.members.get(target).and_then(Option::as_ref) {
            Some(other) => self.pokemon_summary_send(
                party::SWAP_PARTY_POKEMON,
                party::swap_party_pokemon((other.uuid, target), (moved.uuid, source)),
                &format!("slots {source} and {target} to change places"),
            ),
            None => self.pokemon_summary_send(
                party::MOVE_PARTY_POKEMON,
                party::move_party_pokemon(moved.uuid, source, target),
                &format!("slot {source} to go to the empty slot {target}"),
            ),
        }
    }

    fn pokemon_summary_act(&mut self, view: &View, act: Act) {
        let page = &view.page;
        // The party's slots and the markings have their own sounds or none; the rest click.
        if !matches!(act, Act::Slot(_) | Act::Move(_) | Act::Nickname) {
            self.audio.play_button_click();
        }
        match act {
            Act::Tab(tab) => {
                if tab != view.tab
                    && let Some(summary) = self.pokemon_summary()
                {
                    summary.show(tab);
                }
            }
            Act::Exit => {
                if view.side == Side::Party {
                    self.close_pokemon_summary();
                } else if let Some(summary) = self.pokemon_summary() {
                    summary.side = Side::Party;
                }
            }
            Act::Nickname => {
                if let Some(summary) = self.pokemon_summary()
                    && summary.nickname.is_none()
                {
                    summary.nickname = Some(Nickname {
                        value: page.name.plain(),
                        since: Instant::now(),
                    });
                }
            }
            Act::Marking(index) => {
                let mut markings = view.markings.clone();
                if let Some(state) = markings.get_mut(index) {
                    // `MarkingsWidget.incrementState`: off, its first colour, its second.
                    *state = (*state + 1).rem_euclid(3);
                }
                if let Some(summary) = self.pokemon_summary() {
                    summary.markings = Some(markings);
                }
            }
            Act::ItemVisibility => {
                let visible = !page.held_item_visible;
                let uuid = page.uuid;
                if let Some(r) = self.remote.as_mut() {
                    let party = r.pokemon.party.storage_mut();
                    for member in party.party_mut().filter(|member| member.uuid == uuid) {
                        member.held_item_visible = visible;
                    }
                }
                self.pokemon_summary_send(
                    party::SET_ITEM_HIDDEN,
                    party::set_item_hidden(uuid, visible),
                    &format!("the held item to show: {visible}"),
                );
            }
            Act::ItemKind => {
                if let Some(summary) = self.pokemon_summary() {
                    summary.cosmetic = !summary.cosmetic;
                }
            }
            Act::PartySwap => {
                if let Some(summary) = self.pokemon_summary() {
                    summary.swap = Swap {
                        enabled: !summary.swap.enabled,
                        source: None,
                    };
                }
            }
            Act::Slot(index) => {
                let member = view.members.get(index).and_then(Option::as_ref);
                if view.swap.enabled {
                    // `PartyWidget.mouseClicked`: a slot with a Pokémon starts a drag.
                    if member.is_some()
                        && let Some(summary) = self.pokemon_summary()
                    {
                        summary.swap.source = Some(index);
                    }
                } else if let Some(member) = member
                    && member.uuid != page.uuid
                {
                    let uuid = member.uuid;
                    self.audio.play_button_click();
                    // `switchSelection`: the markings of the Pokémon left are told first.
                    self.pokemon_summary_settle();
                    if let Some(summary) = self.pokemon_summary() {
                        summary.select(uuid);
                    }
                }
            }
            Act::StatView(stat_view) => {
                if let Some(summary) = self.pokemon_summary() {
                    summary.stat_view = stat_view;
                }
            }
            Act::Move(index) => {
                let id = page
                    .moves
                    .get(index)
                    .and_then(|line| Some(line.as_ref()?.id.clone()));
                if let Some(summary) = self.pokemon_summary() {
                    summary.select_move(id.as_deref());
                }
            }
            Act::Reorder(index, up) => {
                // `MovesWidget.reorderMove`: with the place above or under, round the tab's places.
                let places = page.moves.len();
                let target = match (up, index) {
                    (true, 0) => places - 1,
                    (true, index) => index - 1,
                    (false, index) if index + 1 >= places => 0,
                    (false, index) => index + 1,
                };
                if let Some(summary) = self.pokemon_summary() {
                    summary.select_move(None);
                }
                self.pokemon_summary_send(
                    party::REQUEST_MOVE_SWAP,
                    party::request_move_swap(index, target, page.slot),
                    &format!(
                        "moves {index} and {target} of slot {} to change places",
                        page.slot
                    ),
                );
            }
            Act::Swap(index) => {
                let replaced = page
                    .moves
                    .get(index)
                    .and_then(|line| Some(line.as_ref()?.id.clone()));
                if let Some(summary) = self.pokemon_summary() {
                    summary.select_move(None);
                    summary.learn_scroll = 0.0;
                    summary.side = match summary.side {
                        Side::Party => Side::Learn { replaced },
                        Side::Learn { .. } => Side::Party,
                    };
                }
            }
            Act::Learn(new) => {
                let Side::Learn { replaced } = &view.side else {
                    return;
                };
                self.pokemon_summary_send(
                    party::BENCH_MOVE,
                    party::bench_move(true, page.uuid, replaced.as_deref(), new.as_deref()),
                    &format!("the move {replaced:?} to give way to {new:?}"),
                );
                if let Some(summary) = self.pokemon_summary() {
                    summary.side = Side::Party;
                }
            }
        }
    }

    /// The wheel turned by `steps` over the screen, up when positive: the list of moves to take and
    /// the selected move's description scroll under the pointer.
    pub(in super::super::super) fn pokemon_summary_scroll(&mut self, steps: f32) {
        let cursor = self.cursor;
        let Some(view) = self.pokemon_summary_view() else {
            return;
        };
        let lines = view
            .selected_move
            .as_deref()
            .filter(|_| view.tab == Tab::Moves)
            .map(|id| {
                let text = Text::tr(&format!("cobblemon.move.{id}.desc"), &[]).plain();
                wrap(&text, 100.0, |text| self.gui.atlas.font.width(text)).len()
            });
        let Some(summary) = self.pokemon_summary() else {
            return;
        };
        if let Side::Learn { replaced } = &view.side
            && over(cursor, view.layout.list())
        {
            // `AbstractSelectionList.mouseScrolled`: half a row a step.
            let limit = learn_scroll_limit(view.page.learn_rows(replaced.as_deref()).len());
            summary.learn_scroll =
                (summary.learn_scroll - steps * LEARN_STEP / 2.0).clamp(0.0, limit);
        } else if let Some(lines) = lines
            && over(cursor, view.layout.description())
        {
            let limit = description_scroll_limit(lines);
            summary.description_scroll =
                (summary.description_scroll - steps * 2.5).clamp(0.0, limit);
        }
    }

    /// The screen (`Summary.render`).
    pub(in super::super::super) fn draw_pokemon_summary(&mut self, batch: &mut HudBatch) {
        widgets::begin_screen(|| {
            widgets::ScreenInfo::new("pokemon_summary", "cobblemon.ui.summary.title")
        });
        self.pokemon_summary_learn();
        self.pokemon_summary_follow();
        let cursor = self.cursor;
        let Some(view) = self.pokemon_summary_view() else {
            // The party is empty, or the server is gone: nothing to show.
            self.screen = Screen::Playing;
            self.capture(true);
            return;
        };
        // The models, asked for before anything is drawn: each is there once its files are read.
        let template = self.res.mob_types().get(super::STAND_IN).cloned();
        let looks: Vec<Option<Look>> = view
            .members
            .iter()
            .map(|member| {
                let member = member.as_ref()?;
                let template = template.as_ref()?;
                self.pokemon_portrait(member.uuid, &member.species, &member.aspects, template)
            })
            .collect();
        let (page, layout) = (&view.page, view.layout);
        let (x, y) = (layout.x, layout.y);
        let pen = &mut Pen {
            batch,
            atlas: &self.gui.atlas,
        };
        let frame = layout.model();
        pen.image("portrait_background", frame);
        pen.image("summary_base", [x, y, BASE.0, BASE.1]);
        if let Some(status) = page.status {
            // The right part of the battle's status tag, with a trim of its own on its left.
            let texture = format!("cobblemon:gui/battle/battle_status_{status}");
            let of = [35.0 / 74.0, 0.0, 1.0, 1.0];
            pen.part(&texture, [x + 34.0, y + 4.0, 39.0, 7.0], of, WHITE);
            pen.image("status_trim", [x + 34.0, y + 5.0, 3.0, 6.0]);
            let label = tr(&format!("cobblemon.ui.status.{status}"));
            pen.large(&label, (x + 39.0, y + 3.0), 32.0, Align::Left, WHITE);
        }
        pen.image(&page.ball, [x + 3.5, y + 15.0, 8.0, 8.0]);
        let sign = match page.gender {
            Gender::Male => Some(("\u{2642}", rgba(0x32, 0xCB, 0xFF, 255))),
            Gender::Female => Some(("\u{2640}", rgba(0xFC, 0x54, 0x54, 255))),
            Gender::Genderless => None,
        };
        if let Some((sign, colour)) = sign {
            let at = (x + 69.0, y + 14.5);
            pen.large(&Text::literal(sign), at, 8.0, Align::Left, colour);
        }
        let at = (x + 6.0, y + 4.5);
        pen.large(&tr("cobblemon.ui.lv"), at, 12.0, Align::Left, WHITE);
        let level = Text::literal(page.level.to_string());
        pen.large(&level, (x + 19.0, y + 4.5), 14.0, Align::Left, WHITE);
        // The name, or what is typed in its place with a cursor that blinks.
        let name = match &view.nickname {
            Some((typed, true)) => Text::literal(format!("{typed}_")),
            Some((typed, false)) => Text::literal(typed.clone()),
            None => page.name.clone(),
        };
        pen.large(&name, (x + 12.0, y + 14.0), 55.0, Align::Left, WHITE);
        widgets::record("nickname", layout.nickname(), true);
        if page.shiny {
            pen.image("icon_shiny", [x + 62.5, y + 33.5, 8.0, 8.0]);
        }
        // The model in its profile pose, as `ModelWidget` stands it: twice `drawProfilePokemon`'s
        // twenty pixels a block, ten pixels above the frame's top, turned to its right.
        if let Some(look) = looks.get(page.slot).and_then(Option::as_ref) {
            use glam::{Mat4, Vec3 as V};
            let (scale, [tx, ty, tz]) = (look.summary_scale, look.summary_translation);
            let top = V::new(frame[0] + frame[2] * 0.5, frame[1] - 10.0, 0.0);
            let gui = Mat4::from_translation(top)
                * Mat4::from_scale(V::new(40.0, 40.0, -40.0))
                * Mat4::from_translation(V::new(tx, ty + 1.5 * scale, tz - 4.0))
                * Mat4::from_scale(V::new(scale, scale, 1.0 / scale))
                * Mat4::from_rotation_x(13f32.to_radians())
                * Mat4::from_rotation_y(325f32.to_radians());
            let clip = [frame[0], frame[1], frame[0] + frame[2], frame[1] + frame[3]];
            self.draw_pokemon_model(pen.batch, look, PoseType::Profile, view.age, gui, clip);
        }
        let double = matches!(&page.types, Some((_, Some(_))));
        let spacer = if double {
            "type_spacer_double"
        } else {
            "type_spacer"
        };
        pen.image(spacer, [x + 5.5, y + 126.0, 67.0, 12.0]);
        // The held item, or the cosmetic one, with what it is beside it.
        let (item, label) = if view.cosmetic {
            (&page.cosmetic_item, "cobblemon.cosmetic_item")
        } else {
            (&page.held_item, "cobblemon.held_item")
        };
        let slot = layout.item();
        if let Some(stack) = item
            .as_ref()
            .and_then(horizon_items::item_nbt::item_from_nbt)
        {
            self.draw_item(pen.batch, &stack, slot[0], slot[1]);
        }
        pen.text(&tr(label), (x + 24.0, y + 114.5), Align::Left);
        // `TypeIcon` centred on x + 39: two types overlap, the second under the first.
        match &page.types {
            Some((first, Some(second))) => {
                pen.type_icon(second, x + 39.0 + 15.0 - 16.5, y + 123.0);
                pen.type_icon(first, x + 39.0 - 16.5, y + 123.0);
            }
            Some((first, None)) => pen.type_icon(first, x + 39.0 - 9.0, y + 123.0),
            None => {}
        }
        pen.image("summary_side_spacer", [x + 217.0, y + 141.0, 72.0, 7.0]);
        match view.tab {
            Tab::Info => draw_info(pen, &view),
            Tab::Moves => draw_moves(pen, &view, cursor),
            Tab::Stats => draw_stats(pen, &view, cursor),
            Tab::Marks => draw_marks(pen, &view),
        }
        match &view.side {
            Side::Party => self.draw_pokemon_summary_party(pen, &view, &looks, cursor),
            Side::Learn { replaced } => draw_learn(pen, &view, replaced.as_deref(), cursor),
        }
        // The markings: each texture holds its three states in rows, at rest and under the pointer.
        for (index, &state) in view.markings.iter().take(6).enumerate() {
            let button = layout.marking(index);
            let column = f32::from(u8::from(over(cursor, button)));
            let row = state.clamp(0, 2) as f32;
            let of = [
                column / 2.0,
                row / 3.0,
                (column + 1.0) / 2.0,
                (row + 1.0) / 3.0,
            ];
            pen.part(&format!("icon_marking_{index}"), button, of, WHITE);
            widgets::record(&format!("marking {index}"), button, true);
        }
        if item.is_some() && !view.cosmetic {
            let texture = if page.held_item_visible {
                "item_visible"
            } else {
                "item_hidden"
            };
            pen.button(texture, slot, over(cursor, slot));
            widgets::record("item visibility", slot, true);
        }
        let button = layout.item_kind();
        let texture = if view.cosmetic {
            "icon_item_held"
        } else {
            "icon_item_cosmetic"
        };
        pen.button(texture, button, over(cursor, button));
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            let button = layout.tab(index);
            if tab == view.tab {
                pen.image("summary_tab", button);
            }
            let icon = format!("summary_tab_icon_{}", tab.name());
            pen.image(&icon, [button[0] + 15.5, button[1] + 3.5, 8.5, 8.5]);
            widgets::record(tab.name(), button, true);
        }
        let button = layout.exit();
        pen.button(
            "cobblemon:gui/common/back_button",
            button,
            over(cursor, button),
        );
        let icon = [button[0] + 7.0, button[1] + 4.0, 10.5, 5.5];
        pen.image("cobblemon:gui/common/back_button_icon", icon);
        widgets::record("back", button, true);
    }

    /// The party beside the tab (`PartyWidget` and its `PartySlotWidget`s).
    fn draw_pokemon_summary_party(
        &self,
        pen: &mut Pen,
        view: &View,
        looks: &[Option<Look>],
        cursor: (f32, f32),
    ) {
        let layout = view.layout;
        let (x, y) = layout.party();
        pen.image("summary_party_background", [x, y, 114.0, 113.0]);
        let label = tr("cobblemon.ui.party");
        pen.large(&label, (x + 32.5, y - 14.5), 62.0, Align::Centre, WHITE);
        let button = layout.party_swap();
        let texture = if view.swap.enabled {
            "summary_party_swap_active"
        } else {
            "summary_party_swap"
        };
        pen.button(texture, button, over(cursor, button));
        pen.image("summary_party_swap_icon", [x + 90.0, y - 6.0, 6.0, 8.5]);
        widgets::record("swap", button, true);
        for index in 0..view.members.len().min(6) {
            let slot = layout.party_slot(index);
            // The Pokémon being dragged has left its slot, and follows the pointer.
            let dragged = view.swap.source == Some(index);
            let member = view.members[index].as_ref().filter(|_| !dragged);
            let selected = member.is_some_and(|member| member.uuid == view.page.uuid);
            let lit = (over(cursor, slot) || selected) && (member.is_some() || view.swap.enabled);
            let look = looks.get(index).and_then(Option::as_ref);
            self.draw_pokemon_summary_slot(pen, slot, member, look, lit);
            let label = member.map_or_else(|| format!("slot {index}"), |m| m.name.plain());
            widgets::record(&label, slot, true);
        }
        if let Some(source) = view.swap.source
            && let Some(member) = view.members.get(source).and_then(Option::as_ref)
        {
            let slot = [cursor.0 - 23.0, cursor.1 - 13.0, SLOT.0, SLOT.1];
            let look = looks.get(source).and_then(Option::as_ref);
            self.draw_pokemon_summary_slot(pen, slot, Some(member), look, false);
        }
    }

    /// One slot of the party (`PartySlotWidget.renderWidget`).
    fn draw_pokemon_summary_slot(
        &self,
        pen: &mut Pen,
        slot: [f32; 4],
        member: Option<&Member>,
        look: Option<&Look>,
        lit: bool,
    ) {
        let [x, y, ..] = slot;
        let texture = match member {
            None => "summary_party_slot_empty",
            Some(member) if member.fainted => "summary_party_slot_fainted",
            Some(_) => "summary_party_slot",
        };
        pen.button(texture, slot, lit);
        let Some(member) = member else {
            return;
        };
        if let Some(out) = member.out {
            pen.image(out, [x + 24.5, y + 3.0, 12.0, 8.5]);
        }
        pen.button(&member.ball, [x - 2.0, y - 3.0, 9.0, 11.0], false);
        if let Some(status) = member.status.filter(|_| !member.fainted) {
            let texture = format!("cobblemon:gui/party/status_{status}");
            pen.image(&texture, [x + 42.0, y + 5.0, 4.0, 14.0]);
        }
        let bar = [x + 4.0, y + 25.0, member.health * 37.0, 1.0];
        pen.fill(bar, health_colour(member.health));
        if let Some(look) = look {
            // `drawProfilePokemon` at 4.5 pixels a block, in a pose stack scaled 2.5 across.
            use glam::{Mat4, Vec3 as V};
            let (scale, [tx, ty, tz]) = (look.profile_scale, look.profile_translation);
            let gui = Mat4::from_translation(V::new(x + 12.5, y - 3.0, 0.0))
                * Mat4::from_scale(V::new(11.25, 11.25, -4.5))
                * Mat4::from_translation(V::new(tx, ty + 1.5 * scale, tz - 4.0))
                * Mat4::from_scale(V::new(scale, scale, 1.0 / scale))
                * Mat4::from_rotation_x(13f32.to_radians())
                * Mat4::from_rotation_y(35f32.to_radians());
            // Cobblemon cuts nothing here; the slot and a little above it, so that a model's depth
            // is tested in a frame of its own.
            let clip = [x - 6.0, y - 10.0, x + SLOT.0, y + SLOT.1];
            self.draw_pokemon_model(pen.batch, look, PoseType::Profile, 0.0, gui, clip);
        }
        pen.text(&member.name, (x + 4.0, y + 20.0), Align::Left);
        let gender = match member.gender {
            Gender::Male => Some("cobblemon:gui/party/party_gender_male"),
            Gender::Female => Some("cobblemon:gui/party/party_gender_female"),
            Gender::Genderless => None,
        };
        if let Some(gender) = gender {
            pen.image(gender, [x + 40.0, y + 20.0, 2.5, 3.5]);
        }
        let level = Text::tr("cobblemon.ui.lv.number", &[&member.level]);
        pen.shadowed(&level, (x + 31.0, y + 13.0), Align::Centre);
        let held = member
            .held_item
            .as_ref()
            .and_then(horizon_items::item_nbt::item_from_nbt);
        if let Some(stack) = held {
            let start = pen.batch.vertices.len();
            self.draw_item_model(pen.batch, &stack, x + 14.0, y + 9.5);
            pen.batch.scale_since(start, x + 14.0, y + 9.5, 0.5, 0.5);
        }
    }

    /// The screen as `horizon-ctl status` tells it, for a test to read: the Pokémon shown with what
    /// each tab says of it, the tab and the side shown, and where each thing that takes a click is.
    /// Null while the screen is closed.
    pub(in super::super::super) fn pokemon_summary_status(&self) -> serde_json::Value {
        if self.screen != Screen::PokemonSummary {
            return serde_json::Value::Null;
        }
        let Some(view) = self.pokemon_summary_view() else {
            return serde_json::Value::Null;
        };
        let page = &view.page;
        let line = |line: &MoveLine| {
            serde_json::json!({
                "id": line.id,
                "type": line.kind,
                "pp": line.pp,
                "max_pp": line.max_pp,
                "power": line.power,
                "accuracy": line.accuracy,
            })
        };
        let targets: Vec<serde_json::Value> = view
            .targets()
            .iter()
            .map(|(rect, act)| {
                serde_json::json!({
                    "act": act.name(),
                    "x": rect[0] + rect[2] / 2.0,
                    "y": rect[1] + rect[3] / 2.0,
                })
            })
            .collect();
        serde_json::json!({
            "tab": view.tab.name(),
            "stat_view": view.stat_view.name(),
            "side": match &view.side {
                Side::Party => "party",
                Side::Learn { .. } => "learn",
            },
            "swap": view.swap.enabled,
            "typing": view.nickname.as_ref().map(|(typed, _)| typed),
            "selected_move": view.selected_move,
            "markings": view.markings,
            "pokemon": {
                "slot": page.slot,
                "species": page.species,
                "name": page.name.plain(),
                "level": page.level,
                "dex": page.dex,
                "types": page.types.as_ref().map(|(first, second)| {
                    std::iter::once(first).chain(second).collect::<Vec<_>>()
                }),
                "trainer": page.trainer,
                "nature": page.nature,
                "minted": page.minted,
                "ability": page.ability,
                "size": page.size,
                "status": page.status,
                "experience": page.experience,
                "to_next": page.to_next,
                "health": page.health,
                // HP, attack, defence, special attack, special defence, speed.
                "stats": page.stats,
                "ivs": Stat::ALL.map(|stat| page.ivs.effective(stat)),
                "evs": page.evs,
                "friendship": page.friendship,
                "fullness": page.fullness,
                "max_fullness": page.max_fullness,
                "held_item_visible": page.held_item_visible,
                "moves": page.moves.iter().map(|m| m.as_ref().map(line)).collect::<Vec<_>>(),
                "learnable": page.learnable.iter().map(line).collect::<Vec<_>>(),
            },
            "party": view.members.iter().map(|m| m.as_ref().map(|m| m.species.clone())).collect::<Vec<_>>(),
            "targets": targets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Charmeleon as a server sends it, with what the screen reads of it.
    const CHARMELEON: &str = r#"{UUID:[I;1,2,3,4],Species:"cobblemon:charmeleon",FormId:"normal",Level:30,
        Experience:22000,Health:71,Gender:"MALE",Shiny:1b,CaughtBall:"cobblemon:ultra_ball",
        ForcedAspects:["male","shiny"],Friendship:200,Fullness:2,ScaleModifier:1.19f,
        IVs:{Base:{"cobblemon:hp":31,"cobblemon:attack":0,"cobblemon:defence":10,
            "cobblemon:special_attack":20,"cobblemon:special_defence":5,"cobblemon:speed":31},
            HyperTrained:{"cobblemon:attack":31}},
        EVs:{"cobblemon:hp":0,"cobblemon:attack":0,"cobblemon:defence":0,"cobblemon:special_attack":252,
            "cobblemon:special_defence":0,"cobblemon:speed":126},
        Nature:"cobblemon:hardy",MintedNature:"cobblemon:modest",
        Ability:{AbilityName:"blaze",AbilityIndex:0},TeraType:"cobblemon:fire",
        MoveSet:[{MoveName:"ember",MovePP:12,RaisedPPStages:1},{MoveName:"slash",MovePP:0,RaisedPPStages:0}],
        BenchedMoves:[{MoveName:"growl",RaisedPPStages:3b}],
        PokemonOriginalTrainer:"c63dc98f-3301-33bd-8ec0-c1a8d3c6f512",
        HeldItem:{id:"cobblemon:charcoal_stick",count:1},Markings:[0,1,2,0,0,0]}"#;

    fn charmeleon(snbt: &str) -> PartyPokemon {
        let (tag, _) = horizon_world::nbt::parse_snbt(snbt).unwrap();
        PartyPokemon::from_tag(tag).unwrap()
    }

    /// Charmeleon's file, cut to what is read, and what its evolution from Charmander taught it.
    fn facts() -> Facts {
        let species = Species::parse(
            r#"{"nationalPokedexNumber": 5, "primaryType": "fire", "weight": 190,
                "experienceGroup": "medium_slow", "preEvolution": "charmander",
                "baseStats": {"hp": 58, "attack": 64, "defence": 58, "special_attack": 80,
                    "special_defence": 65, "speed": 80},
                "moves": ["1:scratch", "1:growl", "1:ember", "12:dragonbreath", "24:slash",
                    "30:flamethrower", "37:scaryface", "tm:cut"]}"#,
            "normal",
        )
        .unwrap();
        Facts {
            species,
            evolution_moves: vec!["firefang".into(), "notamove".into()],
        }
    }

    /// The server's moves, as it sent them (`moves_sync.bin`).
    fn known() -> Moves {
        Moves::read(include_bytes!(
            "../../../cobblelegacy-cobblemon/tests/fixtures/moves_sync.bin"
        ))
        .unwrap()
    }

    const LIMITS: Limits = Limits {
        max_level: 100,
        sizes: Some([0.8, 1.2]),
    };

    fn page() -> Page {
        Page::of(
            &charmeleon(CHARMELEON),
            2,
            Some(&facts()),
            Some(&known()),
            LIMITS,
        )
    }

    /// A view of that page on a 427 by 240 GUI, the size a 1280 by 720 window has at scale 3.
    fn view(page: Page) -> View {
        View {
            layout: Layout::of((427.0, 240.0)),
            members: vec![
                None,
                None,
                Some(Member::of(&charmeleon(CHARMELEON), None)),
                None,
                None,
                None,
            ],
            markings: page.markings.clone(),
            page,
            tab: Tab::Info,
            stat_view: StatView::Stats,
            side: Side::Party,
            selected_move: None,
            swap: Swap::default(),
            nickname: None,
            cosmetic: false,
            learn_scroll: 0.0,
            description_scroll: 0.0,
            age: 0.0,
        }
    }

    #[test]
    fn a_page_says_what_cobblemon_s_summary_would() {
        let page = page();
        assert_eq!((page.slot, page.level, page.shiny), (2, 30, true));
        assert_eq!(page.dex, Some(5));
        assert_eq!(page.types, Some(("fire".into(), None)));
        assert_eq!(page.ball, "cobblemon:item/poke_balls/ultra_ball");
        // No name of its trainer: the start of who it is.
        assert_eq!(page.trainer, "c63dc98f-3301-3...");
        // The mint's nature is the one shown, and the one that counts.
        assert_eq!((page.nature.as_str(), page.minted), ("modest", true));
        assert_eq!(
            (page.raised, page.lowered),
            (Some(Stat::SpecialAttack), Some(Stat::Attack))
        );
        // Attack with the trained IV, a tenth less; special attack with its EVs, a tenth more.
        assert_eq!(page.stats, Some([84, 46, 42, 84, 45, 71]));
        assert_eq!(page.status, None);
        assert_eq!(
            (page.friendship, page.fullness, page.max_fullness),
            (200, 2, Some(5))
        );
        // Medium slow: level 30 starts at 21 760 and spans 2 534.
        assert_eq!(page.to_next, 2534);
        assert!((page.experience_ratio - 240.0 / 2534.0).abs() < 1e-6);
        // The biggest fifth of the server's sizes; an alpha has an icon of its own.
        assert_eq!(page.size, "xl");
        assert!(page.held_item.is_some() && page.held_item_visible);
        assert_eq!(page.markings, [0, 1, 2, 0, 0, 0]);
        // Fainted, its status line says so whatever else it has.
        let fainted = charmeleon(&CHARMELEON.replace("Health:71", "Health:0"));
        let fainted = Page::of(&fainted, 0, None, None, LIMITS);
        assert_eq!(fainted.status, Some("fnt"));
        // Without its species' file: no stats, types or number, and nothing to learn.
        assert_eq!(
            (fainted.stats, fainted.dex, &fainted.types),
            (None, None, &None)
        );
        assert!(fainted.learnable.is_empty());
        assert_eq!(fainted.chart_values(StatView::Stats, false)[0], "?");
    }

    #[test]
    fn the_moves_tab_lists_the_move_set_and_what_may_be_taken() {
        let page = page();
        // Two moves known and something to take: four places, two of them empty.
        let ids: Vec<_> = page
            .moves
            .iter()
            .map(|m| m.as_ref().map(|m| m.id.as_str()))
            .collect();
        assert_eq!(ids, [Some("ember"), Some("slash"), None, None]);
        let ember = page.moves[0].as_ref().unwrap();
        // Ember has 25 PP, 30 with one PP Up.
        assert_eq!(
            (ember.pp, ember.max_pp, ember.kind.as_str()),
            (12, 30, "fire")
        );
        assert_eq!(
            (
                power(ember.power),
                percent(ember.accuracy),
                percent(ember.effect)
            ),
            ("40".into(), "100%".into(), "10%".into())
        );
        // What its levels taught up to 30, what it gave up, what its evolution taught; without what
        // it knows, each once, and nothing the server does not have.
        let ids: Vec<_> = page.learnable.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "scratch",
                "growl",
                "dragonbreath",
                "flamethrower",
                "firefang"
            ]
        );
        // Growl has 40 PP, 64 with the three PP Ups it had when it was given up.
        assert_eq!(page.learnable[1].max_pp, 64);
        // The list has a row to forget the move, when one is replaced.
        assert_eq!(page.learn_rows(Some("ember")).len(), 6);
        assert_eq!(page.learn_rows(None).len(), 5);
        assert!(page.learn_rows(Some("ember"))[5].is_none());
        // A Pokémon with nothing to take shows only the moves it knows.
        let alone = Page::of(&charmeleon(CHARMELEON), 0, None, Some(&known()), LIMITS);
        assert_eq!(alone.moves.len(), 2);
        // A move the server did not tell of is its dummy.
        let unknown = move_facts(Some(&known()), "splashier", 3, 0);
        assert_eq!(
            (unknown.kind.as_str(), unknown.category, unknown.max_pp),
            ("normal", DamageCategory::Status, 0)
        );
    }

    #[test]
    fn the_chart_goes_by_stats_ivs_or_evs() {
        let page = page();
        // Clockwise from the top: HP, attack, defence, speed, special defence, special attack.
        assert_eq!(
            page.chart_values(StatView::Stats, false),
            ["71 / 84", "46", "42", "71", "45", "84"]
        );
        assert_eq!(page.chart(StatView::Stats)[0], 84.0 / 400.0);
        // An IV that was trained shows both, and counts as the trained one.
        assert_eq!(
            page.chart_values(StatView::Ivs, false),
            ["31", "0 (31)", "10", "31", "5", "20"]
        );
        assert_eq!(
            page.chart(StatView::Ivs),
            [1.0, 1.0, 10.0 / 31.0, 1.0, 5.0 / 31.0, 20.0 / 31.0]
        );
        assert_eq!(page.chart_values(StatView::Ivs, true)[2], "32%");
        assert_eq!(
            page.chart_label(StatView::Ivs, Stat::Attack).unwrap(),
            "0 (31)/31"
        );
        assert_eq!(
            page.chart_values(StatView::Evs, false),
            ["0", "0", "0", "126", "0", "252"]
        );
        assert_eq!(page.chart_values(StatView::Evs, true)[3], "50%");
        assert_eq!(
            page.chart_label(StatView::Evs, Stat::Speed).unwrap(),
            "126/252"
        );
        assert_eq!(page.chart_label(StatView::Stats, Stat::Speed), None);
        // The polygon's corners: the top one straight up, none nearer the middle than 5 pixels.
        let (centre, corners) = polygon((100.0, 50.0), [1.0, 0.5, 0.0, 2.0, 1.0, 1.0]);
        assert_eq!(centre, (167.0, 120.0));
        assert!((corners[0].0 - 167.0).abs() < 1e-3 && (corners[0].1 - 72.0).abs() < 1e-3);
        assert!((corners[3].1 - 168.0).abs() < 1e-3);
        let far = |(x, y): (f32, f32)| ((x - 167.0).powi(2) + (y - 120.0).powi(2)).sqrt();
        assert!((far(corners[1]) - 24.0).abs() < 1e-3 && (far(corners[2]) - 5.0).abs() < 1e-3);
    }

    #[test]
    fn numbers_are_written_as_cobblemon_writes_them() {
        assert_eq!(percent(100.0), "100%");
        assert_eq!(percent(12.5), "12.5%");
        assert_eq!(percent(33.333), "33.34%");
        assert_eq!(percent(0.1), "0.1%");
        assert_eq!(percent(30.05), "30.05%");
        assert_eq!(percent(0.0), "\u{2014}");
        assert_eq!(percent(-1.0), "\u{2014}");
        assert_eq!(power(0.0), "\u{2014}");
        assert_eq!(power(65.9), "65");
        assert_eq!(size_category(0.8, [0.8, 1.2]), "xs");
        assert_eq!(size_category(1.0, [0.8, 1.2]), "m");
        assert_eq!(size_category(1.2, [0.8, 1.2]), "xl");
        assert_eq!(size_category(5.0, [0.8, 1.2]), "xl");
        // A server whose Pokémon are all one size.
        assert_eq!(size_category(1.0, [1.0, 1.0]), "xs");
        // The last level spans nothing: an empty bar, not a division by none.
        assert_eq!(
            experience_span(ExperienceGroup::MediumFast, 100, 1_000_000, 100),
            (0, 0.0)
        );
        assert_eq!(
            experience_span(ExperienceGroup::MediumFast, 1, 4, 100),
            (8, 0.5)
        );
        // Words that would cross the width start the next line, measured as Cobblemon does: the
        // line so far and the word with no space between.
        let lines = wrap("aa bb cc dddd e", 5.0, |text| text.len() as f32);
        assert_eq!(lines, ["aa bb", "cc", "dddd", "e"]);
        assert!(wrap("", 5.0, |text| text.len() as f32).is_empty());
    }

    #[test]
    fn a_nickname_is_asked_for_only_when_it_changed() {
        let mut name = Nickname {
            value: "Charmeleon".into(),
            since: Instant::now(),
        };
        // Left as the species' name, with no nickname before: nothing to ask.
        assert_eq!(name.settled("Charmeleon", None), None);
        // Twelve characters at most, and none a chat box refuses.
        name.value.clear();
        name.typed("Flam\u{a7}m\u{7f}\u{1}eche the Great");
        assert_eq!(name.value, "Flammeche th");
        assert_eq!(
            name.settled("Charmeleon", None),
            Some(Some("Flammeche th".into()))
        );
        assert_eq!(name.settled("Charmeleon", Some("Flammeche th")), None);
        // Blank, or the species' own name: no nickname any more.
        name.value = "   ".into();
        assert_eq!(name.settled("Charmeleon", Some("Flam")), Some(None));
        name.value = "Charmeleon".into();
        assert_eq!(name.settled("Charmeleon", Some("Flam")), Some(None));
        name.value = " Braise ".into();
        assert_eq!(
            name.settled("Charmeleon", Some("Flam")),
            Some(Some("Braise".into()))
        );
    }

    #[test]
    fn the_screen_is_laid_out_as_cobblemon_s() {
        // 427 by 240: the base's corner on whole pixels.
        let layout = Layout::of((427.0, 240.0));
        assert_eq!((layout.x, layout.y), (48.0, 39.0));
        assert_eq!(layout.main(), (125.0, 51.0));
        assert_eq!(layout.tab(0), [126.0, 38.0, 39.0, 13.0]);
        assert_eq!(layout.tab(3)[0], 219.0);
        // The party's slots: two a row, the right one 8 pixels lower.
        let (px, py) = layout.party();
        let at = |index| {
            let [x, y, ..] = layout.party_slot(index);
            (x - px, y - py)
        };
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(at),
            [
                (6.0, 7.0),
                (57.0, 15.0),
                (6.0, 39.0),
                (57.0, 47.0),
                (6.0, 71.0),
                (57.0, 79.0)
            ]
        );
        // The list's rows: 91 wide in a frame of 108, 21 apart, moved by the scroll.
        assert_eq!(
            layout.learn_row(0, 0.0),
            [264.0 + 11.0, 62.0 + 4.0, 91.0, 18.0]
        );
        assert_eq!(layout.learn_row(2, 10.5)[1], 62.0 + 4.0 + 42.0 - 10.5);
        assert_eq!(learn_scroll_limit(5), 0.0);
        assert_eq!(learn_scroll_limit(6), 18.0);
        assert_eq!(description_scroll_limit(3), 0.0);
        assert_eq!(description_scroll_limit(8), 14.0);
        // An odd size rounds as integer halves do.
        assert_eq!(Layout::of((480.0, 270.0)), Layout { x: 74.0, y: 54.0 });
    }

    #[test]
    fn a_click_does_what_is_under_it() {
        let mut view = view(page());
        let layout = view.layout;
        let middle = |[x, y, w, h]: [f32; 4]| (x + w / 2.0, y + h / 2.0);
        assert_eq!(view.hit(middle(layout.tab(2))), Some(Act::Tab(Tab::Stats)));
        assert_eq!(view.hit(middle(layout.exit())), Some(Act::Exit));
        assert_eq!(view.hit(middle(layout.nickname())), Some(Act::Nickname));
        assert_eq!(view.hit(middle(layout.marking(5))), Some(Act::Marking(5)));
        assert_eq!(view.hit(middle(layout.item())), Some(Act::ItemVisibility));
        assert_eq!(view.hit(middle(layout.party_slot(2))), Some(Act::Slot(2)));
        assert_eq!(view.slot_at(middle(layout.party_slot(4))), Some(4));
        assert_eq!(view.hit(middle(layout.party_swap())), Some(Act::PartySwap));
        // The Info tab has nothing to click; the Stats tab its views; the Moves tab its moves.
        assert_eq!(view.hit(middle(layout.stat_view(1))), None);
        assert_eq!(view.hit(middle(layout.move_slot(0))), None);
        view.tab = Tab::Stats;
        assert_eq!(
            view.hit(middle(layout.stat_view(1))),
            Some(Act::StatView(StatView::Ivs))
        );
        assert_eq!(
            view.hit(middle(layout.stat_view(3))),
            Some(Act::StatView(StatView::Other))
        );
        view.tab = Tab::Moves;
        assert_eq!(view.hit(middle(layout.move_slot(1))), Some(Act::Move(1)));
        assert_eq!(
            view.hit(middle(layout.move_reorder(1, true))),
            Some(Act::Reorder(1, true))
        );
        assert_eq!(
            view.hit(middle(layout.move_reorder(0, false))),
            Some(Act::Reorder(0, false))
        );
        assert_eq!(view.hit(middle(layout.move_swap(0))), Some(Act::Swap(0)));
        // An empty place has only the button that fills it.
        assert_eq!(view.hit(middle(layout.move_slot(3))), None);
        assert_eq!(view.hit(middle(layout.move_swap(3))), Some(Act::Swap(3)));
        // With the list open the party's slots are not there, and its rows are.
        view.side = Side::Learn {
            replaced: Some("ember".into()),
        };
        assert_eq!(
            view.hit(middle(layout.learn_row(0, 0.0))),
            Some(Act::Learn(Some("scratch".into())))
        );
        // Scrolled to its end, the last row forgets the move; a row out of the frame takes nothing.
        view.learn_scroll = learn_scroll_limit(6);
        assert_eq!(
            view.hit(middle(layout.learn_row(5, view.learn_scroll))),
            Some(Act::Learn(None))
        );
        view.learn_scroll = 0.0;
        let below = layout.learn_row(5, 0.0);
        assert_eq!(view.hit((below[0] + 5.0, below[1] + 16.0)), None);
        // Without a held item its slot is no button.
        view.page.held_item = None;
        assert_eq!(view.hit(middle(layout.item())), None);
        // Every target has a name of its own.
        let mut names: Vec<String> = view.targets().iter().map(|(_, act)| act.name()).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_screen_s_state_follows_what_is_shown() {
        let mut summary = Summary::default();
        summary.open(7);
        assert_eq!((summary.selected, summary.tab), (Some(7), Tab::Info));
        // A move's description shows on a click, and goes on a second.
        summary.show(Tab::Moves);
        summary.select_move(Some("ember"));
        assert_eq!(summary.selected_move.as_deref(), Some("ember"));
        summary.select_move(Some("slash"));
        assert_eq!(summary.selected_move.as_deref(), Some("slash"));
        summary.select_move(Some("slash"));
        assert_eq!(summary.selected_move, None);
        // Another Pokémon keeps the tab and the Stats tab's view, and forgets what was the first's.
        summary.stat_view = StatView::Evs;
        summary.side = Side::Learn { replaced: None };
        summary.markings = Some(vec![1; 6]);
        summary.select_move(Some("ember"));
        summary.select(9);
        assert_eq!((summary.selected, summary.tab), (Some(9), Tab::Moves));
        assert_eq!(summary.stat_view, StatView::Evs);
        assert_eq!(
            (summary.side.clone(), summary.selected_move.clone()),
            (Side::Party, None)
        );
        assert!(summary.markings.is_none());
        // Opened again, it is a new screen.
        summary.open(9);
        assert_eq!(
            (summary.tab, summary.stat_view),
            (Tab::Info, StatView::Stats)
        );
    }

    #[test]
    fn a_slot_of_the_party_shows_its_pokemon() {
        let pokemon = charmeleon(CHARMELEON);
        let member = Member::of(&pokemon, Some(&facts()));
        assert_eq!(member.ball, "cobblemon:gui/ball/ultra_ball");
        assert_eq!(member.level, 30);
        // 71 of the 84 its stats give it.
        assert!((member.health - 71.0 / 84.0).abs() < 1e-6);
        assert!(!member.fainted && member.out.is_none() && member.held_item.is_some());
        // Without its species' file: a full bar while it stands.
        assert_eq!(Member::of(&pokemon, None).health, 1.0);
    }
}
