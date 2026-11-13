//! A screen an external host draws (CobbleLegacy's fork).
//!
//! The network's mods draw their screens with a toolkit no Rust code replaces. A host, another process
//! without a window, composes such a screen into an image; this file shows that image as a screen of the
//! game, sends the host the mouse, the wheel, the keys and the text typed, and carries messages between
//! the host and the server. Nothing here knows what the host draws.
//!
//! `HORIZON_SCREEN_HOST` names a file with the host's command line, an argument a line. The host is
//! started when the player is on a server and talks over its standard input and output ([`wire`]). Its
//! pixels come through a file both sides hold open: the host writes what changed of its image there,
//! says which rectangle, and waits for the game to say it has read it before it writes again. So an
//! image costs one copy of what changed, and nothing while the screen stands still.
//!
//! The image goes into a slot of the HUD atlas (`hud::HOSTED_SCREEN_SPRITE`), which bounds its size: a
//! larger window is drawn at a whole fraction of its size and stretched.
//!
//! A Fabric server sends a mod's messages only on the channels the client said it takes, so whoever
//! launches the game names the host's in `HORIZON_ACCEPT_CHANNELS`. Such a server speaks as soon as the
//! player is in, before the host is started: what comes on those channels until then is kept and
//! handed to the host first.
//!
//! A host that dies or hangs takes its screen with it and nothing else: the game never waits for it.
use super::super::super::{Game, Screen};
use horizon_client::{
    hud::{HOSTED_SCREEN_SIZE, HOSTED_SCREEN_SPRITE, HudBatch},
    items::ItemStack,
    text::Text,
    widgets,
};
use std::{
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        OnceLock, RwLock,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};
use winit::{event::MouseButton, keyboard::KeyCode};

/// The variable that names the host's launch file.
const LAUNCH: &str = "HORIZON_SCREEN_HOST";

/// The variable the host is started with: the game's own resources (the vanilla language files and
/// textures), which a screen names its items and effects from.
const RESOURCES: &str = "HORIZON_RESOURCES";

/// The channels the running host reads, which [`reads`] answers from: `remote.rs` asks before it has
/// the game at hand. `None` until a host has said which.
static CHANNELS: RwLock<Option<Vec<String>>> = RwLock::new(None);

/// The channels a Fabric server is told the client takes for the host: what `HORIZON_ACCEPT_CHANNELS`
/// adds to those the client always names, which whoever launches the game fills with what the host's
/// mods read. None without a host.
///
/// A server sends on them from the moment the player is in (a mod's state, pushed on join), which is
/// before the host is started: until the host says what it reads, they are all that is known of it.
fn announced() -> &'static [String] {
    static ANNOUNCED: OnceLock<Vec<String>> = OnceLock::new();
    ANNOUNCED.get_or_init(|| {
        if std::env::var_os(LAUNCH).is_none() {
            return Vec::new();
        }
        let added = std::env::var(horizon_client::modded_server::ACCEPTED).ok();
        let always = horizon_client::modded_server::accepted_channels(None).len();
        horizon_client::modded_server::accepted_channels(added.as_deref())
            .into_iter()
            .skip(always)
            .collect()
    })
}

/// Whether a message on `channel` is the host's: on a channel it said it reads (`known`), or, before
/// it said anything, on one the server was told is taken for it.
fn is_read(known: Option<&[String]>, announced: &[String], channel: &str) -> bool {
    known.unwrap_or(announced).iter().any(|c| c == channel)
}

/// Whether the host reads the server's messages on `channel`.
pub(super) fn reads(channel: &str) -> bool {
    CHANNELS
        .read()
        .is_ok_and(|known| is_read(known.as_deref(), announced(), channel))
}

/// The mean and the largest of `values`, and how many there are. `None` without any.
fn mean_max(values: impl Iterator<Item = f32>) -> Option<(f32, f32, usize)> {
    let values: Vec<f32> = values.collect();
    let count = values.len();
    (count > 0).then(|| {
        (
            values.iter().sum::<f32>() / count as f32,
            values.iter().copied().fold(0.0, f32::max),
            count,
        )
    })
}

/// The messages between the game and the host: a length (`u32`, of what follows), a type (`u8`), then
/// the fields in order. Numbers are big-endian; a string or a block of bytes is its length (`u32`) and
/// its bytes.
pub(super) mod wire {
    /// The longest message taken from the host.
    pub const LONGEST: usize = 1 << 22;

    /// How the pointer moved or what its button did.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Pointer {
        Move = 0,
        Press = 1,
        Release = 2,
    }

    /// What the game tells the host.
    #[derive(Debug, PartialEq)]
    pub enum ToHost<'a> {
        /// The size of the image to draw, in pixels, and how many of them a GUI pixel is.
        Viewport {
            width: u32,
            height: u32,
            scale: f32,
            language: &'a str,
            player: &'a str,
        },
        /// The player put the screen away.
        Closed,
        Pointer {
            serial: u32,
            kind: Pointer,
            x: f32,
            y: f32,
            button: u8,
            modifiers: u8,
        },
        /// The wheel, in notches: `dy` positive scrolls up.
        Scroll {
            serial: u32,
            x: f32,
            y: f32,
            dx: f32,
            dy: f32,
        },
        /// A key by its GLFW number, as the mods' own code reads keys.
        Key {
            serial: u32,
            down: bool,
            key: i32,
            modifiers: u8,
        },
        Text {
            serial: u32,
            text: &'a str,
        },
        /// A message of the server on a channel the host reads.
        Payload {
            channel: &'a str,
            data: &'a [u8],
        },
        /// The image `frame` is read: the host may write the next.
        Ack {
            frame: u32,
        },
        /// The whole image again: what the game held of it is gone.
        Redraw,
        /// Something the game knows and a screen may ask for (the player's party, where the player
        /// stands), as JSON, each time it changes.
        State {
            key: &'a str,
            json: &'a str,
        },
    }

    impl ToHost<'_> {
        /// The message as it goes to the host, its length first.
        pub fn bytes(&self) -> Vec<u8> {
            let mut out = vec![0; 4];
            let text = |out: &mut Vec<u8>, text: &[u8]| {
                out.extend_from_slice(&(text.len() as u32).to_be_bytes());
                out.extend_from_slice(text);
            };
            match self {
                Self::Viewport {
                    width,
                    height,
                    scale,
                    language,
                    player,
                } => {
                    out.push(1);
                    out.extend_from_slice(&width.to_be_bytes());
                    out.extend_from_slice(&height.to_be_bytes());
                    out.extend_from_slice(&scale.to_be_bytes());
                    text(&mut out, language.as_bytes());
                    text(&mut out, player.as_bytes());
                }
                Self::Closed => out.push(2),
                Self::Pointer {
                    serial,
                    kind,
                    x,
                    y,
                    button,
                    modifiers,
                } => {
                    out.push(3);
                    out.extend_from_slice(&serial.to_be_bytes());
                    out.push(*kind as u8);
                    out.extend_from_slice(&x.to_be_bytes());
                    out.extend_from_slice(&y.to_be_bytes());
                    out.push(*button);
                    out.push(*modifiers);
                }
                Self::Scroll {
                    serial,
                    x,
                    y,
                    dx,
                    dy,
                } => {
                    out.push(4);
                    out.extend_from_slice(&serial.to_be_bytes());
                    for value in [x, y, dx, dy] {
                        out.extend_from_slice(&value.to_be_bytes());
                    }
                }
                Self::Key {
                    serial,
                    down,
                    key,
                    modifiers,
                } => {
                    out.push(5);
                    out.extend_from_slice(&serial.to_be_bytes());
                    out.push(u8::from(*down));
                    out.extend_from_slice(&key.to_be_bytes());
                    out.push(*modifiers);
                }
                Self::Text {
                    serial,
                    text: typed,
                } => {
                    out.push(6);
                    out.extend_from_slice(&serial.to_be_bytes());
                    text(&mut out, typed.as_bytes());
                }
                Self::Payload { channel, data } => {
                    out.push(7);
                    text(&mut out, channel.as_bytes());
                    text(&mut out, data);
                }
                Self::Ack { frame } => {
                    out.push(8);
                    out.extend_from_slice(&frame.to_be_bytes());
                }
                Self::Redraw => out.push(9),
                Self::State { key, json } => {
                    out.push(10);
                    text(&mut out, key.as_bytes());
                    text(&mut out, json.as_bytes());
                }
            }
            let length = (out.len() - 4) as u32;
            out[..4].copy_from_slice(&length.to_be_bytes());
            out
        }
    }

    /// The part of the host's image that changed, written in the shared file at its own place: row
    /// `y` of an image `width` wide starts at byte `y * width * 4`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Frame {
        pub number: u32,
        /// The whole image's size.
        pub width: u32,
        pub height: u32,
        /// The rectangle that changed.
        pub x: u32,
        pub y: u32,
        pub w: u32,
        pub h: u32,
        /// The last input the host had taken when it drew this.
        pub serial: u32,
        /// What drawing it took the host, in microseconds.
        pub micros: u32,
        /// The CRC-32 of the rectangle's bytes, 0 when the host does not check.
        pub crc: u32,
    }

    /// What the host tells the game.
    #[derive(Debug, PartialEq)]
    pub enum FromHost {
        /// The host is up: how long that took it, and the channels it reads.
        Ready {
            startup_ms: u32,
            channels: Vec<String>,
        },
        /// A screen opens (a mod's `setScreen`).
        Show {
            name: String,
            /// The screen reads Escape itself (`Screen.shouldCloseOnEsc` is false, or its
            /// `keyPressed` is its own): the key goes to the host as the others do, and the host says
            /// when the screen closes.
            keeps_escape: bool,
        },
        /// The screen closes by itself.
        Hide,
        Frame(Frame),
        /// The inputs up to `serial` are taken and changed nothing of the image.
        Seen(u32),
        /// A message for the server.
        Send {
            channel: String,
            data: Vec<u8>,
        },
        /// A command for the server, without its slash.
        Command(String),
        Sound {
            id: String,
            volume: f32,
            pitch: f32,
        },
        Log(String),
        /// What the game draws itself over the image: the whole list, each time it changes.
        Overlays(Vec<Overlay>),
        /// A line for the player's chat, as a mod writes one (`player.sendMessage`), legacy colour
        /// codes and all.
        Chat(String),
    }

    /// A thing only the game can draw (an item), which the host's screen lays out: the host leaves its
    /// rectangle empty and the game fills it over the image. `rect` and `clip` are x, y, width and
    /// height in the image's pixels; nothing of it shows outside `clip`.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Overlay {
        /// 1: an item, `what` its id. 2 and 3: a Pokémon's model ([`super::model_of`]). 4: the face of
        /// the player `what` names.
        pub kind: u8,
        pub rect: [f32; 4],
        pub clip: [f32; 4],
        pub what: String,
    }

    struct Reader<'a>(&'a [u8]);

    impl<'a> Reader<'a> {
        fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
            if self.0.len() < count {
                return Err("a message of the host ends early".into());
            }
            let (head, rest) = self.0.split_at(count);
            self.0 = rest;
            Ok(head)
        }
        fn u32(&mut self) -> Result<u32, String> {
            let bytes = self.take(4)?;
            Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        fn f32(&mut self) -> Result<f32, String> {
            Ok(f32::from_bits(self.u32()?))
        }
        fn bytes(&mut self) -> Result<&'a [u8], String> {
            let length = self.u32()? as usize;
            self.take(length)
        }
        fn string(&mut self) -> Result<String, String> {
            String::from_utf8(self.bytes()?.to_vec()).map_err(|e| e.to_string())
        }
    }

    /// A message of the host, without its length.
    pub fn read(message: &[u8]) -> Result<FromHost, String> {
        let (kind, body) = message.split_first().ok_or("an empty message")?;
        let r = &mut Reader(body);
        Ok(match kind {
            1 => {
                let startup_ms = r.u32()?;
                let count = r.u32()?;
                let mut channels = Vec::new();
                for _ in 0..count.min(4096) {
                    channels.push(r.string()?);
                }
                FromHost::Ready {
                    startup_ms,
                    channels,
                }
            }
            2 => {
                let name = r.string()?;
                // Flags follow the name; a host that says nothing more leaves Escape to the game.
                let flags = r.0.first().copied().unwrap_or(0);
                FromHost::Show {
                    name,
                    keeps_escape: flags & 1 != 0,
                }
            }
            3 => FromHost::Hide,
            4 => FromHost::Frame(Frame {
                number: r.u32()?,
                width: r.u32()?,
                height: r.u32()?,
                x: r.u32()?,
                y: r.u32()?,
                w: r.u32()?,
                h: r.u32()?,
                serial: r.u32()?,
                micros: r.u32()?,
                crc: r.u32()?,
            }),
            5 => FromHost::Seen(r.u32()?),
            6 => FromHost::Send {
                channel: r.string()?,
                data: r.bytes()?.to_vec(),
            },
            7 => FromHost::Command(r.string()?),
            8 => FromHost::Sound {
                id: r.string()?,
                volume: r.f32()?,
                pitch: r.f32()?,
            },
            9 => FromHost::Log(r.string()?),
            10 => {
                let count = r.u32()?;
                let mut overlays = Vec::new();
                for _ in 0..count.min(4096) {
                    let kind = r.take(1)?[0];
                    let rect = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                    let clip = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                    overlays.push(Overlay {
                        kind,
                        rect,
                        clip,
                        what: r.string()?,
                    });
                }
                FromHost::Overlays(overlays)
            }
            11 => FromHost::Chat(r.string()?),
            other => return Err(format!("an unknown message of the host: {other}")),
        })
    }
}

use wire::{Frame, FromHost, ToHost};

/// The size the host draws at for a window of `window` pixels at GUI scale `scale`: the window's own
/// while it fits the atlas's slot, else the largest whole fraction of it that does. With it, how many
/// window pixels one of the image's is.
fn image_size(window: [u32; 2], scale: f32) -> ([u32; 2], f32, u32) {
    let (slot_w, slot_h) = HOSTED_SCREEN_SIZE;
    let mut divisor = 1;
    while window[0].div_ceil(divisor) > slot_w || window[1].div_ceil(divisor) > slot_h {
        divisor += 1;
    }
    (
        [(window[0] / divisor).max(1), (window[1] / divisor).max(1)],
        scale / divisor as f32,
        divisor,
    )
}

/// Whether `frame` names a rectangle inside an image that fits the slot and the shared file.
fn frame_fits(frame: &Frame) -> bool {
    let (slot_w, slot_h) = HOSTED_SCREEN_SIZE;
    frame.width <= slot_w
        && frame.height <= slot_h
        && frame.w > 0
        && frame.h > 0
        && frame
            .x
            .checked_add(frame.w)
            .is_some_and(|r| r <= frame.width)
        && frame
            .y
            .checked_add(frame.h)
            .is_some_and(|b| b <= frame.height)
}

/// What an overlay of a Pokémon's model names: `species;aspect,aspect;silhouette`, the last part empty
/// when the model shows as it is.
fn model_of(what: &str) -> (&str, Vec<String>, bool) {
    let mut parts = what.split(';');
    let species = parts.next().unwrap_or_default();
    let aspects = parts
        .next()
        .unwrap_or_default()
        .split(',')
        .filter(|aspect| !aspect.is_empty())
        .map(str::to_owned)
        .collect();
    (species, aspects, parts.next() == Some("silhouette"))
}

/// How long the host may leave an input unanswered before Escape stops being its own.
const HUNG: Duration = Duration::from_secs(3);

/// Whether Escape goes to the host instead of putting its screen away: on a screen that keeps the
/// key, while the host answers. `unanswered` is how long its oldest input has waited: a host that
/// hangs does not hold the player in its screen.
fn escape_is_the_host_s(keeps_escape: bool, unanswered: Option<Duration>) -> bool {
    keeps_escape && unanswered.is_none_or(|waited| waited < HUNG)
}

/// The players of the list as a screen's mod may ask for them (`getPlayerListEntry`): their names
/// and ids, as JSON.
fn players_json(players: &[(u128, &str)]) -> String {
    let players: Vec<serde_json::Value> = players
        .iter()
        .map(|(uuid, name)| {
            let hex = format!("{uuid:032x}");
            let uuid = [
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..],
            ]
            .join("-");
            serde_json::json!({ "name": name, "uuid": uuid })
        })
        .collect();
    serde_json::json!({ "players": players }).to_string()
}

/// The bytes the shared file holds for the slot's largest image.
fn file_size() -> u64 {
    u64::from(HOSTED_SCREEN_SIZE.0) * u64::from(HOSTED_SCREEN_SIZE.1) * 4
}

/// The GLFW number of a key (`GLFW_KEY_*`), which a mod's key handlers compare against.
fn glfw_key(key: KeyCode) -> Option<i32> {
    use KeyCode as K;
    let letters = [
        K::KeyA,
        K::KeyB,
        K::KeyC,
        K::KeyD,
        K::KeyE,
        K::KeyF,
        K::KeyG,
        K::KeyH,
        K::KeyI,
        K::KeyJ,
        K::KeyK,
        K::KeyL,
        K::KeyM,
        K::KeyN,
        K::KeyO,
        K::KeyP,
        K::KeyQ,
        K::KeyR,
        K::KeyS,
        K::KeyT,
        K::KeyU,
        K::KeyV,
        K::KeyW,
        K::KeyX,
        K::KeyY,
        K::KeyZ,
    ];
    if let Some(at) = letters.iter().position(|k| *k == key) {
        return Some(65 + at as i32);
    }
    let digits = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    if let Some(at) = digits.iter().position(|k| *k == key) {
        return Some(48 + at as i32);
    }
    let functions = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    if let Some(at) = functions.iter().position(|k| *k == key) {
        return Some(290 + at as i32);
    }
    Some(match key {
        K::Space => 32,
        K::Quote => 39,
        K::Comma => 44,
        K::Minus => 45,
        K::Period => 46,
        K::Slash => 47,
        K::Semicolon => 59,
        K::Equal => 61,
        K::BracketLeft => 91,
        K::Backslash => 92,
        K::BracketRight => 93,
        K::Backquote => 96,
        K::Escape => 256,
        K::Enter => 257,
        K::Tab => 258,
        K::Backspace => 259,
        K::Insert => 260,
        K::Delete => 261,
        K::ArrowRight => 262,
        K::ArrowLeft => 263,
        K::ArrowDown => 264,
        K::ArrowUp => 265,
        K::PageUp => 266,
        K::PageDown => 267,
        K::Home => 268,
        K::End => 269,
        K::CapsLock => 280,
        K::NumpadEnter => 335,
        K::ShiftLeft => 340,
        K::ControlLeft => 341,
        K::AltLeft => 342,
        K::SuperLeft => 343,
        K::ShiftRight => 344,
        K::ControlRight => 345,
        K::AltRight => 346,
        K::SuperRight => 347,
        _ => return None,
    })
}

/// The running host.
struct Link {
    child: Child,
    /// To the thread that writes the host's input: the game never waits on the host.
    to_host: Sender<Vec<u8>>,
    /// From the thread that reads the host's output. It closes when the host is gone.
    from_host: Receiver<FromHost>,
    /// The file the host writes its pixels in, and where it is.
    pixels: File,
    path: PathBuf,
    started: Instant,
}

impl Link {
    /// Starts the host the launch file names: its program, then its arguments, a line each. The host
    /// is told the shared file and its size after them, and where the game's `resources` are.
    fn start(launch: &Path, resources: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(launch).map_err(|e| e.to_string())?;
        let mut words = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        let program = words.next().ok_or("an empty launch file")?;
        let path =
            std::env::temp_dir().join(format!("horizon-screen-host-{}.rgba", std::process::id()));
        let pixels = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        pixels.set_len(file_size()).map_err(|e| e.to_string())?;
        let mut command = Command::new(program);
        command
            .args(words)
            .arg("--pixels")
            .arg(&path)
            .arg("--size")
            .arg(file_size().to_string())
            .env(RESOURCES, resources)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: a console program started by a windowed one opens a console otherwise.
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let started = Instant::now();
        let mut child = command.spawn().map_err(|e| format!("{program}: {e}"))?;
        let (Some(input), Some(mut output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("the host has no pipes".into());
        };
        let (to_host, queue) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("screen host out".into())
            .spawn(move || {
                let mut input = BufWriter::new(input);
                for message in queue {
                    if input.write_all(&message).is_err() || input.flush().is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (messages, from_host) = mpsc::channel();
        std::thread::Builder::new()
            .name("screen host in".into())
            .spawn(move || {
                loop {
                    let mut head = [0; 4];
                    if output.read_exact(&mut head).is_err() {
                        break;
                    }
                    let length = u32::from_be_bytes(head) as usize;
                    if length == 0 || length > wire::LONGEST {
                        log::warn!("Horizon: the screen host sent a message {length} bytes long");
                        break;
                    }
                    let mut body = vec![0; length];
                    if output.read_exact(&mut body).is_err() {
                        break;
                    }
                    match wire::read(&body) {
                        Ok(message) => {
                            if messages.send(message).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            log::warn!("Horizon: the screen host: {e}");
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            child,
            to_host,
            from_host,
            pixels,
            path,
            started,
        })
    }

    fn tell(&self, message: &ToHost) {
        // A host that is gone is found out by the reader: nothing to do here.
        let _ = self.to_host.send(message.bytes());
    }

    /// The pixels of `frame`'s rectangle, read from the shared file.
    fn read(&self, frame: &Frame) -> std::io::Result<Vec<u8>> {
        let row = frame.width as usize * 4;
        let mut band = vec![0; row * frame.h as usize];
        let mut file = &self.pixels;
        file.seek(SeekFrom::Start(u64::from(frame.y) * row as u64))?;
        file.read_exact(&mut band)?;
        if frame.w == frame.width {
            return Ok(band);
        }
        let (from, wide) = (frame.x as usize * 4, frame.w as usize * 4);
        let mut rgba = Vec::with_capacity(wide * frame.h as usize);
        for line in band.chunks_exact(row) {
            rgba.extend_from_slice(&line[from..from + wide]);
        }
        Ok(rgba)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
        // The next host, on the next server, has not said what it reads.
        if let Ok(mut channels) = CHANNELS.write() {
            *channels = None;
        }
    }
}

/// What a test reads of the link (`horizon-ctl host`).
#[derive(Default)]
struct Stats {
    /// From the host's start to its first word, as the game saw it, and as the host timed itself.
    ready_ms: Option<f32>,
    startup_ms: Option<u32>,
    /// Images taken, and the bytes copied for them.
    frames: u32,
    bytes: u64,
    /// The inputs sent and not answered yet: each one's serial and when it left.
    waiting: VecDeque<(u32, Instant)>,
    /// From an input to the image that follows it, in milliseconds: the last ones.
    latencies: VecDeque<f32>,
    /// The inputs that changed nothing of the image.
    unchanged: u32,
    /// What the host says drawing took it, in microseconds: the last ones.
    host_micros: VecDeque<u32>,
    /// When the server's last message went to the host, and from then to a new screen's first image.
    payload_at: Option<Instant>,
    open_ms: Option<f32>,
    /// Images whose bytes did not match the host's sum.
    corrupt: u32,
    checked: u32,
}

impl Stats {
    const KEPT: usize = 512;

    /// The host has answered the inputs up to `serial`, with an image or without.
    fn answered(&mut self, serial: u32, image: bool) {
        let mut oldest = None;
        while let Some((waiting, at)) = self.waiting.front().copied() {
            if waiting > serial {
                break;
            }
            oldest.get_or_insert(at);
            self.waiting.pop_front();
        }
        let Some(at) = oldest else {
            return;
        };
        if image {
            if self.latencies.len() == Self::KEPT {
                self.latencies.pop_front();
            }
            self.latencies
                .push_back(at.elapsed().as_secs_f32() * 1000.0);
        } else {
            self.unchanged += 1;
        }
    }
}

/// What the window knows of the host and of the screen it draws.
#[derive(Default)]
pub(super) struct Host {
    link: Option<Link>,
    /// The host did not start, or died: it is not started again.
    failed: bool,
    /// The screen that is open, by the name the host gave it.
    open: Option<String>,
    /// That screen reads Escape itself.
    keeps_escape: bool,
    /// The size of the image the atlas's slot holds.
    image: Option<[u32; 2]>,
    /// The image's size and scale the host was last told, and how many window pixels one of its
    /// pixels is.
    viewport: Option<([u32; 2], f32)>,
    divisor: u32,
    /// Where the atlas's slot was when the image went in: an atlas made again has lost it.
    slot: Option<[u32; 2]>,
    /// The pointer as the host last heard of it, in the image's pixels.
    pointer: Option<(f32, f32)>,
    /// What the game draws itself over the image.
    overlays: Vec<wire::Overlay>,
    /// What the host was last told of the game, by key, and the frames since it was looked at.
    told: Vec<(&'static str, String)>,
    frames: u32,
    /// When the screen opened: its models' animations start then.
    opened: Option<Instant>,
    /// The server's messages that came before the host was started, in order: it has them once it is.
    early: Vec<(String, Vec<u8>)>,
    serial: u32,
    stats: Stats,
}

impl Host {
    /// How many of the server's messages wait for the host to be started: a server says little that
    /// early, and what a host that never starts would have read is not kept for ever.
    const EARLY: usize = 256;

    /// A message of the server on a channel the host reads: it goes to the host as it came, or waits
    /// for the host to be started.
    fn payload(&mut self, channel: &str, data: &[u8]) {
        self.stats.payload_at = Some(Instant::now());
        match &self.link {
            Some(link) => link.tell(&ToHost::Payload { channel, data }),
            None if !self.failed && self.early.len() < Self::EARLY => {
                self.early.push((channel.to_owned(), data.to_vec()));
            }
            None => {}
        }
    }

    /// An input goes to the host under the next serial, timed from now.
    fn input(&mut self, message: impl FnOnce(u32) -> Vec<u8>) {
        let Some(link) = &self.link else {
            return;
        };
        self.serial = self.serial.wrapping_add(1);
        self.stats.waiting.push_back((self.serial, Instant::now()));
        if self.stats.waiting.len() > Stats::KEPT {
            self.stats.waiting.pop_front();
        }
        let _ = link.to_host.send(message(self.serial));
    }

    /// The game's `json` under `key`, told to the host when it is not what it was last told.
    fn state(&mut self, key: &'static str, json: String) {
        let Some(link) = &self.link else {
            return;
        };
        match self.told.iter_mut().find(|(k, _)| *k == key) {
            Some((_, last)) if *last == json => return,
            Some((_, last)) => last.clone_from(&json),
            None => self.told.push((key, json.clone())),
        }
        link.tell(&ToHost::State { key, json: &json });
    }

    /// The pointer at `at` (in the image's pixels), told to the host when it moved.
    fn pointer_at(&mut self, at: (f32, f32), modifiers: u8) {
        if self.pointer == Some(at) {
            return;
        }
        self.pointer = Some(at);
        self.input(|serial| {
            ToHost::Pointer {
                serial,
                kind: wire::Pointer::Move,
                x: at.0,
                y: at.1,
                button: 0,
                modifiers,
            }
            .bytes()
        });
    }
}

impl Game<'_> {
    fn pokemon_host(&mut self) -> Option<&mut Host> {
        Some(&mut self.remote.as_mut()?.pokemon.host)
    }

    /// The window's size in pixels and its GUI scale.
    fn pokemon_host_window(&self) -> Option<([u32; 2], f32)> {
        let renderer = self.renderer.as_ref()?;
        let window = [renderer.config.width, renderer.config.height];
        Some((window, HudBatch::new(window).scale))
    }

    /// The cursor in the host's image, in its pixels.
    fn pokemon_host_cursor(&self) -> Option<(f32, f32)> {
        let (_, scale) = self.pokemon_host_window()?;
        let divisor = self.remote.as_ref()?.pokemon.host.divisor.max(1) as f32;
        Some((
            self.cursor.0 * scale / divisor,
            self.cursor.1 * scale / divisor,
        ))
    }

    fn pokemon_host_modifiers(&self) -> u8 {
        super::super::super::glfw_modifiers(self.modifiers) as u8
    }

    /// Once a frame: the host is started when a launch file names one, what it said since the last
    /// frame is taken, and it is told where the pointer is and how large the window is.
    pub(super) fn pokemon_host_frame(&mut self) {
        let Some(host) = self.remote.as_mut().map(|remote| &mut remote.pokemon.host) else {
            return;
        };
        if host.link.is_none() && !host.failed {
            let Some(launch) = std::env::var_os(LAUNCH) else {
                return;
            };
            match Link::start(Path::new(&launch), &self.options.resources) {
                Ok(link) => {
                    log::info!(
                        "Horizon: the screen host is started (process {})",
                        link.child.id()
                    );
                    host.link = Some(link);
                }
                Err(e) => {
                    log::warn!("Horizon: the screen host does not start: {e}");
                    host.failed = true;
                    host.early.clear();
                }
            }
        }
        loop {
            let Some(link) = self.pokemon_host().and_then(|host| host.link.as_ref()) else {
                return;
            };
            match link.from_host.try_recv() {
                Ok(message) => self.pokemon_host_message(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::warn!("Horizon: the screen host is gone");
                    if let Some(host) = self.pokemon_host() {
                        host.link = None;
                        host.failed = true;
                    }
                    self.pokemon_host_hidden();
                    return;
                }
            }
        }
        let Some((window, scale)) = self.pokemon_host_window() else {
            return;
        };
        let (size, scale, divisor) = image_size(window, scale);
        let language = self.settings.language.clone();
        let player = self.options.player_name();
        let cursor = self.pokemon_host_cursor();
        let modifiers = self.pokemon_host_modifiers();
        let hosted = self.screen == Screen::Hosted;
        let slot = self.pokemon_host_slot();
        let Some(host) = self.pokemon_host() else {
            return;
        };
        let Some(link) = &host.link else {
            return;
        };
        if host.viewport != Some((size, scale)) {
            host.viewport = Some((size, scale));
            host.divisor = divisor;
            link.tell(&ToHost::Viewport {
                width: size[0],
                height: size[1],
                scale,
                language: &language,
                player: &player,
            });
        }
        // What the server sent before the host was started, now that the host knows the window.
        if !host.early.is_empty() {
            log::info!(
                "Horizon: the screen host is handed {} messages the server sent before it was started",
                host.early.len()
            );
            for (channel, data) in host.early.drain(..) {
                link.tell(&ToHost::Payload {
                    channel: &channel,
                    data: &data,
                });
            }
        }
        // An atlas made again (a resource pack) holds nothing of the image any more.
        if host.image.is_some() && host.slot != slot {
            host.slot = slot;
            host.image = None;
            link.tell(&ToHost::Redraw);
        }
        if hosted && let Some(cursor) = cursor {
            host.pointer_at(cursor, modifiers);
        }
        host.frames = host.frames.wrapping_add(1);
        let (party, player) = (host.frames % 32 == 0, hosted && host.frames % 4 == 0);
        let inventory = hosted && host.frames % 16 == 0;
        if party {
            let json = self.pokemon_party_status().to_string();
            if let Some(host) = self.pokemon_host() {
                host.state("party", json);
            }
        }
        if player && let Some(p) = &self.player {
            let json = serde_json::json!({
                "x": p.position.x,
                "y": p.position.y,
                "z": p.position.z,
                "yaw": self.yaw,
                "pitch": self.pitch,
            })
            .to_string();
            if let Some(host) = self.pokemon_host() {
                host.state("player", json);
            }
        }
        if inventory {
            // What the player carries, for a screen that lists it (an item to put up for sale).
            let items: Vec<serde_json::Value> = self
                .inventory
                .iter()
                .enumerate()
                .filter_map(|(slot, stack)| {
                    let stack = stack.as_ref().filter(|stack| stack.count > 0)?;
                    Some(serde_json::json!({
                        "slot": slot,
                        "id": &*stack.item,
                        "count": stack.count,
                        "name": horizon_client::stations::hover_name(&self.pack.names, stack),
                    }))
                })
                .collect();
            let json =
                serde_json::json!({ "size": self.inventory.len(), "items": items }).to_string();
            if let Some(host) = self.pokemon_host() {
                host.state("inventory", json);
            }
            // Who is in the player list, for a screen that draws a player's face when they are there.
            if let Some((players, _, _)) = self.remote_tab_players() {
                let listed: Vec<(u128, &str)> =
                    players.iter().map(|p| (p.uuid, p.name.as_str())).collect();
                let json = players_json(&listed);
                if let Some(host) = self.pokemon_host() {
                    host.state("players", json);
                }
            }
        }
    }

    /// Where the atlas's slot is, in its texels.
    fn pokemon_host_slot(&self) -> Option<[u32; 2]> {
        let atlas = &self.gui.atlas;
        let rect = atlas.sprites.get(HOSTED_SCREEN_SPRITE)?;
        Some([
            (rect.u0 * atlas.width as f32).round() as u32,
            (rect.v0 * atlas.height as f32).round() as u32,
        ])
    }

    fn pokemon_host_message(&mut self, message: FromHost) {
        match message {
            FromHost::Ready {
                startup_ms,
                channels,
            } => {
                let Some(host) = self.pokemon_host() else {
                    return;
                };
                let ready = host
                    .link
                    .as_ref()
                    .map(|link| link.started.elapsed().as_secs_f32() * 1000.0);
                host.stats.ready_ms = ready;
                host.stats.startup_ms = Some(startup_ms);
                // A Fabric server sends a mod's messages only on the channels it was told are taken.
                let told = channels.iter().filter(|c| announced().contains(*c)).count();
                log::info!(
                    "Horizon: the screen host is ready after {:.0} ms and reads {} channels, {told} of \
                     which a Fabric server is told are taken",
                    ready.unwrap_or_default(),
                    channels.len()
                );
                if let Ok(mut read) = CHANNELS.write() {
                    *read = Some(channels);
                }
            }
            FromHost::Show { name, keeps_escape } => self.pokemon_host_show(name, keeps_escape),
            FromHost::Hide => self.pokemon_host_hidden(),
            FromHost::Frame(frame) => self.pokemon_host_image(frame),
            FromHost::Seen(serial) => {
                if let Some(host) = self.pokemon_host() {
                    host.stats.answered(serial, false);
                }
            }
            // What a screen makes the game say to the server is rare, and is the player's doing: it is
            // always in the log. A command is there by its name alone, what follows being the
            // player's own words; a test asks for all of it.
            FromHost::Send { channel, data } => {
                log::info!(
                    "Horizon: the screen host sends the server {channel} ({} bytes)",
                    data.len()
                );
                self.pokemon_send(&channel, data);
            }
            FromHost::Command(command) => {
                if std::env::var_os("HORIZON_COBBLEMON_TRACE").is_some() {
                    log::info!("Horizon: the screen host runs the command /{command}");
                } else {
                    let name = command.split_whitespace().next().unwrap_or_default();
                    log::info!("Horizon: the screen host runs the command /{name}");
                }
                self.send_command(&command);
            }
            FromHost::Sound { id, volume, pitch } => {
                self.audio.play(&id, None, volume, pitch);
            }
            FromHost::Log(text) => log::info!("Horizon: the screen host: {text}"),
            FromHost::Chat(text) => self.system_message(Text::literal(text)),
            FromHost::Overlays(overlays) => {
                if let Some(host) = self.pokemon_host() {
                    host.overlays = overlays;
                }
            }
        }
    }

    /// The host opens a screen: it takes the place of play, or of the screen it drew before.
    fn pokemon_host_show(&mut self, name: String, keeps_escape: bool) {
        // `Minecraft.setScreen` takes the place of whatever is open. Here the pause menu (which the
        // window opens by itself when it loses the focus) and the chat give way; a container, whose
        // closing the server must hear of, does not.
        if self.screen == Screen::Chat {
            self.close_chat(super::super::super::chat::ChatExit::Interrupted);
        }
        if self.screen == Screen::Paused {
            self.pause_focus = None;
            self.screen = Screen::Playing;
        }
        if !matches!(self.screen, Screen::Playing | Screen::Hosted) {
            // Another screen has the window: the host's is turned away.
            if let Some(link) = self.pokemon_host().and_then(|host| host.link.as_ref()) {
                link.tell(&ToHost::Closed);
            }
            return;
        }
        let Some(host) = self.pokemon_host() else {
            return;
        };
        log::info!("Horizon: the screen host shows {name}");
        host.open = Some(name);
        host.keeps_escape = keeps_escape;
        host.opened = Some(Instant::now());
        host.overlays.clear();
        host.image = None;
        host.pointer = None;
        host.stats.open_ms = None;
        if self.screen != Screen::Hosted {
            self.screen = Screen::Hosted;
            self.keys.clear();
            self.left = false;
            self.right = false;
            self.mining = None;
            self.capture(false);
        }
    }

    /// The host's screen is no more, by its own doing or because the host is gone.
    fn pokemon_host_hidden(&mut self) {
        if let Some(host) = self.pokemon_host() {
            if let Some(name) = host.open.take() {
                // What a trial without a control socket leaves to read: the link's figures so far.
                let stats = &host.stats;
                let inputs = mean_max(stats.latencies.iter().copied())
                    .map(|(mean, max, count)| {
                        format!(
                            "{count} inputs answered by an image in {mean:.1} ms on average \
                             ({max:.1} at most)"
                        )
                    })
                    .unwrap_or_else(|| "no input answered by an image".to_owned());
                log::info!(
                    "Horizon: the screen host's {name} is closed; since the host started: {} images, \
                     {inputs}, {} of {} images checked were damaged",
                    stats.frames,
                    stats.corrupt,
                    stats.checked
                );
            }
            host.overlays.clear();
            host.image = None;
            host.keeps_escape = false;
        }
        if self.screen == Screen::Hosted {
            self.screen = Screen::Playing;
            self.capture(true);
        }
    }

    /// Escape on the host's screen: the player puts it away and the host is told. A screen that
    /// keeps the key hears it instead, as it hears the others, and closes when the host says so.
    pub(in super::super::super) fn close_pokemon_host_screen(&mut self) {
        let to_host = self.pokemon_host().is_some_and(|host| {
            let unanswered = host.stats.waiting.front().map(|(_, at)| at.elapsed());
            host.link.is_some() && escape_is_the_host_s(host.keeps_escape, unanswered)
        });
        if to_host {
            self.pokemon_host_key(KeyCode::Escape, true);
            return;
        }
        if let Some(link) = self.pokemon_host().and_then(|host| host.link.as_ref()) {
            link.tell(&ToHost::Closed);
        }
        self.pokemon_host_hidden();
    }

    /// A part of the host's image: read from the shared file into the atlas's slot, and the host
    /// told it may write the next.
    fn pokemon_host_image(&mut self, frame: Frame) {
        let slot = self.pokemon_host_slot();
        let Some(host) = self.pokemon_host() else {
            return;
        };
        let Some(link) = &host.link else {
            return;
        };
        let read = if frame_fits(&frame) {
            link.read(&frame).map_err(|e| e.to_string())
        } else {
            Err(format!("an image that does not fit: {frame:?}"))
        };
        link.tell(&ToHost::Ack {
            frame: frame.number,
        });
        host.stats.answered(frame.serial, true);
        let rgba = match read {
            Ok(rgba) => rgba,
            Err(e) => {
                log::warn!("Horizon: the screen host's image: {e}");
                return;
            }
        };
        if frame.crc != 0 {
            host.stats.checked += 1;
            if crc32fast::hash(&rgba) != frame.crc {
                host.stats.corrupt += 1;
            }
        }
        host.stats.frames += 1;
        host.stats.bytes += rgba.len() as u64;
        if host.stats.host_micros.len() == Stats::KEPT {
            host.stats.host_micros.pop_front();
        }
        host.stats.host_micros.push_back(frame.micros);
        if host.image.is_none() && host.stats.open_ms.is_none() {
            host.stats.open_ms = host
                .stats
                .payload_at
                .map(|at| at.elapsed().as_secs_f32() * 1000.0);
            // A screen the server's message opened: how long the player waited for it. One a click
            // in another screen opened has no recent message to count from.
            if let (Some(name), Some(ms)) = (&host.open, host.stats.open_ms)
                && ms < 10_000.0
            {
                log::info!(
                    "Horizon: the screen host's {name} is on screen {ms:.0} ms after the server's \
                     last message"
                );
            }
        }
        // The first part of an image is all of it; a part of another size's image has no place.
        let whole = frame.w == frame.width && frame.h == frame.height;
        if !whole && host.image != Some([frame.width, frame.height]) {
            link.tell(&ToHost::Redraw);
            return;
        }
        let Some(slot) = slot else {
            return;
        };
        host.image = Some([frame.width, frame.height]);
        host.slot = Some(slot);
        self.map_patches.push(horizon_client::font::Patch {
            x: slot[0] + frame.x,
            y: slot[1] + frame.y,
            width: frame.w,
            height: frame.h,
            rgba,
        });
    }

    /// The host's screen: its image over the whole window.
    pub(in super::super::super) fn draw_pokemon_host_screen(&mut self, batch: &mut HudBatch) {
        // The models its screen shows, asked for before anything is drawn: each is there once its
        // files are read.
        let models: Vec<(usize, Option<super::Look>)> = self
            .remote
            .as_ref()
            .map(|r| r.pokemon.host.overlays.clone())
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(_, overlay)| matches!(overlay.kind, 2 | 3))
            .map(|(at, overlay)| {
                let (species, aspects, _) = model_of(&overlay.what);
                let template = self.res.mob_types().get(super::STAND_IN).cloned();
                let look = template.and_then(|t| self.pokemon_look(species, &aspects, &t));
                (at, look)
            })
            .collect();
        // And the faces: each player's skin as the player list draws it, from the same slots of the
        // atlas (a new frame for them, with what the last one drew). A player who is not in the list
        // has none.
        let wanted: Vec<(usize, String)> = self
            .remote
            .as_ref()
            .map(|r| r.pokemon.host.overlays.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(_, overlay)| overlay.kind == 4)
            .map(|(at, overlay)| (at, overlay.what.clone()))
            .collect();
        let mut faces: Vec<(usize, String)> = Vec::new();
        if !wanted.is_empty() {
            self.flush_skin_slots();
            let players = self
                .remote_tab_players()
                .map(|(players, _, _)| players)
                .unwrap_or_default();
            for (at, name) in wanted {
                if let Some(player) = players.iter().find(|p| p.name == name) {
                    faces.push((at, self.face_skin(player.uuid, &player.skin)));
                }
            }
        }
        let Some(host) = self.remote.as_ref().map(|r| &r.pokemon.host) else {
            return;
        };
        let name = host.open.clone().unwrap_or_default();
        widgets::begin_screen(|| widgets::ScreenInfo::new("hosted", name));
        let Some([width, height]) = host.image else {
            return;
        };
        let atlas = &self.gui.atlas;
        let Some(mut rect) = atlas.sprites.get(HOSTED_SCREEN_SPRITE).copied() else {
            return;
        };
        // The image is the slot's top left corner.
        rect.u1 = rect.u0 + width as f32 / atlas.width as f32;
        rect.v1 = rect.v0 + height as f32 / atlas.height as f32;
        batch.rect_uv(0.0, 0.0, batch.width, batch.height, rect, u32::MAX);
        // What the host leaves to the game, over its image: from the image's pixels to GUI pixels.
        let to_gui = batch.width / width as f32;
        let age = host
            .opened
            .map_or(0.0, |at| at.elapsed().as_secs_f32() * 20.0);
        for (at, overlay) in host.overlays.clone().into_iter().enumerate() {
            let [x, y, w, h] = overlay.rect.map(|v| v * to_gui);
            let [clip_x, clip_y, clip_w, clip_h] = overlay.clip.map(|v| v * to_gui);
            if clip_w <= 0.0 || clip_h <= 0.0 {
                continue;
            }
            let clip = [clip_x, clip_y, clip_x + clip_w, clip_y + clip_h];
            if overlay.kind == 2 {
                // A Pokémon's model as `drawProfilePokemon` stands it: `x`, `y` is where the mod's
                // pose stack put its origin and `w` how many GUI pixels a block is there.
                let look = models.iter().find(|(model, _)| *model == at);
                let Some(look) = look.and_then(|(_, look)| look.as_ref()) else {
                    continue;
                };
                use glam::{Mat4, Vec3 as V};
                let (scale, [tx, ty, tz]) = (look.profile_scale, look.profile_translation);
                let gui = Mat4::from_translation(V::new(x, y, 0.0))
                    * Mat4::from_scale(V::new(w, w, -w))
                    * Mat4::from_translation(V::new(tx, ty + 1.5 * scale, tz - 4.0))
                    * Mat4::from_scale(V::new(scale, scale, 1.0 / scale))
                    * Mat4::from_rotation_x(13f32.to_radians())
                    * Mat4::from_rotation_y(325f32.to_radians());
                let start = batch.vertices.len();
                let pose = cobblelegacy_cobblemon::wire::PoseType::Profile;
                self.draw_pokemon_model(batch, look, pose, age, gui, clip);
                if model_of(&overlay.what).2 {
                    // A species the player has not seen: its shape, in black.
                    for vertex in &mut batch.vertices[start..] {
                        vertex.color = 0xFF00_0000;
                    }
                }
                continue;
            }
            if overlay.kind == 3 {
                // A Pokémon as Cobblemon's item of its model shows it (`PokemonItemRenderer`, in an
                // interface): `x`, `y` is where the item's renderer puts the model's origin, in the
                // middle of the slot and a little above its bottom, and `w` how many GUI pixels half
                // the slot is. The model stands on that origin: a profile's translation, and the block
                // and a half that goes with it, are `drawProfilePokemon`'s alone. The model's profile
                // scale, a quarter turn less than a profile's, and the portrait pose.
                let look = models.iter().find(|(model, _)| *model == at);
                let Some(look) = look.and_then(|(_, look)| look.as_ref()) else {
                    continue;
                };
                use glam::{Mat4, Vec3 as V};
                let scale = look.profile_scale;
                let gui = Mat4::from_translation(V::new(x, y, 0.0))
                    * Mat4::from_scale(V::new(w, w, -w))
                    * Mat4::from_scale(V::new(scale, scale, 1.05))
                    * Mat4::from_rotation_y(325f32.to_radians());
                let pose = cobblelegacy_cobblemon::wire::PoseType::Portrait;
                self.draw_pokemon_model(batch, look, pose, 0.0, gui, clip);
                continue;
            }
            if overlay.kind == 4 {
                // A player's face as `PlayerSkinDrawer.draw` lays it: the face of the skin and its hat
                // layer, over the rectangle. Painted eight pixels wide at the origin, then put in
                // place: the rectangle need not lie on whole GUI pixels.
                let Some((_, skin)) = faces.iter().find(|(face, _)| *face == at) else {
                    continue;
                };
                let mut draws = Vec::new();
                horizon_client::tab_list::face(&mut draws, skin, 0, 0, 8, true, false, u32::MAX);
                let start = batch.vertices.len();
                horizon_client::hud_layout::paint(batch, &self.gui.atlas, &draws);
                for vertex in &mut batch.vertices[start..] {
                    vertex.pos = [x + vertex.pos[0] * w / 8.0, y + vertex.pos[1] * h / 8.0];
                }
                batch.clip_since(start, clip[0], clip[1], clip[2], clip[3]);
                continue;
            }
            if overlay.kind != 1 {
                continue;
            }
            // An item is 16 GUI pixels wide: sized to the rectangle and centred in it.
            let side = w.min(h);
            let (left, top) = (x + (w - side) / 2.0, y + (h - side) / 2.0);
            let start = batch.vertices.len();
            self.draw_item(batch, &ItemStack::new(&overlay.what, 1), left, top);
            batch.scale_since(start, left, top, side / 16.0, side / 16.0);
            batch.clip_since(start, clip[0], clip[1], clip[2], clip[3]);
        }
    }

    /// A mouse button over the host's screen.
    pub(in super::super::super) fn pokemon_host_mouse(
        &mut self,
        button: MouseButton,
        pressed: bool,
    ) {
        let Some(cursor) = self.pokemon_host_cursor() else {
            return;
        };
        let modifiers = self.pokemon_host_modifiers();
        let button = super::super::super::glfw_button(button) as u8;
        let Some(host) = self.pokemon_host() else {
            return;
        };
        // A click made of a move and a press in one frame: the move first.
        host.pointer_at(cursor, modifiers);
        let kind = if pressed {
            wire::Pointer::Press
        } else {
            wire::Pointer::Release
        };
        host.input(|serial| {
            ToHost::Pointer {
                serial,
                kind,
                x: cursor.0,
                y: cursor.1,
                button,
                modifiers,
            }
            .bytes()
        });
    }

    /// The wheel over the host's screen, `steps` notches up.
    pub(in super::super::super) fn pokemon_host_scroll(&mut self, steps: f32) {
        let Some(cursor) = self.pokemon_host_cursor() else {
            return;
        };
        let modifiers = self.pokemon_host_modifiers();
        let Some(host) = self.pokemon_host() else {
            return;
        };
        host.pointer_at(cursor, modifiers);
        host.input(|serial| {
            ToHost::Scroll {
                serial,
                x: cursor.0,
                y: cursor.1,
                dx: 0.0,
                dy: steps,
            }
            .bytes()
        });
    }

    /// A key pressed or let go while the host's screen is open: the host hears every one of them but
    /// Escape, which puts the screen away unless the screen keeps it
    /// ([`Self::close_pokemon_host_screen`]).
    pub(in super::super::super) fn pokemon_host_key(&mut self, key: KeyCode, down: bool) {
        if self.screen != Screen::Hosted {
            return;
        }
        let Some(key) = glfw_key(key) else {
            return;
        };
        let modifiers = self.pokemon_host_modifiers();
        if let Some(host) = self.pokemon_host() {
            host.input(|serial| {
                ToHost::Key {
                    serial,
                    down,
                    key,
                    modifiers,
                }
                .bytes()
            });
        }
    }

    /// A key pressed at the window while the host's screen is open: the key, then the characters it
    /// types.
    pub(in super::super::super) fn pokemon_host_key_event(
        &mut self,
        event: &winit::event::KeyEvent,
    ) {
        if let winit::keyboard::PhysicalKey::Code(key) = event.physical_key {
            self.pokemon_host_key(key, true);
        }
        let typed: String = event
            .text
            .as_deref()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if !typed.is_empty() {
            self.pokemon_host_type(&typed);
        }
    }

    /// Characters typed while the host's screen is open.
    pub(in super::super::super) fn pokemon_host_type(&mut self, text: &str) {
        if let Some(host) = self.pokemon_host() {
            host.input(|serial| ToHost::Text { serial, text }.bytes());
        }
    }

    /// A message of the server on a channel the host reads: it goes to the host as it came.
    pub(super) fn remote_pokemon_host(&mut self, channel: &str, data: &[u8]) -> bool {
        if !reads(channel) {
            return false;
        }
        if let Some(host) = self.pokemon_host() {
            host.payload(channel, data);
        }
        true
    }

    /// `horizon-ctl host`: the link as a test reads it. `host payload CHANNEL [HEX]` hands the host a
    /// message as if the server had sent it; `host reset` forgets the timings.
    pub(in super::super::super) fn pokemon_host_control(
        &mut self,
        words: &[&str],
    ) -> Option<Result<serde_json::Value, String>> {
        if words.first() != Some(&"host") {
            return None;
        }
        Some(match words.get(1).copied() {
            None => Ok(self.pokemon_host_status()),
            Some("reset") => {
                if let Some(host) = self.pokemon_host() {
                    let stats = std::mem::take(&mut host.stats);
                    host.stats.ready_ms = stats.ready_ms;
                    host.stats.startup_ms = stats.startup_ms;
                    // Not timings: what was checked of the images since the host started.
                    host.stats.checked = stats.checked;
                    host.stats.corrupt = stats.corrupt;
                }
                Ok(serde_json::json!({ "ok": true }))
            }
            Some("payload") => (|| {
                let channel = words.get(2).ok_or("host payload CHANNEL [HEX]")?;
                let hex = words.get(3).copied().unwrap_or_default();
                if hex.len() % 2 != 0 || !hex.is_ascii() {
                    return Err("the payload is hexadecimal".to_owned());
                }
                let data = (0..hex.len() / 2)
                    .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .map_err(|e| e.to_string())?;
                let taken = self.remote_pokemon_host(channel, &data);
                Ok(serde_json::json!({ "ok": taken }))
            })(),
            Some(other) => Err(format!("host: unknown action {other}")),
        })
    }

    /// The link as `horizon-ctl status` tells it. Null without a host.
    pub(in super::super::super) fn pokemon_host_status(&self) -> serde_json::Value {
        use serde_json::{Value, json};
        let Some(host) = self.remote.as_ref().map(|r| &r.pokemon.host) else {
            return Value::Null;
        };
        if host.link.is_none() && !host.failed {
            return Value::Null;
        }
        let stats = &host.stats;
        let latency = mean_max(stats.latencies.iter().copied());
        let drawing = mean_max(stats.host_micros.iter().map(|m| *m as f32 / 1000.0));
        json!({
            "running": host.link.is_some(),
            "pid": host.link.as_ref().map(|link| link.child.id()),
            "ready_ms": stats.ready_ms,
            "startup_ms": stats.startup_ms,
            "screen": host.open,
            "keeps_escape": host.keeps_escape,
            "image": host.image,
            "divisor": host.divisor,
            "overlays": host.overlays.len(),
            "frames": stats.frames,
            "bytes": stats.bytes,
            "open_ms": stats.open_ms,
            "input_to_image_ms": latency.map(|(mean, max, count)| json!({
                "mean": mean,
                "max": max,
                "count": count,
                "last": stats.latencies.back(),
            })),
            "inputs_without_change": stats.unchanged,
            "inputs_waiting": stats.waiting.len(),
            "host_draw_ms": drawing.map(|(mean, max, count)| json!({
                "mean": mean,
                "max": max,
                "count": count,
            })),
            "checked": stats.checked,
            "corrupt": stats.corrupt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_goes_out_with_its_length_first() {
        let bytes = ToHost::Key {
            serial: 7,
            down: true,
            key: 257,
            modifiers: 1,
        }
        .bytes();
        assert_eq!(bytes, [0, 0, 0, 11, 5, 0, 0, 0, 7, 1, 0, 0, 1, 1, 1]);
        let text = ToHost::Text {
            serial: 1,
            text: "é",
        }
        .bytes();
        assert_eq!(text, [0, 0, 0, 11, 6, 0, 0, 0, 1, 0, 0, 0, 2, 0xC3, 0xA9]);
        assert_eq!(ToHost::Closed.bytes(), [0, 0, 0, 1, 2]);
        let state = ToHost::State {
            key: "k",
            json: "{}",
        }
        .bytes();
        assert_eq!(
            state,
            [0, 0, 0, 12, 10, 0, 0, 0, 1, b'k', 0, 0, 0, 2, b'{', b'}']
        );
    }

    #[test]
    fn a_payload_carries_its_channel_and_its_bytes() {
        let bytes = ToHost::Payload {
            channel: "a:b",
            data: &[9, 8],
        }
        .bytes();
        assert_eq!(
            bytes,
            [
                0, 0, 0, 14, 7, 0, 0, 0, 3, b'a', b':', b'b', 0, 0, 0, 2, 9, 8
            ]
        );
    }

    #[test]
    fn the_host_s_messages_are_read() {
        let ready = [1, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 3, b'a', b':', b'b'];
        assert_eq!(
            wire::read(&ready),
            Ok(FromHost::Ready {
                startup_ms: 256,
                channels: vec!["a:b".to_owned()],
            })
        );
        let mut frame = vec![4];
        for value in [3u32, 1280, 720, 10, 20, 30, 40, 5, 900, 0] {
            frame.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(
            wire::read(&frame),
            Ok(FromHost::Frame(Frame {
                number: 3,
                width: 1280,
                height: 720,
                x: 10,
                y: 20,
                w: 30,
                h: 40,
                serial: 5,
                micros: 900,
                crc: 0,
            }))
        );
        assert_eq!(wire::read(&[3]), Ok(FromHost::Hide));
        // A screen says whether it keeps Escape; a host that does not say leaves it to the game.
        assert_eq!(
            wire::read(&[2, 0, 0, 0, 1, b's']),
            Ok(FromHost::Show {
                name: "s".to_owned(),
                keeps_escape: false,
            })
        );
        assert_eq!(
            wire::read(&[2, 0, 0, 0, 1, b's', 1]),
            Ok(FromHost::Show {
                name: "s".to_owned(),
                keeps_escape: true,
            })
        );
        assert_eq!(
            wire::read(&[11, 0, 0, 0, 2, b'h', b'i']),
            Ok(FromHost::Chat("hi".to_owned()))
        );
        let mut overlays = vec![10, 0, 0, 0, 1, 1];
        for value in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
            overlays.extend_from_slice(&value.to_be_bytes());
        }
        overlays.extend_from_slice(&[0, 0, 0, 3, b'a', b':', b'b']);
        assert_eq!(
            wire::read(&overlays),
            Ok(FromHost::Overlays(vec![wire::Overlay {
                kind: 1,
                rect: [1.0, 2.0, 3.0, 4.0],
                clip: [5.0, 6.0, 7.0, 8.0],
                what: "a:b".to_owned(),
            }]))
        );
        assert_eq!(
            wire::read(&[7, 0, 0, 0, 2, b'h', b'i']),
            Ok(FromHost::Command("hi".to_owned()))
        );
        assert!(wire::read(&[7, 0, 0, 0, 9, b'h']).is_err());
        assert!(wire::read(&[200]).is_err());
        assert!(wire::read(&[]).is_err());
    }

    #[test]
    fn a_window_past_the_slot_is_drawn_at_a_whole_fraction() {
        assert_eq!(image_size([1280, 720], 3.0), ([1280, 720], 3.0, 1));
        assert_eq!(image_size([1920, 1080], 4.0), ([1920, 1080], 4.0, 1));
        assert_eq!(image_size([2560, 1440], 5.0), ([1280, 720], 2.5, 2));
        assert_eq!(image_size([3840, 2160], 6.0), ([1920, 1080], 3.0, 2));
        assert_eq!(image_size([1921, 500], 2.0), ([960, 250], 1.0, 2));
    }

    #[test]
    fn an_image_s_part_must_lie_inside_it() {
        let frame = Frame {
            number: 1,
            width: 1280,
            height: 720,
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
            serial: 0,
            micros: 0,
            crc: 0,
        };
        assert!(frame_fits(&frame));
        assert!(!frame_fits(&Frame { x: 1, ..frame }));
        assert!(!frame_fits(&Frame { h: 0, ..frame }));
        assert!(!frame_fits(&Frame {
            width: 4000,
            w: 4000,
            ..frame
        }));
        assert!(!frame_fits(&Frame {
            y: u32::MAX,
            ..frame
        }));
    }

    #[test]
    fn a_model_s_overlay_names_its_species_its_aspects_and_whether_it_is_a_shape() {
        assert_eq!(
            model_of("cobblemon:pikachu;male,shiny;silhouette"),
            (
                "cobblemon:pikachu",
                vec!["male".to_owned(), "shiny".to_owned()],
                true
            )
        );
        assert_eq!(
            model_of("cobblemon:eevee;;"),
            ("cobblemon:eevee", Vec::new(), false)
        );
        assert_eq!(model_of(""), ("", Vec::new(), false));
    }

    #[test]
    fn keys_have_glfw_s_numbers() {
        assert_eq!(glfw_key(KeyCode::KeyA), Some(65));
        assert_eq!(glfw_key(KeyCode::KeyZ), Some(90));
        assert_eq!(glfw_key(KeyCode::Digit0), Some(48));
        assert_eq!(glfw_key(KeyCode::F12), Some(301));
        assert_eq!(glfw_key(KeyCode::Enter), Some(257));
        assert_eq!(glfw_key(KeyCode::Backspace), Some(259));
        assert_eq!(glfw_key(KeyCode::ArrowUp), Some(265));
        assert_eq!(glfw_key(KeyCode::ShiftLeft), Some(340));
    }

    #[test]
    fn an_answer_times_the_oldest_input_it_covers() {
        let mut stats = Stats::default();
        let now = Instant::now();
        stats.waiting.extend([(1, now), (2, now), (5, now)]);
        stats.answered(2, true);
        assert_eq!(stats.latencies.len(), 1);
        assert_eq!(stats.waiting.len(), 1);
        stats.answered(4, true);
        assert_eq!(stats.latencies.len(), 1);
        stats.answered(5, false);
        assert_eq!((stats.unchanged, stats.waiting.len()), (1, 0));
    }

    #[test]
    fn before_the_host_speaks_its_channels_are_those_the_server_was_told() {
        let announced = ["mod:open".to_owned(), "mod:state".to_owned()];
        assert!(is_read(None, &announced, "mod:state"));
        assert!(!is_read(None, &announced, "other:thing"));
        assert!(!is_read(None, &[], "mod:state"));
        // Once it has said what it reads, nothing else is its own.
        let known = ["mod:open".to_owned(), "host:test".to_owned()];
        assert!(is_read(Some(&known), &announced, "mod:open"));
        assert!(is_read(Some(&known), &announced, "host:test"));
        assert!(!is_read(Some(&known), &announced, "mod:state"));
    }

    #[test]
    fn what_the_server_says_before_the_host_is_started_waits_for_it() {
        let mut host = Host::default();
        host.payload("mod:keys", &[1, 2]);
        host.payload("mod:state", &[]);
        assert_eq!(
            host.early,
            [
                ("mod:keys".to_owned(), vec![1, 2]),
                ("mod:state".to_owned(), Vec::new())
            ]
        );
        assert!(host.stats.payload_at.is_some());
        for _ in 0..Host::EARLY * 2 {
            host.payload("mod:state", &[0]);
        }
        assert_eq!(host.early.len(), Host::EARLY);
        // A host that did not start will never read them.
        let mut failed = Host {
            failed: true,
            ..Host::default()
        };
        failed.payload("mod:keys", &[1]);
        assert!(failed.early.is_empty());
    }

    #[test]
    fn escape_is_the_host_s_on_a_screen_that_keeps_it_while_the_host_answers() {
        assert!(!escape_is_the_host_s(false, None));
        assert!(!escape_is_the_host_s(false, Some(Duration::ZERO)));
        assert!(escape_is_the_host_s(true, None));
        assert!(escape_is_the_host_s(true, Some(Duration::from_millis(200))));
        // A host that has not answered for that long is taken for hung: Escape closes its screen.
        assert!(!escape_is_the_host_s(true, Some(HUNG)));
    }

    #[test]
    fn the_player_list_is_told_by_name_and_id() {
        assert_eq!(
            players_json(&[(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef, "Rouge")]),
            r#"{"players":[{"name":"Rouge","uuid":"01234567-89ab-cdef-0123-456789abcdef"}]}"#
        );
        assert_eq!(players_json(&[]), r#"{"players":[]}"#);
    }

    #[test]
    fn figures_are_a_mean_a_largest_and_a_count() {
        assert_eq!(mean_max([1.0, 2.0, 6.0].into_iter()), Some((3.0, 6.0, 3)));
        assert_eq!(mean_max(std::iter::empty()), None);
    }
}
