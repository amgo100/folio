// Derived from typst-utils v0.15.1, crates/typst-utils/src/lib.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Small helpers taken from `typst-utils`, so that `folio-syntax` does not
//! depend on the rest of that crate.

use std::fmt::{Debug, Formatter};
use std::ops::{Deref, DerefMut};

use unicode_math_class::MathClass;

/// Turn a closure into a value implementing [`Debug`].
pub(crate) fn debug<F>(f: F) -> impl Debug
where
    F: Fn(&mut Formatter<'_>) -> std::fmt::Result,
{
    struct Wrapper<F>(F);

    impl<F> Debug for Wrapper<F>
    where
        F: Fn(&mut Formatter<'_>) -> std::fmt::Result,
    {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            self.0(f)
        }
    }

    Wrapper(f)
}

/// Run `deferred` on `thing` when the returned handle is dropped.
pub(crate) fn defer<T, F: FnOnce(&mut T)>(thing: &mut T, deferred: F) -> impl DerefMut<Target = T> {
    struct DeferHandle<'a, T, F: FnOnce(&mut T)> {
        thing: &'a mut T,
        deferred: Option<F>,
    }

    impl<T, F: FnOnce(&mut T)> Drop for DeferHandle<'_, T, F> {
        fn drop(&mut self) {
            if let Some(deferred) = self.deferred.take() {
                deferred(self.thing);
            }
        }
    }

    impl<T, F: FnOnce(&mut T)> Deref for DeferHandle<'_, T, F> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            self.thing
        }
    }

    impl<T, F: FnOnce(&mut T)> DerefMut for DeferHandle<'_, T, F> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.thing
        }
    }

    DeferHandle {
        thing,
        deferred: Some(deferred),
    }
}

/// The math class of a character, with Typst's overrides of the Unicode
/// classification for better spacing.
pub(crate) fn default_math_class(c: char) -> Option<MathClass> {
    match c {
        ':' => Some(MathClass::Relation),
        '⋯' | '⋱' | '⋰' | '⋮' => Some(MathClass::Normal),
        '.' | '/' => Some(MathClass::Normal),
        // ⊥ UP TACK is not a relation, unlike ⟂ PERPENDICULAR.
        '\u{22A5}' => Some(MathClass::Normal),
        // "Par" in linear logic.
        '⅋' => Some(MathClass::Binary),
        '⎰' | '⟅' => Some(MathClass::Opening),
        '⎱' | '⟆' => Some(MathClass::Closing),
        '⟇' => Some(MathClass::Binary),
        // Arabic comma.
        '،' => Some(MathClass::Punctuation),
        c => unicode_math_class::class(c),
    }
}
