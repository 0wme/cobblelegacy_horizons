// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What animations hand to whoever draws: changes to a part's pose, in the space [`crate::geo`] builds
//! parts in (vanilla `ModelPart`'s: pixels, Y down, radians).

/// A part's pose as vanilla's `ModelPart` holds it: x, y, z (pixels, Y down), xRot, yRot, zRot (radians),
/// xScale, yScale, zScale.
pub type PartPose = [f32; 9];

/// What an animation does to a part at one moment, on top of its pose so far. Already in part space: a
/// Bedrock animation's upward `position` is a downward one here (`y` negated), its `rotation` degrees are
/// radians with their signs kept, as Cobblemon applies them (`BedrockAnimation.run`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Delta {
    /// Added to x, y, z.
    pub position: [f32; 3],
    /// Added to xRot, yRot, zRot.
    pub rotation: [f32; 3],
    /// Multiplies xScale, yScale, zScale.
    pub scale: [f32; 3],
}

impl Delta {
    /// Changes nothing.
    pub const NONE: Self = Self {
        position: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0; 3],
    };

    pub fn apply(&self, pose: &mut PartPose) {
        for i in 0..3 {
            pose[i] += self.position[i];
            pose[3 + i] += self.rotation[i];
            pose[6 + i] *= self.scale[i];
        }
    }

    /// The change at an `intensity` from 0 (none of it) to 1 (all of it), as Cobblemon weakens what
    /// fades in or out (`BedrockAnimation.run`, `ModelPartTransformation.apply`): the move and the
    /// turn by that much, the scale by that much of what it is away from 1. A scale of nothing on
    /// every axis, which hides a part, is whole from half the intensity on.
    pub fn at(self, intensity: f32) -> Self {
        if intensity == 1.0 {
            return self;
        }
        let hides = self.scale == [0.0; 3] && intensity >= 0.5;
        Self {
            position: self.position.map(|moved| moved * intensity),
            rotation: self.rotation.map(|turned| turned * intensity),
            scale: if hides {
                self.scale
            } else {
                self.scale.map(|factor| 1.0 + (factor - 1.0) * intensity)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_adds_position_and_rotation_and_multiplies_scale() {
        let mut pose: PartPose = [1.0, 2.0, 3.0, 0.1, 0.2, 0.3, 1.0, 2.0, 0.5];
        Delta {
            position: [1.0, -2.0, 0.5],
            rotation: [0.1, 0.0, -0.3],
            scale: [2.0, 0.5, 1.0],
        }
        .apply(&mut pose);
        assert_eq!(pose, [2.0, 0.0, 3.5, 0.2, 0.2, 0.0, 2.0, 1.0, 0.5]);
        let before = pose;
        Delta::NONE.apply(&mut pose);
        assert_eq!(pose, before);
    }

    #[test]
    fn a_delta_weakens_with_the_intensity() {
        let delta = Delta {
            position: [2.0, -4.0, 0.0],
            rotation: [1.0, 0.0, -0.5],
            scale: [2.0, 0.5, 1.0],
        };
        assert_eq!(delta.at(1.0), delta);
        assert_eq!(delta.at(0.0), Delta::NONE);
        assert_eq!(
            delta.at(0.5),
            Delta {
                position: [1.0, -2.0, 0.0],
                rotation: [0.5, 0.0, -0.25],
                scale: [1.5, 0.75, 1.0],
            }
        );
        // A part scaled to nothing is hidden from half the way in, and shrinks before that.
        let hidden = Delta {
            scale: [0.0; 3],
            ..Delta::NONE
        };
        assert_eq!(hidden.at(0.5).scale, [0.0; 3]);
        assert_eq!(hidden.at(0.25).scale, [0.75; 3]);
    }
}
