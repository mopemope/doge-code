use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /map to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_map(executor: &mut TuiExecutor, _ui: &mut TuiApp) {
    let repomap = executor.repomap.clone();
    let ui_tx = executor.ui_tx.clone().unwrap();
    tokio::spawn(async move {
        let repomap_guard = repomap.read().await;
        if let Some(map) = &*repomap_guard {
            let _ = ui_tx.send(format!("RepoMap: {} symbols ", map.symbols.len()));
            for s in map.symbols.iter().take(50) {
                let _ = ui_tx.send(format!(
                    "{} {}  @{}:{}",
                    s.kind.as_str(),
                    s.name,
                    s.file.display(),
                    s.start_line
                ));
            }
        } else {
            let _ = ui_tx.send("[repomap] Still generating...".to_string());
        }
    });
}
