//! The Folio compiler library: the memoised compile pipeline
//! ([`compile`]) and the [`World`] that supplies sources, fonts, files and
//! components (SPEC §2.2).
//!
//! A host builds a [`World`] from the project's manifest and lockfile, sets
//! the sources and files it reads from disk, and compiles a variant for a
//! target. Every stage is a memoised query (SPEC §2.3): after an edit,
//! compiling again re-runs only the queries the edit affects, and stops early
//! where a re-run query returns what it returned before. An edit cancels
//! compilations still running on other handles of the world.
//!
//! ```
//! use folio::{Name, Target, World};
//!
//! let mut world = World::new(
//!     "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
//!      [variants.exam]\ntargets = [\"pdf\"]\n",
//! )
//! .unwrap();
//! world.set_source("main.fol", "= Forces\n\nA ball is thrown.\n").unwrap();
//!
//! let exam = Name::new("exam").unwrap();
//! let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
//! assert!(!compiled.has_errors());
//! ```

#![forbid(unsafe_code)]

pub mod diag;
mod world;

use std::fmt::{self, Display, Formatter};
use std::panic::AssertUnwindSafe;

use rayon::prelude::*;

pub use folio_component::{Name, Registry, Target};
pub use folio_core::db::{Bytes, Compiled, FontFile};
pub use folio_core::diag::{Diagnostic, Edit, Fix, Label, Severity};
pub use folio_core::project::{Lockfile, Manifest, ProjectError, ProjectPath};
pub use folio_syntax::{FileId, Source};

pub use self::world::{Event, World, WorldBuilder, WorldError};

/// The version of this compiler, which `folio.toml` pins for byte-identical
/// output (SPEC §2.4).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compiles a variant of the world's document for a target. The variant is
/// `None` only for a project that declares no variants.
///
/// Problems in the document are diagnostics in the result. Fails only when
/// an edit to the world cancels the compilation.
pub fn compile(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Compiled, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        folio_core::db::compile(db, build)
    })
}

/// Compiles every variant and target the manifest declares
/// ([`World::builds`]), in parallel, each on its own handle of the world.
/// Queries that do not read the variant or target run once and are shared.
pub fn compile_all(world: &World) -> Vec<Output> {
    // A handle is not `Sync`, so each build takes its own.
    let builds: Vec<_> = world
        .builds()
        .into_iter()
        .map(|build| (world.clone(), build))
        .collect();
    builds
        .into_par_iter()
        .map(|(world, (variant, target))| {
            let result = compile(&world, target, variant.as_ref());
            Output {
                variant,
                target,
                result,
            }
        })
        .collect()
}

/// The result of one build of [`compile_all`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Output {
    /// The variant, or `None` for a project without variants.
    pub variant: Option<Name>,
    /// The target.
    pub target: Target,
    /// What compiling it produced.
    pub result: Result<Compiled, Cancelled>,
}

/// A compilation was stopped by an edit to the world, so its result would
/// have been stale. Compile again to get a current one.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Cancelled;

impl Display for Cancelled {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("the compilation was cancelled by an edit")
    }
}

impl std::error::Error for Cancelled {}

/// Runs queries, turning salsa's cancellation unwind into [`Cancelled`].
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, Cancelled> {
    salsa::Cancelled::catch(AssertUnwindSafe(f)).map_err(|cancelled| match cancelled {
        salsa::Cancelled::PropagatedPanic => {
            panic!("a query panicked on another thread")
        }
        _ => Cancelled,
    })
}
