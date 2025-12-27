use crate::config::AppConfig;
use crate::error_recovery::diagnosis::DiagnosisEngine;
use crate::error_recovery::strategies::{FixStrategy, PatchFixStrategy};
use crate::error_recovery::types::{ErrorContext, ErrorType, FixResult};

pub struct ErrorRecoveryTool {
    _config: AppConfig, // Keep config for future use
    diagnosis_engine: DiagnosisEngine,
    strategies: Vec<Box<dyn FixStrategy>>,
}

impl ErrorRecoveryTool {
    pub fn new(config: AppConfig) -> Self {
        let mut strategies: Vec<Box<dyn FixStrategy>> = vec![];
        strategies.push(Box::new(PatchFixStrategy));

        // Future expansion: add other strategies
        // strategies.push(Box::new(CompilationFixStrategy));
        // strategies.push(Box::new(TestFixStrategy));

        Self {
            _config: config,
            diagnosis_engine: DiagnosisEngine,
            strategies,
        }
    }

    pub async fn attempt_recovery(
        &self,
        error_message: &str,
        error_context: ErrorContext,
    ) -> Result<FixResult, anyhow::Error> {
        // Diagnose the error
        let error_type = self.diagnose_error(error_message, &error_context).await?;

        // Try appropriate strategy for fixing
        for strategy in &self.strategies {
            if strategy.can_handle(&error_type) {
                return strategy.attempt_fix(&error_type).await;
            }
        }

        // If no strategy can handle it, require human intervention
        Ok(FixResult::RequiresHumanIntervention {
            message: format!("No strategy available to fix this error: {}", error_message),
        })
    }

    async fn diagnose_error(
        &self,
        error_message: &str,
        context: &ErrorContext,
    ) -> Result<ErrorType, anyhow::Error> {
        // Diagnose based on error source
        match context.error_source.as_str() {
            "apply_patch" => Ok(self.diagnosis_engine.diagnose_apply_patch_error(
                error_message,
                &context.file_path,
                &context.patch_content,
            )),
            _ => Err(anyhow::anyhow!(
                "Unknown error source: {}",
                context.error_source
            )),
        }
    }
}
