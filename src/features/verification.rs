use crate::llm::types::ToolCall;
use std::path::Path;
use tokio::process::Command;
use tracing::{debug, warn};

pub struct AutoVerifier;

impl Default for AutoVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoVerifier {
    pub fn new() -> Self {
        Self
    }

    /// Checks if the tool call warrants verification and runs it.
    /// Returns Some(warning_message) if verification fails.
    pub async fn verify(&self, tool_call: &ToolCall, success: bool) -> Option<String> {
        if !success {
            return None;
        }

        let function_name = tool_call.function.name.as_str();
        if !matches!(function_name, "fs_write" | "edit" | "apply_patch") {
            return None;
        }

        let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments).ok()?;

        // Extract file path from arguments
        let path_str = match function_name {
            "fs_write" => args.get("path").and_then(|v| v.as_str()),
            "edit" => args.get("file_path").and_then(|v| v.as_str()),
            "apply_patch" => args.get("file_path").and_then(|v| v.as_str()),
            _ => None,
        }?;

        let path = Path::new(path_str);
        self.run_verification(path).await
    }

    async fn run_verification(&self, path: &Path) -> Option<String> {
        let extension = path.extension().and_then(|e| e.to_str())?;

        match extension {
            "rs" => self.verify_rust(path).await,
            "py" => self.verify_python(path).await,
            "js" | "ts" | "jsx" | "tsx" => self.verify_node(path).await,
            "go" => self.verify_go(path).await,
            _ => None,
        }
    }

    async fn verify_rust(&self, _path: &Path) -> Option<String> {
        // Run cargo check
        // We assume we are in the project root or cargo can find the manifest using --manifest-path or just current dir.
        // Since the agent runs in project root, `cargo check` should work.
        debug!("Running cargo check verification");
        let output = Command::new("cargo")
            .arg("check")
            .arg("--quiet")
            .arg("--message-format=short")
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Combine output
            let msg = format!(
                "<verification_error>\nCargo Check Failed:\n{}{}\n</verification_error>",
                stdout, stderr
            );
            warn!("Verification failed: {}", msg);
            return Some(msg);
        }
        None
    }

    async fn verify_python(&self, path: &Path) -> Option<String> {
        // syntax check
        debug!("Running python syntax check on {:?}", path);
        let output = Command::new("python3")
            .arg("-m")
            .arg("py_compile")
            .arg(path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Some(format!(
                "<verification_error>\nPython Syntax Check Failed:\n{}\n</verification_error>",
                stderr
            ));
        }
        None
    }

    async fn verify_node(&self, path: &Path) -> Option<String> {
        // syntax check using node --check
        // Works for .js. For .ts, we might need tsc.
        // Let's try `node --check` for js/mjs
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext == "js" || ext == "mjs" {
            debug!("Running node syntax check on {:?}", path);
            let output = Command::new("node")
                .arg("--check")
                .arg(path)
                .output()
                .await
                .ok()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Some(format!(
                    "<verification_error>\nNode.js Syntax Check Failed:\n{}\n</verification_error>",
                    stderr
                ));
            }
        }

        // TypeScript verification using tsc
        if ext == "ts" || ext == "tsx" {
            debug!("Running tsc check on {:?}", path);
            // We use --noEmit to only check types/syntax without generating files
            // We also try to run it on the specific file.
            // Note: running tsc on a single file ignores tsconfig.json by default usually,
            // but it's better than nothing for syntax checks.
            let output = Command::new("tsc")
                .arg("--noEmit")
                .arg("--allowSyntheticDefaultImports")
                .arg("--target")
                .arg("esnext")
                .arg("--moduleResolution")
                .arg("node")
                .arg(path)
                .output()
                .await
                .ok()?;

            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // tsc outputs errors to stdout usually
                return Some(format!(
                    "<verification_error>\nTypeScript Check Failed:\n{}\n</verification_error>",
                    stdout
                ));
            }
        }
        None
    }

    async fn verify_go(&self, path: &Path) -> Option<String> {
        // go vet or build
        // go build -o /dev/null path/to/file.go often complains about package main if not main.
        // using `go vet` might be better.
        debug!("Running go vet on {:?}", path);
        let output = Command::new("go")
            .arg("vet")
            .arg(path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Some(format!(
                "<verification_error>\nGo Vet Failed:\n{}\n</verification_error>",
                stderr
            ));
        }
        None
    }
}
