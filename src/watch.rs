use std::path::Path;
use std::sync::mpsc::Sender;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::app::AppEvent;

/// Files a `bd` write touches. Read-only `bd` calls (including abacus's own
/// `bd list`) churn only `embeddeddolt/**`, so a non-recursive watch filtered to
/// these names fires on writes and never feeds back on our reloads.
const WRITE_MARKERS: [&str; 2] = ["last-touched", "interactions.jsonl"];

/// The returned watcher must be kept alive for events to keep flowing.
pub fn spawn(beads_dir: &Path, tx: Sender<AppEvent>) -> notify::Result<RecommendedWatcher> {
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        let is_write = ev.paths.iter().any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| WRITE_MARKERS.contains(&n))
        });
        if is_write {
            let _ = tx.send(AppEvent::BeadsChanged);
        }
    })?;
    w.watch(beads_dir, RecursiveMode::NonRecursive)?;
    Ok(w)
}
