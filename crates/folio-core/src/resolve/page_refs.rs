//! Page-number references (SPEC §2.3, §4.5): the bounded fixpoint that
//! gives each the page its target is laid out on.
//!
//! A page-number reference shows the page its target is on, and the text
//! it shows can move the target: a wider number can push a line, and so
//! the target, onto the next page. The build is therefore laid out in
//! passes ([`settle`](crate::pipeline::settle)), each a build of its own
//! whose references show the numbers the pass before measured:
//!
//! 1. The first pass knows no page: every page-number reference shows
//!    [`UNRESOLVED`](super::UNRESOLVED).
//! 2. Each later pass sets the pages the pass before measured. When a pass
//!    measures the pages it was set with, the numbers have converged. An
//!    ordinary document converges on the second pass: one extra pass.
//! 3. After [`EXTRA_PASSES`] extra passes without converging, the numbers
//!    are frozen at those of the last pass, so the output stays
//!    deterministic, and Resolve warns at each reference whose target's
//!    page did not settle.
//!
//! The build is then laid out with the numbers found, as the last pass
//! was. The passes are explicit rather than a cycle of queries on salsa's
//! fixpoint iteration, which does not allow the diagnostics that every
//! stage reports, and they always start from no page known, so the numbers
//! do not depend on which stage a host asks for first or on what an
//! earlier revision found (SPEC §2.4).

use ecow::EcoVec;
use folio_component::Location;

/// How many passes may follow the first before the page numbers are
/// frozen (SPEC §2.3).
pub const EXTRA_PASSES: u8 = 3;

/// The page numbers that a pass sets its page-number references with: the
/// pages of their targets, as the pass before measured them.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct PageNumbers {
    /// Which pass measured them: 0 before any layout, when every page is
    /// unknown.
    pub pass: u8,
    /// Each target of a page-number reference, in the order the build first
    /// refers to it, with the number of the page it is on, counted from 1,
    /// or `None` where no pass has found it on a page.
    pub pages: EcoVec<(Location, Option<u32>)>,
    /// The targets whose pages did not settle within the passes allowed:
    /// their numbers are frozen at the last pass's. Empty unless the
    /// numbers were frozen.
    pub unsettled: EcoVec<Location>,
    /// Whether the numbers are frozen.
    pub frozen: bool,
}

impl PageNumbers {
    /// The page number of a target, if a pass found it on a page.
    pub fn page(&self, target: Location) -> Option<u32> {
        self.pages
            .iter()
            .find(|(location, _)| *location == target)
            .and_then(|(_, page)| *page)
    }

    /// The numbers to set references to `targets` with: those known of
    /// each, in the order given.
    pub fn for_targets(&self, targets: impl IntoIterator<Item = Location>) -> Self {
        Self {
            pass: self.pass,
            pages: targets.into_iter().map(|t| (t, self.page(t))).collect(),
            unsettled: self.unsettled.clone(),
            frozen: self.frozen,
        }
    }
}

/// The page numbers that follow a pass set with `used`, which measured the
/// targets of `used` on the pages `measured`, in the same order.
pub fn next(used: &PageNumbers, measured: &[Option<u32>]) -> PageNumbers {
    debug_assert_eq!(used.pages.len(), measured.len());
    let settled = used
        .pages
        .iter()
        .zip(measured)
        .all(|((_, page), measured)| page == measured);
    if settled || used.frozen {
        return used.clone();
    }
    if used.pass >= EXTRA_PASSES {
        // The pass set with `used` was the last extra one: its layout is
        // kept, with the numbers it was set with.
        return PageNumbers {
            unsettled: used
                .pages
                .iter()
                .zip(measured)
                .filter(|((_, page), measured)| page != *measured)
                .map(|((target, _), _)| *target)
                .collect(),
            frozen: true,
            ..used.clone()
        };
    }
    PageNumbers {
        pass: used.pass + 1,
        pages: used
            .pages
            .iter()
            .zip(measured)
            .map(|((target, _), page)| (*target, *page))
            .collect(),
        unsettled: EcoVec::new(),
        frozen: false,
    }
}

#[cfg(test)]
mod tests {
    use folio_component::SourceRef;

    use super::*;

    fn target(instance: u32) -> Location {
        Location {
            source: SourceRef::DETACHED,
            instance,
        }
    }

    fn used(pass: u8, pages: &[Option<u32>]) -> PageNumbers {
        PageNumbers {
            pass,
            pages: pages
                .iter()
                .enumerate()
                .map(|(i, p)| (target(i as u32), *p))
                .collect(),
            unsettled: EcoVec::new(),
            frozen: false,
        }
    }

    #[test]
    fn converges_when_a_pass_measures_what_it_was_set_with() {
        let first = used(0, &[None, None]);
        let second = next(&first, &[Some(3), Some(4)]);
        assert_eq!(second, used(1, &[Some(3), Some(4)]));
        assert_eq!(next(&second, &[Some(3), Some(4)]), second);
    }

    #[test]
    fn a_target_on_no_page_converges_at_once() {
        let first = used(0, &[None]);
        assert_eq!(next(&first, &[None]), first);
    }

    #[test]
    fn freezes_after_the_extra_passes() {
        // A reference whose number moves its own target: 4, 5, 4, 5, ...
        let mut numbers = used(0, &[None, None]);
        let mut passes = 0;
        loop {
            let measured = match numbers.page(target(0)) {
                Some(4) => [Some(5), Some(1)],
                _ => [Some(4), Some(1)],
            };
            let after = next(&numbers, &measured);
            if after == numbers {
                break;
            }
            numbers = after;
            passes += 1;
            assert!(passes < 10, "the passes end");
        }
        // The last extra pass's numbers, frozen.
        assert_eq!(numbers.pass, EXTRA_PASSES);
        assert!(numbers.frozen);
        assert_eq!(numbers.unsettled.as_slice(), &[target(0)]);
        assert_eq!(numbers.page(target(1)), Some(1));
    }
}
