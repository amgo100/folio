//! Watching the project directory with the operating system's file events.

use std::path::Path;

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::Error;
use crate::engine::Handle;

/// Watches a directory and everything under it, and tells the engine the
/// paths that change. Dropping it stops watching.
pub(crate) struct Watcher(#[allow(dead_code)] RecommendedWatcher);

impl Watcher {
    /// Starts watching `root`.
    pub(crate) fn start(root: &Path, engine: Handle) -> Result<Self, Error> {
        let everything = root.to_path_buf();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                match event {
                    Ok(event) => {
                        if is_change(&event.kind) && !event.paths.is_empty() {
                            engine.changed(event.paths);
                        }
                    }
                    // Events were lost: read everything again.
                    Err(_) => engine.changed(vec![everything.clone()]),
                }
            })
            .map_err(|e| Error(format!("cannot watch the project: {e}")))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| Error(format!("cannot watch `{}`: {e}", root.display())))?;
        Ok(Self(watcher))
    }
}

/// Whether an event may have changed what a file holds or which files
/// there are: not a file opened, read or closed unwritten, which the
/// loader's own reads cause.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}
