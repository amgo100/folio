//! Watching the project directory with the operating system's file events.

use std::path::Path;

use notify::event::{AccessKind, AccessMode, EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::Error;
use crate::engine::{Change, Handle};

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
                    // Events were lost, as when the system's queue overflowed:
                    // read everything again.
                    // The closes lost with them leave no file open.
                    Ok(event) if event.need_rescan() => {
                        engine.event(vec![everything.clone()], Change::Moved)
                    }
                    Ok(event) => {
                        if let Some(change) = change(&event.kind)
                            && !event.paths.is_empty()
                        {
                            engine.event(event.paths, change);
                        }
                    }
                    // Events were lost: read everything again.
                    Err(_) => engine.event(vec![everything.clone()], Change::Moved),
                }
            })
            .map_err(|e| Error(format!("cannot watch the project: {e}")))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| Error(format!("cannot watch `{}`: {e}", root.display())))?;
        Ok(Self(watcher))
    }
}

/// What an event says about the files it names, if it may have changed
/// what a file holds or which files there are: not a file opened, read or
/// closed unwritten, which the loader's own reads cause.
fn change(kind: &EventKind) -> Option<Change> {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => Some(Change::Closed),
        EventKind::Access(_) => None,
        // A truncate or a write, which a close may follow.
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Any) => Some(Change::Written),
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)) => {
            Some(Change::Moved)
        }
        _ => Some(Change::Other),
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, DataChange, MetadataKind};

    use super::*;

    #[test]
    fn events_are_sorted_by_what_they_say() {
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert_eq!(change(&modify), Some(Change::Written));
        let close = EventKind::Access(AccessKind::Close(AccessMode::Write));
        assert_eq!(change(&close), Some(Change::Closed));
        let read = EventKind::Access(AccessKind::Close(AccessMode::Read));
        assert_eq!(change(&read), None);
        let create = EventKind::Create(CreateKind::File);
        assert_eq!(change(&create), Some(Change::Moved));
        let chmod = EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions));
        assert_eq!(change(&chmod), Some(Change::Other));
    }
}
