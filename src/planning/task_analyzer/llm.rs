use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::llm::OpenAIClient;
use crate::planning::llm_decomposer::LlmTaskDecomposer;
use crate::tools::FsTools;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(crate) fn should_use_llm_decomposition(
    classification: &crate::planning::task_types::TaskClassification,
) -> bool {
    use crate::planning::task_types::TaskType::*;

    match classification.task_type {
        ArchitecturalChange | LargeRefactoring | ProjectRestructure => true,
        Refactoring | FeatureImplementation => classification.complexity_score > 0.7,
        MultiFileEdit => {
            classification.complexity_score > 0.6 || classification.estimated_steps > 8
        }
        _ => false,
    }
}

pub(crate) fn with_llm_decomposer(
    llm_decomposer_slot: &mut Option<LlmTaskDecomposer>,
    client: OpenAIClient,
    model: String,
    fs_tools: FsTools,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    cfg: AppConfig,
) {
    *llm_decomposer_slot = Some(LlmTaskDecomposer::new(
        client, model, fs_tools, repomap, cfg,
    ));
}
