//! Page identity and geometry.

use crate::Result;
use crate::error::Error;

/// Stable identity for a page, unaffected by reordering, insertion, or removal.
///
/// Page indices describe position and change constantly; a `PageId` does not.
/// Mutation APIs therefore address pages by identity and use indices only to
/// express where an insertion lands.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PageId(u64);

impl PageId {
    /// Wrap a raw identifier. Prefer [`PageIdAllocator`] over constructing these directly.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Unwrap the raw identifier, for in-memory interchange with code that cannot name the `PageId` type
    /// (for example a widget id or a hash map key expressed as a bare `u64`).
    ///
    /// The returned value must never be written to disk or carried across a save-and-reopen: a `PageId`
    /// is unique only within one document in one process, and the PDF format has nowhere to persist it.
    /// See `docs/architecture/contracts.md`, "never persist a `PageId`".
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "page#{}", self.0)
    }
}

/// Hands out identifiers that are unique within one document.
#[derive(Debug, Default)]
pub struct PageIdAllocator {
    next: u64,
}

impl PageIdAllocator {
    /// Produce an identifier that this allocator has never produced before.
    pub fn allocate(&mut self) -> PageId {
        let id = PageId::new(self.next);
        self.next += 1;
        id
    }
}

/// Page rotation, in quarter turns clockwise. PDF permits no other values.
///
/// `Hash` is derived so that [`crate::render::RenderRequest`] can serve as a
/// cache key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Rotation {
    /// No rotation.
    #[default]
    None,
    /// 90 degrees clockwise.
    Quarter,
    /// 180 degrees.
    Half,
    /// 270 degrees clockwise.
    ThreeQuarter,
}

impl Rotation {
    /// Clockwise rotation in degrees: 0, 90, 180, or 270.
    pub const fn degrees(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Quarter => 90,
            Self::Half => 180,
            Self::ThreeQuarter => 270,
        }
    }

    /// Interpret a degree value, accepting any multiple of 90 including negatives.
    ///
    /// Returns [`Error::Unsupported`] for values that are not multiples of 90.
    pub fn from_degrees(degrees: i32) -> Result<Self> {
        if degrees % 90 != 0 {
            return Err(Error::Unsupported(format!("rotation of {degrees} degrees is not a multiple of 90")));
        }
        let quarters = degrees.div_euclid(90).rem_euclid(4);
        Ok(match quarters {
            0 => Self::None,
            1 => Self::Quarter,
            2 => Self::Half,
            _ => Self::ThreeQuarter,
        })
    }

    /// Compose two rotations, wrapping at a full turn.
    pub fn rotated_by(self, other: Self) -> Self {
        let total = i32::from(self.degrees()) + i32::from(other.degrees());
        Self::from_degrees(total).unwrap_or(Self::None)
    }

    /// Whether this rotation exchanges width and height.
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Quarter | Self::ThreeQuarter)
    }
}

/// Page dimensions in PDF points, where one point is 1/72 inch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageSize {
    /// Width in points, before any rotation is applied.
    pub width_pt: f32,
    /// Height in points, before any rotation is applied.
    pub height_pt: f32,
}

impl PageSize {
    /// A4 in points, the most common fixture size.
    pub const A4: Self = Self {
        width_pt: 595.0,
        height_pt: 842.0,
    };

    /// US Letter in points.
    pub const LETTER: Self = Self {
        width_pt: 612.0,
        height_pt: 792.0,
    };

    /// Construct a size from explicit dimensions.
    pub const fn new(width_pt: f32, height_pt: f32) -> Self {
        Self { width_pt, height_pt }
    }
}

/// Everything the UI needs to know about a page without reading its content.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageInfo {
    /// Stable identity.
    pub id: PageId,
    /// Media box dimensions, before rotation.
    pub size: PageSize,
    /// Rotation recorded on the page.
    pub rotation: Rotation,
}

impl PageInfo {
    /// Dimensions as displayed, with the page's rotation applied.
    pub const fn display_size(&self) -> PageSize {
        if self.rotation.swaps_axes() {
            PageSize::new(self.size.height_pt, self.size.width_pt)
        } else {
            self.size
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_distinct_identifiers() {
        let mut allocator = PageIdAllocator::default();
        let first = allocator.allocate();
        let second = allocator.allocate();
        assert_ne!(first, second);
    }

    #[test]
    fn accepts_negative_and_overlarge_degrees() {
        assert_eq!(Rotation::from_degrees(-90).unwrap(), Rotation::ThreeQuarter);
        assert_eq!(Rotation::from_degrees(450).unwrap(), Rotation::Quarter);
        assert_eq!(Rotation::from_degrees(0).unwrap(), Rotation::None);
    }

    #[test]
    fn rejects_degrees_that_are_not_quarter_turns() {
        assert!(Rotation::from_degrees(45).is_err());
    }

    #[test]
    fn composes_rotations_with_wraparound() {
        assert_eq!(Rotation::ThreeQuarter.rotated_by(Rotation::Half), Rotation::Quarter);
    }

    #[test]
    fn quarter_turns_swap_display_dimensions() {
        let page = PageInfo {
            id: PageId::new(0),
            size: PageSize::A4,
            rotation: Rotation::Quarter,
        };
        assert_eq!(page.display_size(), PageSize::new(842.0, 595.0));
    }

    #[test]
    fn half_turns_preserve_display_dimensions() {
        let page = PageInfo {
            id: PageId::new(0),
            size: PageSize::A4,
            rotation: Rotation::Half,
        };
        assert_eq!(page.display_size(), PageSize::A4);
    }
}
