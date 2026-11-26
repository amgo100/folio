//! Fragments and regions: what layout produces and the space it fills (SPEC
//! §1.5, §3.3, §7.1).

use ecow::EcoVec;

use crate::draw::Shape;
use crate::geom::{Abs, Point, Rect, Size};
use crate::text::{GlyphRun, Location, SourceRef};

/// A positioned box produced by layout. Pages and slides are fragments.
///
/// Besides its content, a fragment carries what the page builder needs to
/// place it in a flow: its baselines, so that surrounding text can snap to
/// the baseline grid, and its keep and break constraints across its lower
/// boundary (SPEC §3.3).
///
/// Like its glyphs and shapes, a fragment names the source node it is the
/// box of, so the span map leads from any box on a page back to source
/// (SPEC §1.5, §12.2).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Fragment {
    /// The size of the box.
    pub size: Size,
    /// The source node whose box this is: the element of a block, or
    /// [`SourceRef::DETACHED`] for a box that no node makes.
    pub source: SourceRef,
    /// Which instance of the element made from `source` the box is: with
    /// `source`, the instance's [`Location`]. `0` for a box that no node
    /// makes.
    pub instance: u32,
    /// The content, positioned relative to the top-left corner, in painting
    /// order.
    pub items: EcoVec<(Point, FragmentItem)>,
    /// The distance from the top to the first baseline. A fragment without
    /// lines of text has its baselines at its bottom edge.
    pub first_baseline: Abs,
    /// The distance from the top to the last baseline.
    pub last_baseline: Abs,
    /// Whether the page builder must keep this fragment on the same page or
    /// in the same region as the next block.
    pub keep_with_next: bool,
    /// The cost of a break directly after this fragment: positive values
    /// discourage the break, negative values encourage it, and zero is
    /// neutral. Keeps and break penalties are costs, not hard rules (SPEC
    /// §7.2).
    pub break_penalty_after: i32,
}

impl Fragment {
    /// An empty fragment of a size, with its baselines at the bottom edge,
    /// no keep or break constraints, and no source node.
    pub fn new(size: Size) -> Self {
        Self {
            size,
            source: SourceRef::DETACHED,
            instance: 0,
            items: EcoVec::new(),
            first_baseline: size.height,
            last_baseline: size.height,
            keep_with_next: false,
            break_penalty_after: 0,
        }
    }

    /// The same fragment as the box of a source node, without saying
    /// which instance of it ([`Location::UNKNOWN_INSTANCE`]), as a
    /// component given only a [`SourceRef`] can; [`Self::with_location`]
    /// names an instance.
    pub fn with_source(mut self, source: SourceRef) -> Self {
        self.source = source;
        self.instance = if source.is_detached() {
            0
        } else {
            Location::UNKNOWN_INSTANCE
        };
        self
    }

    /// The same fragment as the box of an element instance.
    pub fn with_location(mut self, location: Location) -> Self {
        self.source = location.source;
        self.instance = location.instance;
        self
    }

    /// The location of the element instance whose box this is, or
    /// [`Location::DETACHED`] for a box that no node makes.
    pub fn location(&self) -> Location {
        Location {
            source: self.source,
            instance: self.instance,
        }
    }

    /// Adds an item on top of the existing ones.
    pub fn push(&mut self, pos: Point, item: FragmentItem) {
        self.items.push((pos, item));
    }

    /// Adds another fragment as a group on top of the existing items.
    pub fn push_fragment(&mut self, pos: Point, fragment: Fragment) {
        self.push(pos, FragmentItem::Group(fragment));
    }
}

/// One item of a fragment.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum FragmentItem {
    /// A nested fragment, positioned by its top-left corner.
    Group(Fragment),
    /// A run of glyphs, positioned by the start of its baseline.
    Text(GlyphRun),
    /// A shape, positioned by the origin of its path.
    Shape {
        /// The shape.
        shape: Shape,
        /// The source node the shape came from.
        source: SourceRef,
    },
}

/// The regions a flow is laid out into: the current one, the ones that
/// follow, and their constraints (SPEC §3.3).
///
/// A layout container receives the regions available to it and returns a
/// fragment for the first; content that does not fit continues in the next.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Regions {
    /// The space left in the current region. Its height may be infinite for
    /// content measured without a page.
    pub size: Size,
    /// The full height of the current region, before anything was placed in
    /// it.
    pub full: Abs,
    /// The heights of the regions that follow, in order. Their widths equal
    /// the current one.
    pub backlog: EcoVec<Abs>,
    /// The height of every region after the backlog, if there are more.
    pub last: Option<Abs>,
    /// Areas of the current region that content must avoid, such as wrapped
    /// floats (SPEC §7.7), relative to its top-left corner.
    pub exclusions: EcoVec<Rect>,
    /// The baseline grid that body text snaps to, if there is one (SPEC
    /// §7.3).
    pub grid: Option<BaselineGrid>,
    /// Whether the current region is fresh and empty, so that content must
    /// be placed here even if it overflows: a layout container then has to
    /// place at least one line or block (SPEC §3.3).
    pub must_progress: bool,
}

impl Regions {
    /// A single region of a size, with nothing after it.
    pub fn one(size: Size) -> Self {
        Self {
            size,
            full: size.height,
            backlog: EcoVec::new(),
            last: None,
            exclusions: EcoVec::new(),
            grid: None,
            must_progress: false,
        }
    }

    /// The same region repeated without end, as on pages of one master.
    pub fn repeat(size: Size) -> Self {
        Self {
            last: Some(size.height),
            ..Self::one(size)
        }
    }

    /// The regions after the current one, or `None` if there are none.
    pub fn next(&self) -> Option<Self> {
        let (height, backlog) = match self.backlog.split_first() {
            Some((&height, rest)) => (height, rest.iter().copied().collect()),
            None => (self.last?, EcoVec::new()),
        };
        Some(Self {
            size: Size::new(self.size.width, height),
            full: height,
            backlog,
            last: self.last,
            exclusions: EcoVec::new(),
            grid: self.grid,
            must_progress: true,
        })
    }
}

/// A baseline grid: lines every `step`, the first `offset` below the top of
/// the region (SPEC §7.3).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct BaselineGrid {
    /// The distance between grid lines.
    pub step: Abs,
    /// The distance from the top of the region to the first grid line.
    pub offset: Abs,
}

impl BaselineGrid {
    /// The first grid line at or below `y`, measured from the top of the
    /// region. A grid with a step that is not positive and finite does not
    /// move `y`.
    pub fn snap(self, y: Abs) -> Abs {
        let step = self.step.to_pt();
        if !(step > 0.0 && step.is_finite()) {
            return y;
        }
        if y <= self.offset {
            return self.offset;
        }
        let lines = ((y - self.offset).to_pt() / step).ceil();
        self.offset + self.step * lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_region() {
        let mut regions = Regions::one(Size::new(Abs::pt(100.0), Abs::pt(50.0)));
        regions.backlog = EcoVec::from([Abs::pt(80.0)]);
        regions.last = Some(Abs::pt(90.0));
        let second = regions.next().unwrap();
        assert_eq!(second.size.height, Abs::pt(80.0));
        assert!(second.must_progress);
        let third = second.next().unwrap();
        assert_eq!(third.size.height, Abs::pt(90.0));
        assert_eq!(third.next().unwrap().size.height, Abs::pt(90.0));
        assert_eq!(Regions::one(Size::zero()).next(), None);
    }

    #[test]
    fn grid_snapping() {
        let grid = BaselineGrid {
            step: Abs::pt(13.0),
            offset: Abs::pt(10.0),
        };
        assert_eq!(grid.snap(Abs::pt(10.0)), Abs::pt(10.0));
        assert_eq!(grid.snap(Abs::pt(11.0)), Abs::pt(23.0));
        assert_eq!(grid.snap(Abs::pt(23.0)), Abs::pt(23.0));
        assert_eq!(grid.snap(Abs::pt(2.0)), Abs::pt(10.0));
    }
}
