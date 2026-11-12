//! Models that bring their own animation (CobbleLegacy's fork).
//!
//! Vanilla's mobs are posed by `anim`'s families, each written for one model's part names. A Pokémon's
//! model comes from Cobblemon's files with its own bones and its own animations, so its type names an
//! [`OwnAnimation`] instead ([`Anim::Own`](super::Anim::Own)): whoever built the model registers it here
//! and gets the number the type carries. Nothing is registered, and nothing here runs, without such a
//! model.
use super::Mob;
use super::anim::{Poses, State};
use std::sync::{Arc, PoisonError, RwLock};

/// Poses a model's parts for one draw, in place of vanilla's `setupAnim`: `poses` holds the model at rest.
pub trait OwnAnimation: Send + Sync {
    fn setup(&self, mob: &Mob, state: &State, poses: &mut Poses);
}

/// The number of no animation: the model stays at rest.
pub const NONE: u32 = u32::MAX;

/// Never shrinks: a type keeps its number for as long as mobs of it may be drawn.
static ANIMATIONS: RwLock<Vec<Arc<dyn OwnAnimation>>> = RwLock::new(Vec::new());

/// Keeps `animation` and gives the number to put in the type's `Anim::Own`.
pub fn register(animation: Arc<dyn OwnAnimation>) -> u32 {
    let mut animations = ANIMATIONS.write().unwrap_or_else(PoisonError::into_inner);
    animations.push(animation);
    (animations.len() - 1) as u32
}

pub(super) fn setup(animation: u32, mob: &Mob, state: &State, poses: &mut Poses) {
    // The read guard, not a clone of the Arc: one uncontended lock per mob drawn.
    let animations = ANIMATIONS.read().unwrap_or_else(PoisonError::into_inner);
    if let Some(animation) = animations.get(animation as usize) {
        animation.setup(mob, state, poses);
    }
}
