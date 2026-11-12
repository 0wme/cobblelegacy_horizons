// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MoLang as Cobblemon 1.8.1 evaluates it for a model's animations: the `com.bedrockk.molang` library its
//! jar carries (`TokenIterator`, `MoLangParser` and its parselets, the `ast` classes, `MoLangMath`), with
//! the names `BedrockAnimation.run` and `MolangBoneValue.resolve` give a `PosableState`'s runtime.
//!
//! This is that library, not Bedrock's MoLang, wherever the two differ:
//! - `&&` binds looser than `||`, and all six comparisons bind alike;
//! - `a ? b : c` takes `b` only when `a` is exactly 1, and chains to the left: `a ? b : c ? d : e` is
//!   `(a ? b : c) ? d : e`; `!a` is 0 only when `a` is exactly 1;
//! - a character it does not know is skipped, a leading dot included: `.5` reads as 5;
//! - what follows the first complete expression is ignored: `1e3` is 1, `2 3` is 2;
//! - `math.sin` and `math.cos` take degrees, `math.trunc` rounds down, `math.lerp` keeps its third
//!   argument within 0 and 1, a `math.` function it does not have is 0;
//! - names are read in lower case, `q.`, `v.`, `t.` and `c.` are `query.`, `variable.`, `temp.` and
//!   `context.` (there is no `m.`), and a name that leads nowhere is 0.
//!
//! Everything is computed in `f64`, as the library does, and only then made an `f32`.
//!
//! Not here, and an error rather than a guess: what no animation of Cobblemon 1.8.1 has in a bone's
//! channel. Those are strings, assignments, `return`, `loop`, `for_each`, `break`, `continue`, `this`,
//! braces, `[]`, `??`, `->`, `math.die_roll` and `math.die_roll_integer`, and letters or digits outside
//! ASCII. An expression the library compiles and then throws on when it runs (`1 +`, `math.sin()`) is an
//! error here from the start, even where the library would never reach the part that throws.
//!
//! One string is read: an argument on its own of a function outside `math.`, as a poser's conditions
//! write them (`q.has_aspect('sheared') == false`). Those functions are the game's and read as 0 here,
//! so the string is given to nothing.
use horizon_protocol::{Error, Result};
use std::{f64::consts::PI, fmt};

/// What an expression reads besides its own numbers.
///
/// Of a `PosableState`'s many `query.` functions only `anim_time` is here: the others need the game (the
/// `query.r.` ones are a ridden Pokémon's springs) and read as 0. `variable.camera_rotation_x` and `_y`,
/// which `MolangBoneValue.resolve` also sets, read as 0 too.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    /// `query.anim_time`: `PosableState.animationSeconds`, the seconds the state itself has been running
    /// ((age + partial tick) / 20), whichever animation asks. It is not the time within that animation,
    /// which is `variable.anim_time`: [`Expression::eval`] reads this field for both, a channel of
    /// [`crate::animation`] gives `variable.anim_time` its own time.
    pub anim_time: f32,
    /// `variable.age_in_ticks`: the `ageInTicks` given to `BedrockAnimation.run`. `query.age_in_ticks`
    /// is not a function of the state and reads as 0, as do the two below under `query.`.
    pub age_in_ticks: f32,
    /// `variable.limb_swing`.
    pub limb_swing: f32,
    /// `variable.limb_swing_amount`.
    pub limb_swing_amount: f32,
}

/// [`Context`] as an expression reads it: widened to the `f64` the library holds everything in, with the
/// two times apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Inputs {
    query_anim_time: f64,
    variable_anim_time: f64,
    age_in_ticks: f64,
    limb_swing: f64,
    limb_swing_amount: f64,
}

impl Inputs {
    /// `context`, with `animation_seconds` as `variable.anim_time`: the time `MolangBoneValue.resolve` is
    /// given, which is the time within the animation.
    pub(crate) fn new(context: &Context, animation_seconds: f64) -> Self {
        Self {
            query_anim_time: f64::from(context.anim_time),
            variable_anim_time: animation_seconds,
            age_in_ticks: f64::from(context.age_in_ticks),
            limb_swing: f64::from(context.limb_swing),
            limb_swing_amount: f64::from(context.limb_swing_amount),
        }
    }

    fn read(&self, input: Input) -> f64 {
        match input {
            Input::QueryAnimTime => self.query_anim_time,
            Input::VariableAnimTime => self.variable_anim_time,
            Input::AgeInTicks => self.age_in_ticks,
            Input::LimbSwing => self.limb_swing,
            Input::LimbSwingAmount => self.limb_swing_amount,
        }
    }

    /// What stands in for `Math.random()`: a number from 0 up to 1 that depends on which `math.random`
    /// asks (`seed`) and on the moment, so that it changes with every frame as the library's does, and is
    /// the same whenever the same moment is asked about again.
    fn roll(&self, seed: u64) -> f64 {
        let mut state = seed;
        for time in [
            self.query_anim_time,
            self.variable_anim_time,
            self.age_in_ticks,
        ] {
            state = mix(state ^ time.to_bits());
        }
        // The top 53 bits, as a fraction: every `f64` of that many bits below 1.
        (state >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// What a compiled expression reads as it runs: a model's animation reads [`Inputs`], a particle effect
/// its [`Scope`].
trait Source {
    fn read(&self, input: Input) -> f64;
    /// The variable a [`Scope`] keeps at `index`.
    fn slot(&self, index: u16) -> f64;
    /// `Math.random()` for the `math.random` that `seed` tells from every other.
    fn random(&self, seed: u64) -> f64;
}

impl Source for Inputs {
    fn read(&self, input: Input) -> f64 {
        Inputs::read(self, input)
    }

    fn slot(&self, _: u16) -> f64 {
        0.0
    }

    fn random(&self, seed: u64) -> f64 {
        self.roll(seed)
    }
}

/// What an expression compiled by [`Expression::parse_scoped`] reads: the variables of a runtime that
/// has some, each at the slot its name was given when the expression was compiled.
///
/// `math.random` draws a new number each time it is evaluated, as the library's `Math.random()` does:
/// `random` is the state those draws move on.
#[derive(Clone, Copy, Debug)]
pub struct Scope<'a> {
    pub slots: &'a [f64],
    pub random: &'a std::cell::Cell<u64>,
}

impl Scope<'_> {
    /// The next number from 0 up to 1 of `random`: one SplitMix64 output.
    pub fn roll(random: &std::cell::Cell<u64>) -> f64 {
        let state = random.get();
        random.set(state.wrapping_add(0x9E37_79B9_7F4A_7C15));
        (mix(state) >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl Source for Scope<'_> {
    /// The numbers a model's animation is given: a particle's runtime has none of them.
    fn read(&self, _: Input) -> f64 {
        0.0
    }

    fn slot(&self, index: u16) -> f64 {
        self.slots.get(usize::from(index)).copied().unwrap_or(0.0)
    }

    fn random(&self, _: u64) -> f64 {
        Self::roll(self.random)
    }
}

/// SplitMix64's step: every bit of the result depends on every bit of `state`.
fn mix(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a: tells one text from another, for the random numbers of what is compiled from it.
pub(crate) fn hash(text: &str) -> u64 {
    text.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

/// One of the numbers of [`Inputs`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Input {
    QueryAnimTime,
    VariableAnimTime,
    AgeInTicks,
    LimbSwing,
    LimbSwingAmount,
}

/// What is made of one number: the two prefix operators and `MoLangMath`'s functions of one argument.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unary {
    Negate,
    Not,
    Abs,
    Acos,
    Asin,
    Atan,
    Ceil,
    Cos,
    Exp,
    /// `math.floor`, and `math.trunc`, which is `Math.floor` too.
    Floor,
    HermiteBlend,
    Ln,
    Round,
    Sign,
    Sin,
    Sqrt,
    ToDegrees,
    ToRadians,
}

/// What is made of two numbers: the infix operators and `MoLangMath`'s functions of two arguments.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Binary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    NotEqual,
    Smaller,
    SmallerOrEqual,
    Greater,
    GreaterOrEqual,
    And,
    Or,
    /// `a ?: b`, a ternary without its middle.
    Otherwise,
    Atan2,
    CopySign,
    Max,
    Min,
    Mod,
    Pow,
}

/// What is made of three numbers: the ternary and `MoLangMath`'s functions of three arguments.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ternary {
    Select,
    Clamp,
    Lerp,
    LerpRotate,
}

/// `Math.max`: unlike `f64::max`, a NaN on either side is the result.
fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 && a.is_sign_negative() {
        b
    } else if a >= b {
        a
    } else {
        b
    }
}

/// `Math.min`: unlike `f64::min`, a NaN on either side is the result.
fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 && b.is_sign_negative() {
        b
    } else if a <= b {
        a
    } else {
        b
    }
}

/// `Math.round(double)`: the nearest `long`, halves upwards, 0 for a NaN.
fn java_round(a: f64) -> i64 {
    let floor = a.floor();
    (if a - floor >= 0.5 { floor + 1.0 } else { floor }) as i64
}

/// `Math.pow`, which is C's `pow` but for two cases C answers 1 in.
fn java_pow(a: f64, b: f64) -> f64 {
    if b.is_nan() || (a.abs() == 1.0 && b.is_infinite()) {
        f64::NAN
    } else {
        a.powf(b)
    }
}

/// `MoLangMath.radify`.
fn radify(degrees: f64) -> f64 {
    ((degrees + 180.0) % 360.0 + 180.0) % 360.0
}

fn truth(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

/// The `ast` class or the `MoLangMath` function of one number, written out as it is there: the order of
/// the operations is the library's, so the last bit is too.
fn unary(op: Unary, a: f64) -> f64 {
    match op {
        Unary::Negate => -a,
        // `BooleanNotExpression`: only exactly 1 is true.
        Unary::Not => truth(a != 1.0),
        Unary::Abs => a.abs(),
        Unary::Acos => a.acos() * 180.0 / PI,
        Unary::Asin => a.asin() * 180.0 / PI,
        Unary::Atan => a.atan() * 180.0 / PI,
        Unary::Ceil => a.ceil(),
        Unary::Cos => (a * PI / 180.0).cos(),
        Unary::Exp => a.exp(),
        Unary::Floor => a.floor(),
        // `3 * value ^ 2 - 2 * value ^ 3` on an `int`, where Java's `^` is exclusive or and binds last.
        Unary::HermiteBlend => {
            let value = a as i32;
            f64::from(value.wrapping_mul(3) ^ 2i32.wrapping_sub(value.wrapping_mul(2)) ^ 3)
        }
        Unary::Ln => a.ln(),
        Unary::Round => java_round(a) as f64,
        Unary::Sign => {
            if a >= 0.0 {
                1.0
            } else {
                -1.0
            }
        }
        Unary::Sin => (a * PI / 180.0).sin(),
        Unary::Sqrt => a.sqrt(),
        // `Math.toDegrees` and `Math.toRadians` of Java 21: one multiplication by these constants.
        Unary::ToDegrees => a * 57.295_779_513_082_32,
        Unary::ToRadians => a * 0.017_453_292_519_943_295,
    }
}

/// The `ast.binaryop` class or the `MoLangMath` function of two numbers.
fn binary(op: Binary, a: f64, b: f64) -> f64 {
    match op {
        Binary::Add => a + b,
        Binary::Subtract => a - b,
        Binary::Multiply => a * b,
        Binary::Divide => a / b,
        Binary::Equal => truth(a == b),
        Binary::NotEqual => truth(a != b),
        Binary::Smaller => truth(a < b),
        Binary::SmallerOrEqual => truth(a <= b),
        Binary::Greater => truth(a > b),
        Binary::GreaterOrEqual => truth(a >= b),
        // `BooleanAndExpression`, `BooleanOrExpression`: anything but 0 is true, a NaN included.
        Binary::And => truth(a != 0.0 && b != 0.0),
        Binary::Or => truth(a != 0.0 || b != 0.0),
        // `TernaryExpression` without `thenExpr`: the condition itself when it is true.
        Binary::Otherwise => {
            if a == 1.0 {
                a
            } else {
                b
            }
        }
        Binary::Atan2 => a.atan2(b) * 180.0 / PI,
        Binary::CopySign => a.abs() * if b >= 0.0 { 1.0 } else { -1.0 },
        Binary::Max => java_max(a, b),
        Binary::Min => java_min(a, b),
        Binary::Mod => a % b,
        Binary::Pow => java_pow(a, b),
    }
}

/// `TernaryExpression` or the `MoLangMath` function of three numbers.
fn ternary(op: Ternary, a: f64, b: f64, c: f64) -> f64 {
    match op {
        // Only exactly 1 is true.
        Ternary::Select => {
            if a == 1.0 {
                b
            } else {
                c
            }
        }
        Ternary::Clamp => java_min(c, java_max(a, b)),
        Ternary::Lerp => a + (b - a) * java_max(0.0, java_min(1.0, c)),
        Ternary::LerpRotate => {
            let (mut start, mut end) = (radify(a), radify(b));
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            if end - start > 180.0 {
                radify(end + c * (360.0 - (end - start)))
            } else {
                start + c * (end - start)
            }
        }
    }
}

/// `MoLangMath.random`.
fn random(low: f64, high: f64, unit: f64) -> f64 {
    low + unit * (high - low)
}

/// `MoLangMath.randomInt`: its arguments cut to `int`s first, its result an `int` again.
fn random_integer(low: f64, high: f64, unit: f64) -> f64 {
    let (low, high) = (low as i32, high as i32);
    let rolled = f64::from(low) + unit * f64::from(high.wrapping_sub(low));
    f64::from(java_round(rolled) as i32)
}

/// One step of a compiled expression, which works on a stack of numbers: an expression is its operands'
/// steps and then its own, so a step finds its operands on top of the stack and leaves its result there.
///
/// Every operand is computed, the branch a ternary does not take and the right of an `&&` that is already
/// false too. The library skips those, which shows only in what it would have assigned or rolled there,
/// and nothing here assigns or keeps a roll.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Number(f64),
    Read(Input),
    /// A variable of a [`Scope`].
    Slot(u16),
    Unary(Unary),
    Binary(Binary),
    Ternary(Ternary),
    /// `math.random(low, high)`, with what tells this one from every other (`Inputs::random`).
    Random(u64),
    /// `math.random_integer(low, high)`, likewise.
    RandomInteger(u64),
}

/// How many numbers an expression may need at once. The deepest of Cobblemon 1.8.1's needs 7.
const STACK: usize = 32;

/// How deep an expression's parentheses, arguments and prefix operators may nest: the parser recurses
/// once for each.
const MAX_NESTING: usize = 100;

/// The most numbers `ops` has on the stack at once, if every step finds its operands and one number is
/// left at the end.
fn stack_needed(ops: &[Op]) -> Option<usize> {
    let (mut height, mut highest) = (0usize, 0);
    for op in ops {
        let (taken, left) = match op {
            Op::Number(_) | Op::Read(_) | Op::Slot(_) => (0, 1),
            Op::Unary(_) => (1, 1),
            Op::Binary(_) | Op::Random(_) | Op::RandomInteger(_) => (2, 1),
            Op::Ternary(_) => (3, 1),
        };
        height = height.checked_sub(taken)? + left;
        highest = highest.max(height);
    }
    (height == 1).then_some(highest)
}

fn run(ops: &[Op], inputs: &impl Source) -> f64 {
    let mut stack = [0.0; STACK];
    let mut height = 0;
    for op in ops {
        match *op {
            Op::Number(value) => {
                stack[height] = value;
                height += 1;
            }
            Op::Read(input) => {
                stack[height] = inputs.read(input);
                height += 1;
            }
            Op::Slot(index) => {
                stack[height] = inputs.slot(index);
                height += 1;
            }
            Op::Unary(op) => stack[height - 1] = unary(op, stack[height - 1]),
            Op::Binary(op) => {
                height -= 1;
                stack[height - 1] = binary(op, stack[height - 1], stack[height]);
            }
            Op::Ternary(op) => {
                height -= 2;
                stack[height - 1] =
                    ternary(op, stack[height - 1], stack[height], stack[height + 1]);
            }
            Op::Random(seed) => {
                height -= 1;
                stack[height - 1] = random(stack[height - 1], stack[height], inputs.random(seed));
            }
            Op::RandomInteger(seed) => {
                height -= 1;
                stack[height - 1] =
                    random_integer(stack[height - 1], stack[height], inputs.random(seed));
            }
        }
    }
    stack[0]
}

/// A compiled MoLang expression. Compiling allocates, evaluating does not.
#[derive(Clone, Debug, PartialEq)]
pub struct Expression(Compiled);

#[derive(Clone, Debug, PartialEq)]
enum Compiled {
    /// Nothing in it depended on the context: it is this number.
    Constant(f64),
    /// Its steps, which [`stack_needed`] has checked [`run`] can follow.
    Steps(Box<[Op]>),
}

impl Expression {
    /// Compiles `source` as `MoLang.createParser(source).parseExpression()` reads it: its first
    /// expression. What Cobblemon does to a bone's text before that (`cleanExpression`) is
    /// [`crate::animation`]'s to do.
    ///
    /// An error is something the library refuses or throws on, or something this module leaves out (see
    /// the module's own description).
    pub fn parse(source: &str) -> Result<Self> {
        Self::parse_noting(source, &mut |_| {})
    }

    /// [`Self::parse`], telling `name` every name `source` refers to before it gets to an error, as the
    /// library looks it up: in lower case, its first part written out (`query.anim_time`, `math.sin`,
    /// `variable.speed`, `query.r.dive`).
    pub fn parse_noting(source: &str, name: &mut dyn FnMut(&str)) -> Result<Self> {
        Self::compile(source, hash(source), name)
    }

    /// [`Self::parse_noting`] with what tells this expression's `math.random`s from another's: two
    /// expressions of one `seed` and one text roll the same numbers.
    pub(crate) fn compile(source: &str, seed: u64, name: &mut dyn FnMut(&str)) -> Result<Self> {
        Self::compile_with(source, seed, name, None)
    }

    /// Compiles `source` for a runtime that has variables: `slot` is asked about every name outside
    /// `math.`, as the library looks it up (`variable.particle_age`, `query.entity_size`), and answers
    /// the slot of a [`Scope`] that name reads. A name it has no slot for is what [`Self::parse`] makes
    /// of it: 0, but for the few names a model's animation is given, which a `Scope` reads as 0 too.
    pub fn parse_scoped(source: &str, slot: &mut dyn FnMut(&str) -> Option<u16>) -> Result<Self> {
        Self::compile_with(source, hash(source), &mut |_| {}, Some(slot))
    }

    /// The expression's value in `scope`, as the library computes it. Never allocates.
    pub fn eval_scoped(&self, scope: &Scope) -> f64 {
        match &self.0 {
            Compiled::Constant(value) => *value,
            Compiled::Steps(ops) => run(ops, scope),
        }
    }

    fn compile_with<'n>(
        source: &str,
        seed: u64,
        name: &'n mut dyn FnMut(&str),
        slots: Option<&'n mut dyn FnMut(&str) -> Option<u16>>,
    ) -> Result<Self> {
        let mut parser = Parser {
            source,
            index: 0,
            peeked: None,
            ops: Vec::new(),
            nesting: 0,
            seed,
            randoms: 0,
            name: String::new(),
            note: name,
            slots,
        };
        // `parseExpression` answers null for an empty text, which Cobblemon cannot hold as a bone's value.
        if parser.expression(ANYTHING)?.is_none() {
            return Err(bad("there is nothing to evaluate"));
        }
        // Whatever is left of the text is never looked at.
        let ops = parser.ops;
        if let [Op::Number(value)] = ops[..] {
            return Ok(Self(Compiled::Constant(value)));
        }
        match stack_needed(&ops) {
            Some(needed) if needed <= STACK => Ok(Self(Compiled::Steps(ops.into_boxed_slice()))),
            _ => Err(bad(format_args!(
                "the expression needs more than {STACK} numbers at once"
            ))),
        }
    }

    /// The expression that is `value`.
    pub fn number(value: f32) -> Self {
        Self::exact(f64::from(value))
    }

    /// The expression that is `value`, as the library holds a number.
    pub(crate) fn exact(value: f64) -> Self {
        Self(Compiled::Constant(value))
    }

    /// The expression's value if nothing in it depends on the context: it has no `query.anim_time`, no
    /// `variable.` of [`Context`] and no `math.random`. A name this module reads as 0 counts as that 0.
    pub fn constant(&self) -> Option<f32> {
        self.constant_exact().map(|value| value as f32)
    }

    pub(crate) fn constant_exact(&self) -> Option<f64> {
        match self.0 {
            Compiled::Constant(value) => Some(value),
            Compiled::Steps(_) => None,
        }
    }

    /// The expression's value, with `context.anim_time` for both `query.anim_time` and
    /// `variable.anim_time`. Never allocates. Dividing by zero is infinite or NaN, as in the library.
    pub fn eval(&self, context: &Context) -> f32 {
        self.eval_exact(&Inputs::new(context, f64::from(context.anim_time))) as f32
    }

    /// The expression's value as the library computes it, before Cobblemon makes it a `Float`.
    pub(crate) fn eval_exact(&self, inputs: &Inputs) -> f64 {
        match &self.0 {
            Compiled::Constant(value) => *value,
            Compiled::Steps(ops) => run(ops, inputs),
        }
    }
}

/// `TokenType`'s operators and punctuation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Symbol {
    Equals,
    NotEquals,
    Coalesce,
    And,
    Or,
    GreaterOrEquals,
    SmallerOrEquals,
    Arrow,
    Greater,
    Smaller,
    BracketLeft,
    BracketRight,
    ArrayLeft,
    ArrayRight,
    CurlyBracketLeft,
    CurlyBracketRight,
    Comma,
    Assign,
    Plus,
    Minus,
    Asterisk,
    Slash,
    Question,
    Colon,
    Semicolon,
    Bang,
}

/// `Precedence`'s ordinals: an operator takes what follows it up to the next operator that does not bind
/// tighter than itself.
const ANYTHING: u8 = 0;
const ASSIGNMENT: u8 = 2;
const CONDITIONAL: u8 = 3;
const ARRAY_ACCESS: u8 = 4;
const COALESCE: u8 = 5;
const AND: u8 = 6;
const OR: u8 = 7;
const COMPARE: u8 = 8;
const SUM: u8 = 9;
const PRODUCT: u8 = 10;
const PREFIX: u8 = 11;
const ARROW: u8 = 12;

impl Symbol {
    /// The symbols of two characters, which `TokenIterator.next` tries first.
    fn of_two(first: u8, second: u8) -> Option<Self> {
        Some(match (first, second) {
            (b'=', b'=') => Self::Equals,
            (b'!', b'=') => Self::NotEquals,
            (b'?', b'?') => Self::Coalesce,
            (b'&', b'&') => Self::And,
            (b'|', b'|') => Self::Or,
            (b'>', b'=') => Self::GreaterOrEquals,
            (b'<', b'=') => Self::SmallerOrEquals,
            (b'-', b'>') => Self::Arrow,
            _ => return None,
        })
    }

    fn of_one(character: u8) -> Option<Self> {
        Some(match character {
            b'>' => Self::Greater,
            b'<' => Self::Smaller,
            b'(' => Self::BracketLeft,
            b')' => Self::BracketRight,
            b'[' => Self::ArrayLeft,
            b']' => Self::ArrayRight,
            b'{' => Self::CurlyBracketLeft,
            b'}' => Self::CurlyBracketRight,
            b',' => Self::Comma,
            b'=' => Self::Assign,
            b'+' => Self::Plus,
            b'-' => Self::Minus,
            b'*' => Self::Asterisk,
            b'/' => Self::Slash,
            b'?' => Self::Question,
            b':' => Self::Colon,
            b';' => Self::Semicolon,
            b'!' => Self::Bang,
            _ => return None,
        })
    }

    fn text(self) -> &'static str {
        match self {
            Self::Equals => "==",
            Self::NotEquals => "!=",
            Self::Coalesce => "??",
            Self::And => "&&",
            Self::Or => "||",
            Self::GreaterOrEquals => ">=",
            Self::SmallerOrEquals => "<=",
            Self::Arrow => "->",
            Self::Greater => ">",
            Self::Smaller => "<",
            Self::BracketLeft => "(",
            Self::BracketRight => ")",
            Self::ArrayLeft => "[",
            Self::ArrayRight => "]",
            Self::CurlyBracketLeft => "{",
            Self::CurlyBracketRight => "}",
            Self::Comma => ",",
            Self::Assign => "=",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Asterisk => "*",
            Self::Slash => "/",
            Self::Question => "?",
            Self::Colon => ":",
            Self::Semicolon => ";",
            Self::Bang => "!",
        }
    }

    /// How tightly the symbol binds what is on its two sides, if `MoLangParser` has an infix parselet for
    /// it: a symbol without one ends the expression.
    fn binding(self) -> Option<u8> {
        Some(match self {
            Self::Assign => ASSIGNMENT,
            Self::Question => CONDITIONAL,
            Self::ArrayLeft => ARRAY_ACCESS,
            Self::Coalesce => COALESCE,
            Self::And => AND,
            Self::Or => OR,
            Self::Equals
            | Self::NotEquals
            | Self::Greater
            | Self::GreaterOrEquals
            | Self::Smaller
            | Self::SmallerOrEquals => COMPARE,
            Self::Plus | Self::Minus => SUM,
            Self::Asterisk | Self::Slash => PRODUCT,
            Self::Arrow => ARROW,
            _ => return None,
        })
    }

    /// `GenericBinaryOpParselet`'s expression for the symbol, among those this module has.
    fn operator(self) -> Option<Binary> {
        Some(match self {
            Self::And => Binary::And,
            Self::Or => Binary::Or,
            Self::Slash => Binary::Divide,
            Self::Equals => Binary::Equal,
            Self::Greater => Binary::Greater,
            Self::GreaterOrEquals => Binary::GreaterOrEqual,
            Self::Minus => Binary::Subtract,
            Self::NotEquals => Binary::NotEqual,
            Self::Plus => Binary::Add,
            Self::Asterisk => Binary::Multiply,
            Self::Smaller => Binary::Smaller,
            Self::SmallerOrEquals => Binary::SmallerOrEqual,
            _ => return None,
        })
    }
}

/// What `TokenIterator.next` hands the parser.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'s> {
    Symbol(Symbol),
    /// A name or a keyword, as written: the library puts it in lower case.
    Name(&'s str),
    /// Digits with at most one dot among them, after the first.
    Number(&'s str),
    /// Something between single quotes.
    Text,
    End,
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Symbol(symbol) => write!(f, "`{}`", symbol.text()),
            Self::Name(written) | Self::Number(written) => write!(f, "`{written}`"),
            Self::Text => f.write_str("a string"),
            Self::End => f.write_str("the end"),
        }
    }
}

/// The names `TokenType` makes tokens of their own, besides `true` and `false`: all statements or
/// structures, none of which is here.
const KEYWORDS: [&str; 6] = ["return", "continue", "break", "for_each", "loop", "this"];

fn bad(message: impl fmt::Display) -> Error {
    Error::new(format!("molang: {message}"))
}

fn unsupported(what: impl fmt::Display) -> Error {
    bad(format_args!("{what} is not among what this evaluates"))
}

/// A function of `MoLangMath.LIBRARY` that takes arguments.
#[derive(Clone, Copy)]
enum Callee {
    Unary(Unary),
    Binary(Binary),
    Ternary(Ternary),
    Random,
    RandomInteger,
}

impl Callee {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "abs" => Self::Unary(Unary::Abs),
            "acos" => Self::Unary(Unary::Acos),
            "sin" => Self::Unary(Unary::Sin),
            "asin" => Self::Unary(Unary::Asin),
            "atan" => Self::Unary(Unary::Atan),
            "atan2" => Self::Binary(Binary::Atan2),
            "ceil" => Self::Unary(Unary::Ceil),
            "clamp" => Self::Ternary(Ternary::Clamp),
            "cos" => Self::Unary(Unary::Cos),
            "exp" => Self::Unary(Unary::Exp),
            "mod" => Self::Binary(Binary::Mod),
            "floor" | "trunc" => Self::Unary(Unary::Floor),
            "hermite_blend" => Self::Unary(Unary::HermiteBlend),
            "lerp" => Self::Ternary(Ternary::Lerp),
            "lerp_rotate" => Self::Ternary(Ternary::LerpRotate),
            "ln" => Self::Unary(Unary::Ln),
            "max" => Self::Binary(Binary::Max),
            "min" => Self::Binary(Binary::Min),
            "pow" => Self::Binary(Binary::Pow),
            "random" => Self::Random,
            "random_integer" => Self::RandomInteger,
            "round" => Self::Unary(Unary::Round),
            "sqrt" => Self::Unary(Unary::Sqrt),
            "d2r" => Self::Unary(Unary::ToRadians),
            "r2d" => Self::Unary(Unary::ToDegrees),
            "sign" => Self::Unary(Unary::Sign),
            "copy_sign" => Self::Binary(Binary::CopySign),
            _ => return None,
        })
    }

    /// How many arguments the function reads: fewer is an `IndexOutOfBoundsException` in the library, more
    /// are computed and never looked at.
    fn arity(self) -> usize {
        match self {
            Self::Unary(_) => 1,
            Self::Binary(_) | Self::Random | Self::RandomInteger => 2,
            Self::Ternary(_) => 3,
        }
    }
}

/// A call's arguments, already compiled one after the other.
struct Arguments {
    count: usize,
    /// Where each of the first four starts among the steps: no function reads more than three, and the
    /// fourth's start is where the three end.
    starts: [usize; 4],
}

/// `MoLangParser` over `TokenIterator`, writing the steps of what it reads instead of a tree: an
/// expression's steps are its operands' followed by its own, so a parsed expression is everything from
/// where it started to the end of `ops`.
struct Parser<'s, 'n> {
    source: &'s str,
    /// Where `TokenIterator` is in `source`.
    index: usize,
    /// `MoLangParser.readTokens`, which never holds more than the next token.
    peeked: Option<Token<'s>>,
    ops: Vec<Op>,
    nesting: usize,
    seed: u64,
    /// How many `math.random`s there are so far.
    randoms: u64,
    /// Where a name is spelled out, kept from one name to the next.
    name: String,
    note: &'n mut dyn FnMut(&str),
    /// The slots of a [`Scope`]'s variables, by name, when the expression is compiled for one.
    slots: Option<&'n mut dyn FnMut(&str) -> Option<u16>>,
}

impl<'s> Parser<'s, '_> {
    /// `TokenIterator.next`.
    fn scan(&mut self) -> Result<Token<'s>> {
        let bytes = self.source.as_bytes();
        loop {
            let start = self.index;
            let Some(&byte) = bytes.get(start) else {
                return Ok(Token::End);
            };
            if let Some(&second) = bytes.get(start + 1)
                && let Some(symbol) = Symbol::of_two(byte, second)
            {
                self.index += 2;
                return Ok(Token::Symbol(symbol));
            }
            if let Some(symbol) = Symbol::of_one(byte) {
                self.index += 1;
                return Ok(Token::Symbol(symbol));
            }
            match byte {
                b'\'' => {
                    // Up to the next quote that no backslash precedes, or to the end.
                    let (mut end, mut previous) = (start + 1, 0);
                    while end < bytes.len() && (bytes[end] != b'\'' || previous == b'\\') {
                        previous = bytes[end];
                        end += 1;
                    }
                    self.index = (end + 1).min(bytes.len());
                    return Ok(Token::Text);
                }
                b'a'..=b'z' | b'A'..=b'Z' => {
                    let mut end = start + 1;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'.'))
                    {
                        end += 1;
                    }
                    self.index = end;
                    self.ascii_follows()?;
                    return Ok(Token::Name(&self.source[start..end]));
                }
                b'0'..=b'9' => {
                    let (mut end, mut decimal) = (start + 1, false);
                    while end < bytes.len()
                        && (bytes[end].is_ascii_digit() || (bytes[end] == b'.' && !decimal))
                    {
                        decimal |= bytes[end] == b'.';
                        end += 1;
                    }
                    self.index = end;
                    self.ascii_follows()?;
                    return Ok(Token::Number(&self.source[start..end]));
                }
                0x80.. => {
                    self.ascii_follows()?;
                    // Not a letter nor a digit: skipped like any character the library has no use for.
                    let character = self.source[start..].chars().next();
                    self.index += character.map_or(1, char::len_utf8);
                }
                // Spaces, and everything else that is nothing to the library: `%`, `^`, `#`, `"`, a dot or
                // an underscore that starts nothing.
                _ => self.index += 1,
            }
        }
    }

    /// Refuses a letter or a digit outside ASCII where the tokenizer is: Java decides what those are, and
    /// they would start a name or carry on the one just read.
    fn ascii_follows(&self) -> Result<()> {
        match self.source[self.index..].chars().next() {
            Some(character) if !character.is_ascii() && character.is_alphanumeric() => {
                Err(unsupported(format_args!("`{character}`")))
            }
            _ => Ok(()),
        }
    }

    fn peek(&mut self) -> Result<Token<'s>> {
        match self.peeked {
            Some(token) => Ok(token),
            None => {
                let token = self.scan()?;
                self.peeked = Some(token);
                Ok(token)
            }
        }
    }

    /// `MoLangParser.consumeToken()`.
    fn take(&mut self) -> Result<Token<'s>> {
        match self.peeked.take() {
            Some(token) => Ok(token),
            None => self.scan(),
        }
    }

    /// `MoLangParser.matchToken`: takes the next token if it is `symbol`.
    fn eat(&mut self, symbol: Symbol) -> Result<bool> {
        let found = self.peek()? == Token::Symbol(symbol);
        if found {
            self.peeked = None;
        }
        Ok(found)
    }

    /// `MoLangParser.consumeToken(expectedType)`.
    fn expect(&mut self, symbol: Symbol) -> Result<()> {
        match self.take()? {
            Token::Symbol(found) if found == symbol => Ok(()),
            found => Err(bad(format_args!(
                "expected `{}`, found {found}",
                symbol.text()
            ))),
        }
    }

    /// `MoLangParser.parseExpression(precedence)`: one operand and every operator after it that binds
    /// tighter than `precedence`. Answers where its steps start, or nothing at the end of the text, where
    /// the library answers null.
    fn expression(&mut self, precedence: u8) -> Result<Option<usize>> {
        let token = self.take()?;
        if token == Token::End {
            return Ok(None);
        }
        if self.nesting == MAX_NESTING {
            return Err(bad(format_args!(
                "the expression nests deeper than {MAX_NESTING}"
            )));
        }
        self.nesting += 1;
        let start = self.ops.len();
        match token {
            // `BooleanParselet`.
            Token::Name(written) if written.eq_ignore_ascii_case("true") => {
                self.ops.push(Op::Number(1.0));
            }
            Token::Name(written) if written.eq_ignore_ascii_case("false") => {
                self.ops.push(Op::Number(0.0));
            }
            Token::Name(written)
                if KEYWORDS
                    .iter()
                    .any(|keyword| written.eq_ignore_ascii_case(keyword)) =>
            {
                return Err(unsupported(token));
            }
            // `NameParselet`: the arguments come first.
            Token::Name(written) => {
                let arguments = self.arguments(written)?;
                self.name(written, start, &arguments)?;
            }
            // `NumberParselet`: `Double.parseDouble` of digits and at most one dot.
            Token::Number(written) => {
                let value = written
                    .parse()
                    .map_err(|_| bad(format_args!("{token} is not a number")))?;
                self.ops.push(Op::Number(value));
            }
            // `GroupParselet`.
            Token::Symbol(Symbol::BracketLeft) => {
                self.operand(ANYTHING, token)?;
                self.expect(Symbol::BracketRight)?;
            }
            // `UnaryMinusParselet`, `UnaryPlusParselet`, `BooleanNotParselet`.
            Token::Symbol(Symbol::Minus) => {
                self.operand(PREFIX, token)?;
                self.unary(Unary::Negate, start);
            }
            Token::Symbol(Symbol::Plus) => {
                self.operand(PREFIX, token)?;
            }
            Token::Symbol(Symbol::Bang) => {
                self.operand(PREFIX, token)?;
                self.unary(Unary::Not, start);
            }
            Token::Text | Token::Symbol(Symbol::CurlyBracketLeft) => {
                return Err(unsupported(token));
            }
            // "Cannot parse ... expression".
            Token::Symbol(_) | Token::End => {
                return Err(bad(format_args!("{token} where a value should start")));
            }
        }
        // `parseInfixExpression`.
        while let Token::Symbol(symbol) = self.peek()?
            && let Some(binding) = symbol.binding()
            && precedence < binding
        {
            self.peeked = None;
            if symbol == Symbol::Question {
                self.conditional(start)?;
            } else if let Some(operator) = symbol.operator() {
                let right = self.operand(binding, Token::Symbol(symbol))?;
                self.binary(operator, start, right);
            } else {
                // `=`, `[`, `??`, `->`.
                return Err(unsupported(Token::Symbol(symbol)));
            }
        }
        self.nesting -= 1;
        Ok(Some(start))
    }

    /// [`Self::expression`] where the library would go on with a null and throw once it evaluates it.
    fn operand(&mut self, precedence: u8, after: Token) -> Result<usize> {
        self.expression(precedence)?
            .ok_or_else(|| bad(format_args!("nothing follows {after}")))
    }

    /// `MoLangParser.parseArgs`, for the name `of`.
    ///
    /// A string is read as an argument on its own of a name outside `math.`: those are the game's
    /// functions (`q.has_aspect('sheared')`), which read as 0 here whatever they are given, so the string
    /// is never computed with.
    fn arguments(&mut self, of: &str) -> Result<Arguments> {
        let mut arguments = Arguments {
            count: 0,
            starts: [self.ops.len(); 4],
        };
        let math = of
            .split('.')
            .next()
            .is_some_and(|root| root.eq_ignore_ascii_case("math"));
        if self.eat(Symbol::BracketLeft)? && !self.eat(Symbol::BracketRight)? {
            loop {
                let start = if !math && self.peek()? == Token::Text {
                    self.peeked = None;
                    self.ops.push(Op::Number(0.0));
                    self.ops.len() - 1
                } else {
                    self.operand(ANYTHING, Token::Symbol(Symbol::BracketLeft))?
                };
                if let Some(slot) = arguments.starts.get_mut(arguments.count) {
                    *slot = start;
                }
                arguments.count += 1;
                if !self.eat(Symbol::Comma)? {
                    break;
                }
            }
            self.expect(Symbol::BracketRight)?;
        }
        Ok(arguments)
    }

    /// Replaces the steps from `start` on, which are a name's arguments, by `op`: no name that ends here
    /// reads its arguments.
    fn instead(&mut self, start: usize, op: Op) {
        self.ops.truncate(start);
        self.ops.push(op);
    }

    /// What `NameExpression` or `FuncCallExpression` evaluate to for the name `written`, whose arguments
    /// are the steps from `start` on: `MoLangEnvironment.getValue`, through the structs of
    /// `MoLangRuntime` as Cobblemon fills them.
    fn name(&mut self, written: &str, start: usize, arguments: &Arguments) -> Result<()> {
        // `fixNameShortcut`, and `String.split`, which drops the empty names a trailing dot makes.
        let mut name = std::mem::take(&mut self.name);
        name.clear();
        name.push_str(written.trim_end_matches('.'));
        name.make_ascii_lowercase();
        let head = name.find('.').unwrap_or(name.len());
        let long = match &name[..head] {
            "q" => Some("query"),
            "v" => Some("variable"),
            "t" => Some("temp"),
            "c" => Some("context"),
            _ => None,
        };
        if let Some(long) = long {
            name.replace_range(..head, long);
        }
        (self.note)(&name);
        let resolved = self.resolve(&name, start, arguments);
        self.name = name;
        resolved
    }

    fn resolve(&mut self, name: &str, start: usize, arguments: &Arguments) -> Result<()> {
        let mut parts = name.split('.');
        let root = parts.next().unwrap_or_default();
        let key = parts.next();
        let deeper = parts.next().is_some();
        if !matches!(
            root,
            "math" | "query" | "variable" | "temp" | "context" | "array"
        ) {
            // `MoLangEnvironment.getValue` without a struct of that name.
            self.instead(start, Op::Number(0.0));
            return Ok(());
        }
        // A struct asked for its value with no name left: `NoSuchElementException`. (`context` alone is 0
        // before a state's first evaluation and this afterwards.)
        let key = key.ok_or_else(|| bad(format_args!("`{name}` is a struct, not a number")))?;
        if root != "math"
            && let Some(slot) = self.slots.as_mut().and_then(|slots| slots(name))
        {
            self.instead(start, Op::Slot(slot));
            return Ok(());
        }
        let op = match (root, key) {
            // `QueryStruct.get`: a function that answers a number answers it whatever names follow.
            ("query", "anim_time") => Op::Read(Input::QueryAnimTime),
            // `VariableStruct.get`: likewise for a variable that holds a number.
            ("variable", "anim_time") => Op::Read(Input::VariableAnimTime),
            ("variable", "age_in_ticks") => Op::Read(Input::AgeInTicks),
            ("variable", "limb_swing") => Op::Read(Input::LimbSwing),
            ("variable", "limb_swing_amount") => Op::Read(Input::LimbSwingAmount),
            ("math", "pi") => Op::Number(PI),
            ("math", "die_roll" | "die_roll_integer") => {
                return Err(unsupported(format_args!("`{name}`")));
            }
            ("math", function) => match Callee::named(function) {
                Some(callee) => {
                    // With names after the function's, `QueryStruct.get` calls it without arguments.
                    let given = if deeper { 0 } else { arguments.count };
                    if given < callee.arity() {
                        return Err(bad(format_args!(
                            "`{name}` needs {} arguments, has {given}",
                            callee.arity()
                        )));
                    }
                    self.call(callee, start, arguments);
                    return Ok(());
                }
                // `QueryStruct.get` without a function of that name.
                None => Op::Number(0.0),
            },
            // The state's other functions, a variable nothing here assigns, `temp`, `context`, `array`.
            _ => Op::Number(0.0),
        };
        self.instead(start, op);
        Ok(())
    }

    /// The steps of `callee` over its arguments, which are the steps from `start` on.
    fn call(&mut self, callee: Callee, start: usize, arguments: &Arguments) {
        // The arguments past those the function reads; `starts` has the end of the last one read.
        if arguments.count > callee.arity() {
            self.ops.truncate(arguments.starts[callee.arity()]);
        }
        match callee {
            Callee::Unary(op) => self.unary(op, start),
            Callee::Binary(op) => self.binary(op, start, arguments.starts[1]),
            Callee::Ternary(op) => {
                let [_, second, third, _] = arguments.starts;
                let constant = match (
                    &self.ops[start..second],
                    &self.ops[second..third],
                    &self.ops[third..],
                ) {
                    ([Op::Number(a)], [Op::Number(b)], [Op::Number(c)]) => {
                        Some(ternary(op, *a, *b, *c))
                    }
                    _ => None,
                };
                match constant {
                    Some(value) => self.instead(start, Op::Number(value)),
                    None => self.ops.push(Op::Ternary(op)),
                }
            }
            Callee::Random | Callee::RandomInteger => {
                let seed = mix(self.seed.wrapping_add(self.randoms));
                self.randoms += 1;
                self.ops.push(match callee {
                    Callee::Random => Op::Random(seed),
                    _ => Op::RandomInteger(seed),
                });
            }
        }
    }

    /// `op` over the steps from `start` on; computed now if those are one number.
    fn unary(&mut self, op: Unary, start: usize) {
        match self.ops[start..] {
            [Op::Number(value)] => self.ops[start] = Op::Number(unary(op, value)),
            _ => self.ops.push(Op::Unary(op)),
        }
    }

    /// `op` over the steps from `left` to `right` and those from `right` on; computed now if both are
    /// one number.
    fn binary(&mut self, op: Binary, left: usize, right: usize) {
        match (&self.ops[left..right], &self.ops[right..]) {
            ([Op::Number(a)], [Op::Number(b)]) => {
                let value = binary(op, *a, *b);
                self.instead(left, Op::Number(value));
            }
            _ => self.ops.push(Op::Binary(op)),
        }
    }

    /// `TernaryParselet`, after the `?` of a condition whose steps start at `start`: `a ? b : c`,
    /// `a ? b`, `a ?: c` and, at the end of the text, `a ?`. Each part goes up to the next `?` or `:`.
    fn conditional(&mut self, start: usize) -> Result<()> {
        let then = self.ops.len();
        let (has_then, otherwise) = if self.eat(Symbol::Colon)? {
            (false, self.expression(CONDITIONAL)?)
        } else {
            let has_then = self.expression(CONDITIONAL)?.is_some();
            let otherwise = match self.eat(Symbol::Colon)? {
                true => self.expression(CONDITIONAL)?,
                false => None,
            };
            (has_then, otherwise)
        };
        let Some(&[Op::Number(condition)]) = self.ops.get(start..then) else {
            // Without its last part a ternary is 0 when its condition is false.
            if otherwise.is_none() {
                self.ops.push(Op::Number(0.0));
            }
            self.ops.push(match has_then {
                true => Op::Ternary(Ternary::Select),
                false => Op::Binary(Binary::Otherwise),
            });
            return Ok(());
        };
        // The condition is a number: only the part it picks stays.
        match (condition == 1.0, has_then, otherwise) {
            (true, true, _) => {
                self.ops.truncate(otherwise.unwrap_or(self.ops.len()));
                self.ops.drain(start..then);
            }
            (true, false, _) => self.instead(start, Op::Number(condition)),
            (false, _, Some(otherwise)) => {
                self.ops.drain(start..otherwise);
            }
            (false, _, None) => self.instead(start, Op::Number(0.0)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(source: &str) -> f64 {
        value_at(source, &Context::default())
    }

    /// `source`'s value with `context` and 0.25 for `variable.anim_time`.
    fn value_at(source: &str, context: &Context) -> f64 {
        Expression::parse(source)
            .unwrap_or_else(|e| panic!("{source}: {}", e.0))
            .eval_exact(&Inputs::new(context, 0.25))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn products_bind_tighter_than_sums_and_both_go_left_to_right() {
        assert_eq!(value("1 + 2 * 3"), 7.0);
        assert_eq!(value("2 * 3 + 1"), 7.0);
        assert_eq!(value("10 - 4 - 3"), 3.0);
        assert_eq!(value("16 / 4 / 2"), 2.0);
        assert_eq!(value("2 - 3 * 4 + 5"), -5.0);
        assert_eq!(value("(1 + 2) * 3"), 9.0);
        assert_eq!(value("2 * (3 + 4) * 5"), 70.0);
        assert_eq!(value("1 + 2 * 3 - 4 / 8"), 6.5);
    }

    #[test]
    fn prefix_operators_bind_tighter_than_any_infix_one() {
        assert_eq!(value("-2 * 3"), -6.0);
        assert_eq!(value("-(2 + 3) * 4"), -20.0);
        assert_eq!(value("2 * -3"), -6.0);
        assert_eq!(value("2 - -3"), 5.0);
        assert_eq!(value("--3"), 3.0);
        assert_eq!(value("+3"), 3.0);
        assert_eq!(value("2 * +3"), 6.0);
        assert_eq!(value("!0 + 1"), 2.0);
        assert_eq!(value("!1 == 0"), 1.0);
    }

    #[test]
    fn comparisons_bind_tighter_than_or_which_binds_tighter_than_and() {
        assert_eq!(value("3 > 2"), 1.0);
        assert_eq!(value("2 >= 2"), 1.0);
        assert_eq!(value("1 < 1"), 0.0);
        assert_eq!(value("1 <= 1"), 1.0);
        assert_eq!(value("2 == 2"), 1.0);
        assert_eq!(value("2 != 2"), 0.0);
        assert_eq!(value("1 + 1 == 2"), 1.0);
        // The six of them bind alike, left to right.
        assert_eq!(value("1 < 2 < 3"), 1.0);
        assert_eq!(value("3 > 2 > 1"), 0.0);
        assert_eq!(value("1 == 1 && 2 == 2"), 1.0);
        // `(1 || 0) && 0` and `0 && (0 || 1)`: not as C reads them.
        assert_eq!(value("1 || 0 && 0"), 0.0);
        assert_eq!(value("0 && 0 || 1"), 0.0);
        assert_eq!(value("0 || 3"), 1.0);
        assert_eq!(value("2 && 3"), 1.0);
        assert_eq!(value("2 && 0"), 0.0);
        assert_eq!(value("true + TRUE + false"), 2.0);
    }

    #[test]
    fn a_ternary_is_true_for_exactly_one_and_chains_to_the_left() {
        assert_eq!(value("1 ? 2 : 3"), 2.0);
        assert_eq!(value("0 ? 2 : 3"), 3.0);
        assert_eq!(value("2 ? 5 : 7"), 7.0);
        assert_eq!(value("0.5 ? 5 : 7"), 7.0);
        assert_eq!(value("3 > 2 ? 5 : 7"), 5.0);
        assert_eq!(value("1 ? 2 + 1 : 3 * 3"), 3.0);
        // `(1 ? 2 : 0) ? 3 : 4`, and 2 is not true.
        assert_eq!(value("1 ? 2 : 0 ? 3 : 4"), 4.0);
        // `(1 ? 0) ? 5 : 6`, the `: 7` left over.
        assert_eq!(value("1 ? 0 ? 5 : 6 : 7"), 6.0);
        assert_eq!(value("1 ? 2"), 2.0);
        assert_eq!(value("0 ? 2"), 0.0);
        assert_eq!(value("1 ?: 5"), 1.0);
        assert_eq!(value("3 ?: 5"), 5.0);
        assert_eq!(value("0 ? : 9"), 9.0);
        assert_eq!(value("1 ?"), 1.0);
        assert_eq!(value("!2"), 1.0);
        assert_eq!(value("!1"), 0.0);
        assert_eq!(value("!0"), 1.0);
    }

    #[test]
    fn a_ternary_on_the_context_computes_the_same_as_one_on_a_number() {
        let context = |anim_time| Context {
            anim_time,
            ..Context::default()
        };
        for (source, at_one, at_two) in [
            ("q.anim_time ? 5 : 7", 5.0, 7.0),
            ("q.anim_time ? 5", 5.0, 0.0),
            ("q.anim_time ?: 7", 1.0, 7.0),
            ("q.anim_time ?", 1.0, 0.0),
            (
                "q.anim_time == 2 ? q.anim_time * 10 : -q.anim_time",
                -1.0,
                20.0,
            ),
            ("q.anim_time > 1 && q.anim_time < 3", 0.0, 1.0),
            ("q.anim_time > 1 || q.anim_time < 0", 0.0, 1.0),
            ("!q.anim_time", 0.0, 1.0),
        ] {
            assert_eq!(value_at(source, &context(1.0)), at_one, "{source}");
            assert_eq!(value_at(source, &context(2.0)), at_two, "{source}");
        }
    }

    #[test]
    fn what_the_tokenizer_does_not_know_is_skipped_and_what_follows_an_expression_is_ignored() {
        assert_eq!(value(".5"), 5.0);
        assert_eq!(value("2*.5"), 10.0);
        assert_eq!(value("-.1+.1*3"), 2.0);
        assert_eq!(value("1."), 1.0);
        assert_eq!(value("1.5.2"), 1.5);
        assert_eq!(value("007"), 7.0);
        assert_eq!(value("1e3"), 1.0);
        assert_eq!(value("1.0E-4"), 1.0);
        assert_eq!(value("5 % 3"), 5.0);
        assert_eq!(value("1 2"), 1.0);
        assert_eq!(value("2 ) + 5"), 2.0);
        assert_eq!(value("2; 3"), 2.0);
        assert_eq!(value(" \t2\n*\r\n 3 "), 6.0);
        assert_eq!(value("1 \u{a0}+\u{2003} 2"), 3.0);
        assert_eq!(value("2 'never closed"), 2.0);
    }

    #[test]
    fn sine_and_cosine_take_degrees() {
        assert!(close(value("math.sin(90)"), 1.0));
        assert!(close(value("math.sin(-90)"), -1.0));
        assert!(close(value("math.cos(180)"), -1.0));
        assert!(close(value("math.cos(60)"), 0.5));
        // `Math.sin(30 * Math.PI / 180.0)`, not one half.
        assert_eq!(value("math.sin(30)"), (30.0 * PI / 180.0).sin());
        assert!(close(value("math.asin(1)"), 90.0));
        assert!(close(value("math.acos(0.5)"), 60.0));
        assert!(close(value("math.atan(1)"), 45.0));
        assert!(close(value("math.atan2(1, -1)"), 135.0));
        assert!(close(value("math.d2r(180)"), PI));
        assert!(close(value("math.r2d(math.pi)"), 180.0));
        let context = Context {
            anim_time: 0.5,
            ..Context::default()
        };
        assert!(close(
            value_at("2 + 3 * math.sin(q.anim_time * 90 * 2)", &context),
            5.0
        ));
    }

    #[test]
    fn every_function_of_the_library_computes_what_the_library_does() {
        for (source, expected) in [
            ("math.abs(-3.5)", 3.5),
            ("math.ceil(1.2)", 2.0),
            ("math.ceil(-1.5)", -1.0),
            ("math.floor(-1.5)", -2.0),
            ("math.floor(1.7)", 1.0),
            // `Math.floor` too.
            ("math.trunc(-1.5)", -2.0),
            ("math.trunc(1.7)", 1.0),
            ("math.round(2.5)", 3.0),
            ("math.round(-2.5)", -2.0),
            ("math.round(-0.5)", 0.0),
            ("math.round(2.4)", 2.0),
            ("math.round(-2.6)", -3.0),
            ("math.round(0.49999999999999994)", 0.0),
            // `min(c, max(a, b))`, whatever the order of the bounds.
            ("math.clamp(5, 0, 3)", 3.0),
            ("math.clamp(-5, 0, 3)", 0.0),
            ("math.clamp(2, 0, 3)", 2.0),
            ("math.clamp(1, 3, 0)", 0.0),
            // Java's `%`: the sign of the first.
            ("math.mod(7, 3)", 1.0),
            ("math.mod(-7, 3)", -1.0),
            ("math.mod(7, -3)", 1.0),
            ("math.mod(5.5, 2)", 1.5),
            ("math.pow(2, 10)", 1024.0),
            ("math.pow(9, 0.5)", 3.0),
            ("math.sqrt(16)", 4.0),
            ("math.exp(0)", 1.0),
            ("math.ln(1)", 0.0),
            ("math.min(1, 2)", 1.0),
            ("math.min(2, -1)", -1.0),
            ("math.max(1, 2)", 2.0),
            ("math.max(2, -1)", 2.0),
            ("math.lerp(0, 10, 0.25)", 2.5),
            ("math.lerp(4, 2, 0.5)", 3.0),
            // The third argument is kept within 0 and 1.
            ("math.lerp(0, 10, 2)", 10.0),
            ("math.lerp(0, 10, -1)", 0.0),
            ("math.lerp_rotate(10, 350, 0.5)", 0.0),
            ("math.lerp_rotate(10, 50, 0.25)", 20.0),
            ("math.sign(-3)", -1.0),
            ("math.sign(0)", 1.0),
            ("math.sign(2)", 1.0),
            ("math.copy_sign(3, -1)", -3.0),
            ("math.copy_sign(-3, 0)", 3.0),
            // `3 * value ^ 2 - 2 * value ^ 3` with Java's exclusive or.
            ("math.hermite_blend(2)", -5.0),
            ("math.hermite_blend(0.5)", 1.0),
            ("math.pi", PI),
            ("math.pi * 2", 2.0 * PI),
            ("MATH.PI", PI),
            // Arguments past those a function reads change nothing.
            ("math.abs(-2, 5)", 2.0),
            ("math.min(1, 2, 3)", 1.0),
            ("math.clamp(5, 0, 3, 9, 9)", 3.0),
            ("math.pi(1, 2)", PI),
            ("math.pi.x", PI),
            // Not in the library: 0, as is any function under `m.`, which is no shortcut.
            ("math.tan(45)", 0.0),
            ("math.tan", 0.0),
            ("m.sin(90)", 0.0),
        ] {
            assert_eq!(value(source), expected, "{source}");
        }
        assert!(close(value("math.exp(1)"), std::f64::consts::E));
        assert!(close(value("math.ln(10)"), std::f64::consts::LN_10));
        assert!(close(value("math.sqrt(2)"), std::f64::consts::SQRT_2));
        assert!(close(value("math.pow(2, 0.5)"), std::f64::consts::SQRT_2));
    }

    #[test]
    fn numbers_that_are_not_finite_come_out_as_in_java() {
        assert_eq!(value("1 / 0"), f64::INFINITY);
        assert_eq!(value("-1 / 0"), f64::NEG_INFINITY);
        assert!(value("0 / 0").is_nan());
        assert!(value("math.mod(1, 0)").is_nan());
        assert!(value("math.sqrt(-1)").is_nan());
        assert!(value("math.pow(-8, 1 / 3)").is_nan());
        assert_eq!(value("math.ln(0)"), f64::NEG_INFINITY);
        // `Math.min` and `Math.max` answer NaN for a NaN on either side; `Math.round` answers 0.
        assert!(value("math.min(1, 0 / 0)").is_nan());
        assert!(value("math.max(0 / 0, 1)").is_nan());
        assert!(value("math.clamp(0 / 0, 0, 1)").is_nan());
        assert_eq!(value("math.round(0 / 0)"), 0.0);
        assert_eq!(value("math.round(1 / 0)"), i64::MAX as f64);
        // A NaN is not 0, so it is true to `&&` and `||`, and not 1, so it is false to a ternary.
        assert_eq!(value("0 / 0 && 1"), 1.0);
        assert_eq!(value("0 / 0 ? 1 : 2"), 2.0);
        assert_eq!(value("0 / 0 == 0 / 0"), 0.0);
        assert_eq!(value("0 / 0 != 0 / 0"), 1.0);
        assert!(java_pow(1.0, f64::NAN).is_nan());
        assert!(java_pow(-1.0, f64::INFINITY).is_nan());
        assert_eq!(java_pow(f64::NAN, 0.0), 1.0);
        assert!(java_max(1.0, f64::NAN).is_nan());
        assert!(java_min(f64::NAN, 1.0).is_nan());
        assert!(java_max(-0.0, 0.0).is_sign_positive());
        assert!(java_min(0.0, -0.0).is_sign_negative());
    }

    #[test]
    fn names_are_read_in_lower_case_through_the_structs_cobblemon_fills() {
        let context = Context {
            anim_time: 1.5,
            age_in_ticks: 40.0,
            limb_swing: 3.0,
            limb_swing_amount: 0.75,
        };
        for (source, expected) in [
            ("q.anim_time", 1.5),
            ("query.anim_time", 1.5),
            ("Q.ANIM_TIME", 1.5),
            ("Query.Anim_Time * 2", 3.0),
            ("q.anim_time(7)", 1.5),
            ("q.anim_time.", 1.5),
            ("query.anim_time.whatever", 1.5),
            // The time within the animation, which `value_at` sets to 0.25.
            ("v.anim_time", 0.25),
            ("variable.anim_time", 0.25),
            ("variable.anim_time.x", 0.25),
            ("v.age_in_ticks", 40.0),
            ("variable.limb_swing", 3.0),
            ("V.Limb_Swing_Amount", 0.75),
            ("v.limb_swing * 2 + v.limb_swing_amount", 6.75),
            // These three are the state's variables, not its queries.
            ("q.age_in_ticks", 0.0),
            ("query.limb_swing", 0.0),
            ("q.limb_swing_amount", 0.0),
            // What nothing here knows is 0, with or without arguments.
            ("q.is_on_fire", 0.0),
            ("q.r.yaw_change(0)", 0.0),
            ("5 - q.r.velocity_y(3 + q.anim_time)", 5.0),
            ("v.speed", 0.0),
            ("variable.camera_rotation_x", 0.0),
            ("t.x", 0.0),
            ("temp.x", 0.0),
            ("c.other", 0.0),
            ("array.x", 0.0),
            ("somewhere", 0.0),
            ("some.where(1, 2)", 0.0),
            ("1 + some.where * 4", 1.0),
        ] {
            assert_eq!(value_at(source, &context), expected, "{source}");
        }
    }

    #[test]
    fn the_public_evaluation_is_the_exact_one_made_an_f32() {
        let context = Context {
            anim_time: 1.37,
            age_in_ticks: 27.4,
            limb_swing: 0.6,
            limb_swing_amount: 0.25,
        };
        let expression =
            Expression::parse("4 + 2.5 * math.sin(q.anim_time * 90 * 4) + v.anim_time").unwrap();
        let time = f64::from(1.37f32);
        let expected = 4.0 + 2.5 * (time * 90.0 * 4.0 * PI / 180.0).sin() + time;
        assert_eq!(expression.eval(&context), expected as f32);
        assert_eq!(expression.constant(), None);
        assert_eq!(Expression::number(2.5).eval(&context), 2.5);
        assert_eq!(Expression::number(2.5).constant(), Some(2.5));
        assert_eq!(
            Expression::parse("1 / 0").unwrap().eval(&context),
            f32::INFINITY
        );
    }

    #[test]
    fn what_does_not_depend_on_the_context_is_computed_once() {
        for (source, expected) in [
            ("7", 7.0),
            ("-7", -7.0),
            ("1 + 2 * 3", 7.0),
            ("math.sin(90) * 2 + math.clamp(5, 0, 3)", 5.0),
            ("math.pi", PI as f32),
            ("3 > 2 ? 10 : q.anim_time", 10.0),
            ("0 ? q.anim_time : 4", 4.0),
            ("1 ?: q.anim_time", 1.0),
            ("q.no_such_query(q.anim_time) + 2", 2.0),
            ("math.tan(q.anim_time)", 0.0),
            ("math.abs(-2, q.anim_time)", 2.0),
        ] {
            let expression = Expression::parse(source).unwrap();
            assert_eq!(expression.constant(), Some(expected), "{source}");
            assert!(matches!(expression.0, Compiled::Constant(_)), "{source}");
        }
        for source in [
            "q.anim_time",
            "v.anim_time * 0",
            "1 + v.age_in_ticks",
            "math.sin(q.anim_time)",
            "math.random(0, 1)",
            "math.random_integer(1, 6)",
            "q.anim_time ? 1 : 2",
        ] {
            assert_eq!(
                Expression::parse(source).unwrap().constant(),
                None,
                "{source}"
            );
        }
        // A number among what does depend on it is folded where the library's own order allows: the
        // product of the last two here, not of the first two there.
        let folded = |source: &str| match Expression::parse(source).unwrap().0 {
            Compiled::Steps(ops) => ops.len(),
            Compiled::Constant(_) => 0,
        };
        assert_eq!(folded("q.anim_time * (90 * 4)"), 3);
        assert_eq!(folded("q.anim_time * 90 * 4"), 5);
    }

    #[test]
    fn random_numbers_stay_in_range_and_depend_on_the_moment_and_on_who_asks() {
        let expression = Expression::parse("math.random(-2, 3)").unwrap();
        let whole = Expression::parse("math.random_integer(1, 6)").unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let mut faces = std::collections::BTreeSet::new();
        for tick in 0..2000 {
            let context = Context {
                anim_time: tick as f32 / 20.0,
                age_in_ticks: tick as f32,
                ..Context::default()
            };
            let rolled = expression.eval(&context);
            assert!((-2.0..3.0).contains(&rolled), "{rolled}");
            assert_eq!(rolled, expression.eval(&context));
            seen.insert(rolled.to_bits());
            let face = whole.eval(&context);
            assert_eq!(face, face.round());
            assert!((1.0..=6.0).contains(&face), "{face}");
            faces.insert(face as i32);
        }
        assert!(seen.len() > 1990, "{}", seen.len());
        assert_eq!(faces.into_iter().collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);
        // Two in one expression, and one text compiled for two places, do not roll alike.
        let context = Context {
            anim_time: 3.5,
            ..Context::default()
        };
        assert_ne!(
            value_at("math.random(0, 1) - math.random(0, 1)", &context),
            0.0
        );
        let at = |seed| {
            Expression::compile("math.random(0, 1)", seed, &mut |_| {})
                .unwrap()
                .eval(&context)
        };
        assert_ne!(at(1), at(2));
        assert_eq!(at(1), at(1));
        // Bounds the wrong way round and bounds that are not numbers are the library's own business.
        assert!((1.0..=5.0).contains(&value_at("math.random(5, 1)", &context)));
        assert!(value_at("math.random(0 / 0, 1)", &context).is_nan());
        assert_eq!(value_at("math.random_integer(0 / 0, 0 / 0)", &context), 0.0);
    }

    #[test]
    fn names_are_reported_as_the_library_looks_them_up() {
        let mut names = Vec::new();
        Expression::parse_noting(
            "Q.Anim_Time + math.SIN(v.speed.) * q.r.dive(0) + t.a + c.b + m.cos(1) + lone",
            &mut |name| names.push(name.to_owned()),
        )
        .unwrap();
        assert_eq!(
            names,
            [
                "query.anim_time",
                "variable.speed",
                "math.sin",
                "query.r.dive",
                "temp.a",
                "context.b",
                "m.cos",
                "lone"
            ]
        );
    }

    #[test]
    fn what_the_library_refuses_or_throws_on_is_an_error() {
        for source in [
            "",
            "   ",
            "%",
            "(1",
            "()",
            ")",
            "1 + * 2",
            "* 2",
            ", 1",
            "1 +",
            "2 *",
            "-",
            "!",
            "1 ? 2 : 3 +",
            "1 &&",
            "math.sin()",
            "math.sin",
            "math.sin(",
            "math.sin(1",
            "math.sin(1,)",
            "math.sin(,1)",
            "math.sin(1 2)",
            "math.random()",
            "math.random(1)",
            "math.clamp(1, 2)",
            "math.sin.x(30)",
            "math",
            "query",
            "q",
            "variable",
            "v.",
            "temp",
            "array",
        ] {
            assert!(Expression::parse(source).is_err(), "{source:?}");
        }
    }

    #[test]
    fn a_string_is_read_as_an_argument_of_the_game_s_functions_which_are_0() {
        // A sheep that is not sheared keeps its wool, a Hippowdon on no sand keeps its usual poses.
        assert_eq!(value("q.has_aspect('sheared') == false"), 1.0);
        assert_eq!(value("q.has_aspect('sheared')"), 0.0);
        assert_eq!(
            value("!q.is_standing_on_blocks(2, 'minecraft:sand', 'minecraft:red_sand')"),
            1.0
        );
        // Whatever the string holds, and inside another such function.
        assert_eq!(value(r"q.f('a,b)', 'it\'s') ? 2 : 3"), 3.0);
        assert_eq!(value("q.f(q.g('x'), 1) + 4"), 4.0);
        assert_eq!(value("Q.Has_Aspect('x') + 1"), 1.0);
    }

    #[test]
    fn what_no_animation_uses_is_an_error_rather_than_a_guess() {
        for source in [
            "'text'",
            "q.anim_time == 'text'",
            "math.sin('text')",
            "MATH.sin('text')",
            "q.has_aspect('a' + 1)",
            "q.has_aspect(('a'))",
            "q.has_aspect(-'a')",
            "v.x = 4",
            "t.x = 4; t.x",
            "return 3",
            "RETURN 3",
            "{ 1; }",
            "1 ? { 2; }",
            "loop(3, 1)",
            "for_each(v.x, q.list, 1)",
            "break",
            "continue",
            "this",
            "q.list[0]",
            "v.x ?? 2",
            "this -> v.x",
            "math.die_roll(1, 0, 6)",
            "math.die_roll_integer(1, 0, 6)",
            "1 + é",
            "é",
            "q.animé",
            "1٣",
        ] {
            assert!(Expression::parse(source).is_err(), "{source:?}");
        }
        let error = Expression::parse("v.x = 4").unwrap_err();
        assert_eq!(error.0, "molang: `=` is not among what this evaluates");
    }

    #[test]
    fn an_expression_too_deep_or_too_wide_is_an_error_and_a_long_one_is_not() {
        let nested = |depth: usize| format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        // The number inside is one level too.
        assert_eq!(value(&nested(MAX_NESTING - 1)), 1.0);
        assert!(Expression::parse(&nested(MAX_NESTING)).is_err());
        assert!(Expression::parse(&"-".repeat(100_000)).is_err());
        assert!(Expression::parse(&"(".repeat(100_000)).is_err());
        // Each `(` here keeps one more number waiting.
        let waiting = |depth: usize| {
            let mut source = String::new();
            for _ in 0..depth {
                source.push_str("q.anim_time + (");
            }
            source.push_str("q.anim_time");
            source.push_str(&")".repeat(depth));
            source
        };
        let context = Context {
            anim_time: 1.0,
            ..Context::default()
        };
        assert_eq!(value_at(&waiting(STACK - 1), &context), STACK as f64);
        assert!(Expression::parse(&waiting(STACK)).is_err());
        // A chain to the left keeps two at most, however long.
        let chain = vec!["q.anim_time"; 50_000].join(" + ");
        assert_eq!(value_at(&chain, &context), 50_000.0);
    }

    #[test]
    fn no_text_makes_the_parser_panic() {
        const PIECES: [&str; 40] = [
            "1",
            "2.5",
            ".",
            "q.anim_time",
            "v.x",
            "math.sin",
            "math.clamp",
            "math.random",
            "math.pi",
            "(",
            ")",
            ",",
            "+",
            "-",
            "*",
            "/",
            "?",
            ":",
            "!",
            "==",
            "!=",
            "<",
            ">=",
            "&&",
            "||",
            "??",
            "->",
            "=",
            "[",
            "]",
            "{",
            "}",
            ";",
            "'",
            " ",
            "true",
            "return",
            "loop",
            "é",
            "\u{1F600}",
        ];
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let context = Context {
            anim_time: 0.7,
            ..Context::default()
        };
        let mut compiled = 0;
        for _ in 0..20_000 {
            let mut source = String::new();
            state = mix(state);
            for piece in 0..1 + state % 12 {
                state = mix(state ^ piece);
                source.push_str(PIECES[(state % PIECES.len() as u64) as usize]);
            }
            if let Ok(expression) = Expression::parse(&source) {
                expression.eval(&context);
                compiled += 1;
            }
        }
        assert!(compiled > 1000, "{compiled}");
    }

    /// Texts given to the library itself, `com.bedrockk.molang` as Cobblemon 1.8.1's jar carries it, each
    /// under the three [`ORACLE_CONTEXTS`]. `tests/fixtures/anim_molang_oracle.txt` has what it answered,
    /// a line for each text and context in this order: the `double`'s bits in hexadecimal, then the
    /// number as Java prints it; or `error` where it threw.
    ///
    /// The recipe, of which nothing is kept in this repository: the `com/bedrockk` classes out of
    /// `Cobblemon-fabric-1.8.1+1.21.1.jar`, and a few lines of Java against them and Gson that, for each
    /// line of their input and each context, make a `MoLangRuntime` as `PosableState` and
    /// `BedrockAnimation.run` set one up (`environment.query.addFunction("anim_time", ...)` answering
    /// the context's first number; `environment.setSimpleVariable` of `anim_time`, `age_in_ticks`,
    /// `limb_swing` and `limb_swing_amount` to its other four, each a `float` made a `double`), and
    /// print `runtime.execute(MoLang.createParser(line).parseExpression()).asDouble()`, or `error` for
    /// a null expression and for anything thrown.
    const ORACLE_SOURCES: [&str; 225] = [
        "1 + 2 * 3",
        "2 * 3 + 1",
        "10 - 4 - 3",
        "16 / 4 / 2",
        "2 - 3 * 4 + 5",
        "(1 + 2) * 3",
        "2 * (3 + 4) * 5",
        "1 + 2 * 3 - 4 / 8",
        "-2 * 3",
        "-(2 + 3) * 4",
        "2 * -3",
        "2 - -3",
        "--3",
        "+3",
        "2 * +3",
        "!0 + 1",
        "!1 == 0",
        "!2",
        "!0.5",
        "3 > 2",
        "2 >= 2",
        "1 < 1",
        "1 <= 1",
        "2 == 2",
        "2 != 2",
        "1 + 1 == 2",
        "1 < 2 < 3",
        "3 > 2 > 1",
        "1 == 1 && 2 == 2",
        "1 || 0 && 0",
        "0 && 0 || 1",
        "0 || 3",
        "2 && 3",
        "2 && 0",
        "true + TRUE + false",
        "1 ? 2 : 3",
        "0 ? 2 : 3",
        "2 ? 5 : 7",
        "0.5 ? 5 : 7",
        "3 > 2 ? 5 : 7",
        "1 ? 2 + 1 : 3 * 3",
        "1 ? 2 : 0 ? 3 : 4",
        "1 ? 0 ? 5 : 6 : 7",
        "1 ? 2",
        "0 ? 2",
        "1 ?: 5",
        "3 ?: 5",
        "0 ? : 9",
        "1 ?",
        "q.anim_time > 1 ? q.anim_time * 10 : -q.anim_time",
        "q.anim_time ? 5 : 7",
        "q.anim_time ?: 7",
        "v.anim_time < 1 && q.anim_time > 1 || v.limb_swing > 3",
        ".5",
        "2*.5",
        "-.1+.1*3",
        "1.",
        "1.5.2",
        "007",
        "1e3",
        "1.0E-4",
        "5 % 3",
        "1 2",
        "2 ) + 5",
        "2; 3",
        "\t2 *\t3 ",
        "0.1 + 0.2",
        "1 / 3",
        "1 / 0",
        "-1 / 0",
        "0 / 0",
        "0 / 0 && 1",
        "0 / 0 ? 1 : 2",
        "0 / 0 == 0 / 0",
        "q.anim_time",
        "query.anim_time",
        "Q.ANIM_TIME",
        "Query.Anim_Time * 2",
        "q.anim_time(7)",
        "q.anim_time.",
        "query.anim_time.whatever",
        "v.anim_time",
        "variable.anim_time",
        "variable.anim_time.x",
        "v.age_in_ticks",
        "variable.limb_swing",
        "V.Limb_Swing_Amount",
        "v.limb_swing * 2 + v.limb_swing_amount",
        "q.age_in_ticks",
        "query.limb_swing",
        "q.limb_swing_amount",
        "q.is_on_fire",
        "q.r.yaw_change(0)",
        "5 - q.r.velocity_y(3 + q.anim_time)",
        "v.speed",
        "t.x",
        "temp.x",
        "c.other",
        "array.x",
        "somewhere",
        "some.where(1, 2)",
        "1 + some.where * 4",
        "NaN - 2 * math.cos(q.anim_time * 90)",
        "math.sin(90)",
        "math.sin(30)",
        "math.sin(-90)",
        "math.sin(180)",
        "math.sin(q.anim_time * 90 * 4)",
        "math.cos(180)",
        "math.cos(60)",
        "math.cos(v.age_in_ticks * 7.5 + 20)",
        "2 + 3 * math.sin(q.anim_time * 90 * 2 - 45) + math.cos(q.anim_time * 360) * 0.5",
        "math.asin(1)",
        "math.asin(0.3)",
        "math.acos(0.5)",
        "math.atan(1)",
        "math.atan(q.anim_time)",
        "math.atan2(1, -1)",
        "math.atan2(q.anim_time, 3)",
        "math.d2r(180)",
        "math.r2d(1)",
        "math.abs(-3.5)",
        "math.ceil(1.2)",
        "math.ceil(-1.5)",
        "math.floor(-1.5)",
        "math.floor(1.7)",
        "math.trunc(-1.5)",
        "math.trunc(1.7)",
        "math.round(2.5)",
        "math.round(-2.5)",
        "math.round(-0.5)",
        "math.round(2.4)",
        "math.round(-2.6)",
        "math.round(0.49999999999999994)",
        "math.round(q.anim_time * 3)",
        "math.round(0 / 0)",
        "math.round(1 / 0)",
        "math.clamp(5, 0, 3)",
        "math.clamp(-5, 0, 3)",
        "math.clamp(1, 3, 0)",
        "math.clamp(math.sin(q.anim_time * 90) * 2, -1, 1)",
        "math.clamp(0 / 0, 0, 1)",
        "math.mod(7, 3)",
        "math.mod(-7, 3)",
        "math.mod(7, -3)",
        "math.mod(5.5, 2)",
        "math.mod(q.anim_time * 2, 1) * 360",
        "math.mod(1, 0)",
        "math.pow(2, 10)",
        "math.pow(2, 0.5)",
        "math.pow(-8, 1 / 3)",
        "math.pow(q.anim_time, 2)",
        "math.pow(1, 0 / 0)",
        "math.pow(-1, 1 / 0)",
        "math.pow(0 / 0, 0)",
        "math.sqrt(2)",
        "math.sqrt(-1)",
        "math.sqrt(math.sin(q.anim_time * 90) + 1)",
        "math.exp(1)",
        "math.exp(-q.anim_time)",
        "math.ln(10)",
        "math.ln(0)",
        "math.min(1, 2)",
        "math.min(2, -1)",
        "math.min(1, 0 / 0)",
        "math.min(0, -0)",
        "math.max(1, 2)",
        "math.max(0 / 0, 1)",
        "math.max(q.anim_time, 5)",
        "math.lerp(0, 10, 0.25)",
        "math.lerp(4, 2, 0.5)",
        "math.lerp(0, 10, 2)",
        "math.lerp(0, 10, -1)",
        "math.lerp(1, 3, q.anim_time)",
        "math.lerp_rotate(10, 350, 0.5)",
        "math.lerp_rotate(10, 50, 0.25)",
        "math.lerp_rotate(350, 10, 0.25)",
        "math.lerp_rotate(-170, 170, q.anim_time)",
        "math.sign(-3)",
        "math.sign(0)",
        "math.sign(0 / 0)",
        "math.copy_sign(3, -1)",
        "math.copy_sign(-3, 0)",
        "math.hermite_blend(2)",
        "math.hermite_blend(0.5)",
        "math.hermite_blend(q.anim_time)",
        "math.pi",
        "math.pi * 2",
        "MATH.PI",
        "math.pi(1, 2)",
        "math.pi.x",
        "math.abs(-2, 5)",
        "math.min(1, 2, 3)",
        "math.clamp(5, 0, 3, 9, 9)",
        "math.tan(45)",
        "math.tan",
        "m.sin(90)",
        "",
        "%",
        "(1",
        "()",
        ")",
        "1 + * 2",
        "* 2",
        "1 +",
        "2 *",
        "-",
        "!",
        "math.sin()",
        "math.sin",
        "math.sin(1",
        "math.sin(1,)",
        "math.sin(,1)",
        "math.sin(1 2)",
        "math.random()",
        "math.random(1)",
        "math.clamp(1, 2)",
        "math.sin.x(30)",
        "math",
        "query",
        "q",
        "variable",
        "v.",
        "temp",
        "array",
    ];

    /// `query.anim_time`, then `variable.anim_time`, `age_in_ticks`, `limb_swing`, `limb_swing_amount`.
    const ORACLE_CONTEXTS: [[f32; 5]; 3] = [
        [0.0; 5],
        [1.37, 0.37, 27.4, 0.6, 0.25],
        [12.5, 2.5, 250.0, 3.25, 1.0],
    ];

    #[test]
    fn the_library_itself_answers_the_same() {
        let mut answers = include_str!("../tests/fixtures/anim_molang_oracle.txt").lines();
        let (mut numbers, mut errors) = (0, 0);
        for source in ORACLE_SOURCES {
            let compiled = Expression::parse(source);
            for [query, variable, age_in_ticks, limb_swing, limb_swing_amount] in ORACLE_CONTEXTS {
                let answer = answers.next().expect("an answer for each text and context");
                let context = Context {
                    anim_time: query,
                    age_in_ticks,
                    limb_swing,
                    limb_swing_amount,
                };
                let bits = answer.split(' ').next().unwrap_or_default();
                let expression = match &compiled {
                    Err(_) if bits == "error" => {
                        errors += 1;
                        continue;
                    }
                    Ok(expression) if bits != "error" => expression,
                    _ => panic!("{source:?}: {compiled:?}, the library {answer}"),
                };
                let expected = f64::from_bits(u64::from_str_radix(bits, 16).expect(answer));
                let found = expression.eval_exact(&Inputs::new(&context, f64::from(variable)));
                // The same to the last bit, but for the bit two libraries' sines may differ by.
                let allowed = 4.0 * f64::EPSILON * expected.abs().max(1.0);
                assert!(
                    found.to_bits() == expected.to_bits()
                        || (found.is_nan() && expected.is_nan())
                        || ((found - expected).abs() <= allowed
                            && found.is_sign_negative() == expected.is_sign_negative()),
                    "{source:?} with {context:?} and {variable}: {found:?}, the library {expected:?}"
                );
                numbers += 1;
            }
        }
        assert_eq!(answers.next(), None);
        assert_eq!((numbers, errors), (591, 84));
    }
}
