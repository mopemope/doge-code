use crate::analysis::Analyzer;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use tracing::{error, info, warn};

/// Delegate /rebuild-repomap to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_rebuild_repomap(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("[Starting forced complete repomap rebuild (ignoring cache)...]");
    let repomap_clone = executor.repomap.clone();
    let project_root = executor.cfg.project_root.clone();
    let ui_tx = executor.ui_tx.clone();

    tokio::spawn(async move {
        info!("Starting forced complete repomap rebuild");
        let start_time = std::time::Instant::now();

        let mut analyzer = match Analyzer::new(&project_root).await {
            Ok(analyzer) => analyzer,
            Err(e) => {
                error!("Failed to create Analyzer: {:?}", e);
                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!("[Failed to create analyzer: {}]", e));
                }
                return;
            }
        };

        // Clear cache first to force complete rebuild
        if let Err(e) = analyzer.clear_cache().await {
            warn!("Failed to clear cache before forced rebuild: {}", e);
        }

        match analyzer.build_parallel().await {
            Ok(map) => {
                let duration = start_time.elapsed();
                let symbol_count = map.symbols.len();
                *repomap_clone.write().await = Some(map);

                info!(
                    "Forced repomap rebuild completed in {:?} with {} symbols",
                    duration, symbol_count
                );
                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!(
                        "[Forced rebuild completed in {:?} - {} symbols found (cache cleared)]",
                        duration, symbol_count
                    ));
                }
            }
            Err(e) => {
                error!("Failed to force rebuild RepoMap: {:?}", e);
                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!("[Failed to force rebuild repomap: {}]", e));
                }
            }
        }
    });
}
