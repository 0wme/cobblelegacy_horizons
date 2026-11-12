// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! CobbleLegacy's side of a Cobblemon 1.8.1 server (Minecraft 1.21.1, Fabric): what that server sends on its
//! plugin channels, read here so the Horizon client can show it. Nothing of this is in vanilla, so there is
//! no vanilla reference; the reference is Cobblemon 1.8.1 itself, whose layouts these files follow.
pub mod animation;
pub mod battle;
pub mod coded_posers;
pub mod geo;
mod json;
pub mod molang;
pub mod moves;
pub mod party;
pub mod pose;
pub mod poser;
pub mod resolver;
pub mod snowstorm;
pub mod wire;
