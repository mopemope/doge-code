use crate::tools::budget::head_tail_truncate;

use super::types::{
    CommandEvidence, VerificationContext, VerificationKind, VerificationObservedEvent,
    VerificationOutcome, VerificationSource,
};

/// Provenance output budget: stdout + stderr excerpts stay within ~4 KiB.
pub const PROVENANCE_OUTPUT_BUDGET_CHARS: usize = 4_096;
const PROVENANCE_EXCERPT_STREAM_BUDGET: usize = 2_048;

/// Compute the identity hash for a diff or captured output.
///
/// The value is `blake3:<hex>`; it is an identity/dedup handle, not a
/// cryptographic proof of anything.
pub fn diff_hash_for(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

/// Classify a structured `program + args` invocation.
///
/// Conservative exact matching only: substring or fuzzy matches are rejected
/// so `echo cargo test`, `my-cargo-test-wrapper`, `cat test.log`, or
/// `npm run contest` never classify as verification.
pub fn classify_verification(program: &str, args: &[String]) -> Option<VerificationKind> {
    let argv: Vec<&str> = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect();
    classify_argv(&argv)
}

fn classify_argv(argv: &[&str]) -> Option<VerificationKind> {
    let (program, args) = argv.split_first()?;
    let prog = basename(program);
    match prog {
        "cargo" => classify_cargo(args),
        "go" => classify_go(args),
        "pytest" => Some(VerificationKind::Test),
        "python" | "python3" => classify_python(args),
        "npm" | "pnpm" | "yarn" | "bun" => classify_js_pm(prog, args),
        "npx" => classify_npx(args),
        "tsc" => classify_tsc(args),
        "eslint" => Some(VerificationKind::Lint),
        "vitest" | "jest" => Some(VerificationKind::Test),
        "dotnet" => classify_dotnet(args),
        "mvn" => classify_maven(args),
        "gradle" | "gradlew" => classify_gradle(args),
        "ctest" => Some(VerificationKind::Test),
        "cmake" => classify_cmake(args),
        _ => {
            // `./gradlew` is invoked via a relative path; basename above
            // already strips directories, but keep the literal check for
            // clarity on Windows-style names.
            if prog == "gradlew" || prog == "gradlew.bat" {
                classify_gradle(args)
            } else {
                None
            }
        }
    }
}

fn basename(program: &str) -> &str {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).trim();
    if base.len() > 4 && base[base.len() - 4..].eq_ignore_ascii_case(".exe") {
        base[..base.len() - 4].trim()
    } else {
        base
    }
}

fn classify_cargo(args: &[&str]) -> Option<VerificationKind> {
    let first = *args.first()?;
    match first {
        "test" => Some(VerificationKind::Test),
        "check" => Some(VerificationKind::TypeCheck),
        "clippy" => Some(VerificationKind::Lint),
        "fmt" => {
            if args.contains(&"--check") {
                Some(VerificationKind::FormatCheck)
            } else {
                None
            }
        }
        "build" => Some(VerificationKind::Build),
        "nextest" => {
            if args.get(1).is_some_and(|s| *s == "run") {
                Some(VerificationKind::Test)
            } else {
                None
            }
        }
        "metadata" => None,
        _ => None,
    }
}

fn classify_go(args: &[&str]) -> Option<VerificationKind> {
    match args.first().copied() {
        Some("test") => Some(VerificationKind::Test),
        Some("build") => Some(VerificationKind::Build),
        Some("vet") => Some(VerificationKind::Lint),
        _ => None,
    }
}

fn classify_python(args: &[&str]) -> Option<VerificationKind> {
    // `python -m pytest ...` -> Test; `python -m py_compile ...` -> SyntaxCheck.
    // A script literally named `pytest.py` must not match.
    if args.len() >= 2 && args[0] == "-m" {
        match args[1] {
            "pytest" => Some(VerificationKind::Test),
            "py_compile" => Some(VerificationKind::SyntaxCheck),
            _ => None,
        }
    } else {
        None
    }
}

fn classify_js_pm(_pm: &str, args: &[&str]) -> Option<VerificationKind> {
    // npm/pnpm/yarn/bun <test|run test|run lint|run build|run typecheck|run check>
    match args {
        ["test", ..] => Some(VerificationKind::Test),
        ["run", script, ..] => match *script {
            "test" => Some(VerificationKind::Test),
            "lint" => Some(VerificationKind::Lint),
            "build" => Some(VerificationKind::Build),
            "typecheck" | "type-check" | "check" => Some(VerificationKind::TypeCheck),
            _ => None,
        },
        _ => None,
    }
}

fn classify_npx(args: &[&str]) -> Option<VerificationKind> {
    let first = *args.first()?;
    match first {
        "tsc" => classify_tsc(&args[1..]),
        "eslint" => Some(VerificationKind::Lint),
        "vitest" => Some(VerificationKind::Test),
        "jest" => Some(VerificationKind::Test),
        _ => None,
    }
}

fn classify_tsc(args: &[&str]) -> Option<VerificationKind> {
    if args.contains(&"--noEmit") {
        Some(VerificationKind::TypeCheck)
    } else {
        None
    }
}

fn classify_dotnet(args: &[&str]) -> Option<VerificationKind> {
    match args.first().copied() {
        Some("test") => Some(VerificationKind::Test),
        Some("build") => Some(VerificationKind::Build),
        _ => None,
    }
}

fn classify_maven(args: &[&str]) -> Option<VerificationKind> {
    // `mvn test|verify` -> Test; `mvn package` -> Build.
    if args.contains(&"test") || args.contains(&"verify") {
        Some(VerificationKind::Test)
    } else if args.contains(&"package") {
        Some(VerificationKind::Build)
    } else {
        None
    }
}

fn classify_gradle(args: &[&str]) -> Option<VerificationKind> {
    // Task names may be mixed with flags; only exact task tokens count.
    let has = |t: &str| args.contains(&t);
    if has("test") || has("check") {
        Some(VerificationKind::Test)
    } else if has("build") {
        Some(VerificationKind::Build)
    } else {
        None
    }
}

fn classify_cmake(args: &[&str]) -> Option<VerificationKind> {
    if args.first().copied() == Some("--build") {
        Some(VerificationKind::Build)
    } else {
        None
    }
}

/// Find the single `in_progress` plan item, if any.
///
/// Never infers from filenames, content, LLM output, or completed items.
/// Returns `None` when there is no (or more than one, which validation
/// forbids) `in_progress` item.
pub fn current_in_progress_plan_item(items: &[crate::tools::plan::PlanItem]) -> Option<String> {
    let mut found: Option<String> = None;
    for item in items {
        if item.status == "in_progress" {
            if found.is_some() {
                return None;
            }
            found = Some(item.id.clone());
        }
    }
    found
}

/// Build the pre-execution snapshot: current plan item + active change ids.
///
/// Call this before the process starts so a change that lands mid-run is
/// never attributed to the running command. Requirement ids are the union of
/// active change requirement ids, falling back to current plan item links
/// (for contract/test-only or pre-change validation runs).
pub fn capture_verification_context(
    plan_items: &[crate::tools::plan::PlanItem],
    active_change_ids: &[String],
) -> VerificationContext {
    capture_verification_context_full(
        plan_items,
        active_change_ids,
        &[],
        None,
        &std::collections::HashMap::new(),
    )
}

/// Full capture with directive attribution and requirement resolution.
///
/// `change_requirement_ids` maps `change_id -> requirement_ids` (frozen ids
/// from `ChangeCommitted`); `plan_requirement_ids` are the current plan item
/// links used only as a fallback when no active change carries requirements.
#[allow(clippy::too_many_arguments)]
pub fn capture_verification_context_full(
    plan_items: &[crate::tools::plan::PlanItem],
    active_change_ids: &[String],
    change_requirement_ids: &[Vec<String>],
    directive_id: Option<String>,
    plan_requirement_ids: &std::collections::HashMap<String, Vec<String>>,
) -> VerificationContext {
    let plan_item_id = current_in_progress_plan_item(plan_items);
    let mut requirement_ids: Vec<String> = change_requirement_ids
        .iter()
        .flatten()
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    requirement_ids.sort();
    if requirement_ids.is_empty()
        && let Some(current) = plan_item_id.as_deref()
        && let Some(linked) = plan_requirement_ids.get(current)
    {
        requirement_ids = linked.clone();
        requirement_ids.sort();
        requirement_ids.dedup();
    }
    VerificationContext {
        directive_id,
        plan_item_id,
        requirement_ids,
        observed_change_ids: active_change_ids.to_vec(),
        matched_obligations: Vec::new(),
    }
}

/// Input for building a `VerificationObserved` event from a finished process.
pub struct VerificationRecordInput<'a> {
    pub kind: VerificationKind,
    pub source: VerificationSource,
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd_relative: Option<String>,
    pub success: bool,
    pub status: &'a str,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: &'a str,
    pub stderr: &'a str,
    /// True when the execution core already truncated capture before we saw
    /// it; the digest then covers captured output, not complete output.
    pub capture_truncated: bool,
    pub context: VerificationContext,
    pub extra_warnings: Vec<String>,
}

pub fn build_verification_event(input: VerificationRecordInput<'_>) -> VerificationObservedEvent {
    let stdout_excerpt = head_tail_truncate(input.stdout, PROVENANCE_EXCERPT_STREAM_BUDGET).text;
    let stderr_excerpt = head_tail_truncate(input.stderr, PROVENANCE_EXCERPT_STREAM_BUDGET).text;
    // Digest covers the output available to us at record time.
    let mut hasher_input = Vec::with_capacity(input.stdout.len() + input.stderr.len() + 1);
    hasher_input.extend_from_slice(input.stdout.as_bytes());
    hasher_input.push(0);
    hasher_input.extend_from_slice(input.stderr.as_bytes());
    let output_digest = format!("blake3:{}", blake3::hash(&hasher_input).to_hex());

    let excerpt_truncated = input.stdout.chars().count() > PROVENANCE_EXCERPT_STREAM_BUDGET
        || input.stderr.chars().count() > PROVENANCE_EXCERPT_STREAM_BUDGET;
    let output_truncated = excerpt_truncated || input.capture_truncated;

    let mut warnings = input.extra_warnings;
    if input.capture_truncated && !warnings.iter().any(|w| w.contains("capture")) {
        warnings.push(
            "process output exceeded capture limits before provenance recording; digest covers captured output only"
                .to_string(),
        );
    }

    VerificationObservedEvent {
        directive_id: input.context.directive_id.clone(),
        plan_item_id: input.context.plan_item_id.clone(),
        requirement_ids: input.context.requirement_ids.clone(),
        verification_kind: input.kind,
        source: input.source,
        command: CommandEvidence {
            program: input.program.to_string(),
            args: input.args.to_vec(),
            cwd: input.cwd_relative,
        },
        outcome: VerificationOutcome {
            success: input.success,
            status: input.status.to_string(),
            exit_code: input.exit_code,
            timed_out: input.timed_out,
        },
        observed_change_ids: input.context.observed_change_ids.clone(),
        matched_obligations: input.context.matched_obligations.clone(),
        stdout_excerpt,
        stderr_excerpt,
        output_digest,
        output_truncated,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_rust_commands() {
        assert_eq!(
            classify_verification("cargo", &args(&["test"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("cargo", &args(&["nextest", "run"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("cargo", &args(&["check"])),
            Some(VerificationKind::TypeCheck)
        );
        assert_eq!(
            classify_verification("cargo", &args(&["clippy"])),
            Some(VerificationKind::Lint)
        );
        assert_eq!(
            classify_verification("cargo", &args(&["fmt", "--check"])),
            Some(VerificationKind::FormatCheck)
        );
        assert_eq!(
            classify_verification("cargo", &args(&["build"])),
            Some(VerificationKind::Build)
        );
        assert_eq!(classify_verification("cargo", &args(&["metadata"])), None);
    }

    #[test]
    fn test_go_python_commands() {
        assert_eq!(
            classify_verification("go", &args(&["test", "./..."])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("go", &args(&["build", "./..."])),
            Some(VerificationKind::Build)
        );
        assert_eq!(
            classify_verification("go", &args(&["vet", "./..."])),
            Some(VerificationKind::Lint)
        );
        assert_eq!(
            classify_verification("pytest", &[]),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("python", &args(&["-m", "pytest"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("python", &args(&["-m", "py_compile", "a.py"])),
            Some(VerificationKind::SyntaxCheck)
        );
    }

    #[test]
    fn test_js_commands() {
        assert_eq!(
            classify_verification("pnpm", &args(&["test"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("npm", &args(&["run", "lint"])),
            Some(VerificationKind::Lint)
        );
        assert_eq!(
            classify_verification("npm", &args(&["run", "build"])),
            Some(VerificationKind::Build)
        );
        assert_eq!(
            classify_verification("npx", &args(&["tsc", "--noEmit"])),
            Some(VerificationKind::TypeCheck)
        );
        assert_eq!(
            classify_verification("npx", &args(&["eslint", "."])),
            Some(VerificationKind::Lint)
        );
        assert_eq!(
            classify_verification("npx", &args(&["vitest", "run"])),
            Some(VerificationKind::Test)
        );
    }

    #[test]
    fn test_dotnet_java_cmake() {
        assert_eq!(
            classify_verification("dotnet", &args(&["test"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("dotnet", &args(&["build"])),
            Some(VerificationKind::Build)
        );
        assert_eq!(
            classify_verification("mvn", &args(&["test"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("mvn", &args(&["verify"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("mvn", &args(&["package"])),
            Some(VerificationKind::Build)
        );
        assert_eq!(
            classify_verification("gradle", &args(&["test"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("./gradlew", &args(&["check"])),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("./gradlew", &args(&["build"])),
            Some(VerificationKind::Build)
        );
        assert_eq!(
            classify_verification("ctest", &[]),
            Some(VerificationKind::Test)
        );
        assert_eq!(
            classify_verification("cmake", &args(&["--build", "."])),
            Some(VerificationKind::Build)
        );
    }

    #[test]
    fn test_near_miss_not_classified() {
        assert_eq!(
            classify_verification("echo", &args(&["cargo", "test"])),
            None
        );
        assert_eq!(classify_verification("my-cargo-test-wrapper", &[]), None);
        assert_eq!(classify_verification("cat", &args(&["test.log"])), None);
        assert_eq!(
            classify_verification("npm", &args(&["run", "contest"])),
            None
        );
        assert_eq!(
            classify_verification("python", &args(&["script_named_pytest.py"])),
            None
        );
        assert_eq!(classify_verification("cargo", &args(&["testish"])), None);
    }

    #[test]
    fn test_verification_event_budget_and_digest() {
        let big_stdout = "x".repeat(10_000);
        let event = build_verification_event(VerificationRecordInput {
            kind: VerificationKind::Test,
            source: VerificationSource::ExecuteProcess,
            program: "cargo",
            args: &args(&["test"]),
            cwd_relative: None,
            success: true,
            status: "completed",
            exit_code: Some(0),
            timed_out: false,
            stdout: &big_stdout,
            stderr: "",
            capture_truncated: false,
            context: VerificationContext {
                plan_item_id: Some("step-1".to_string()),
                observed_change_ids: vec!["chg-1".to_string()],
                matched_obligations: Vec::new(),
                directive_id: None,
                requirement_ids: Vec::new(),
            },
            extra_warnings: vec![],
        });
        assert!(event.output_truncated);
        assert!(event.stdout_excerpt.chars().count() <= PROVENANCE_OUTPUT_BUDGET_CHARS);
        assert!(event.output_digest.starts_with("blake3:"));
        assert_eq!(event.observed_change_ids, vec!["chg-1".to_string()]);
        // Deterministic digest.
        let again = build_verification_event(VerificationRecordInput {
            kind: VerificationKind::Test,
            source: VerificationSource::ExecuteProcess,
            program: "cargo",
            args: &args(&["test"]),
            cwd_relative: None,
            success: true,
            status: "completed",
            exit_code: Some(0),
            timed_out: false,
            stdout: &big_stdout,
            stderr: "",
            capture_truncated: false,
            context: VerificationContext::default(),
            extra_warnings: vec![],
        });
        assert_eq!(event.output_digest, again.output_digest);
    }

    #[test]
    fn test_in_progress_resolution() {
        use crate::tools::plan::PlanItem;
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "a".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "b".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        assert_eq!(
            current_in_progress_plan_item(&items),
            Some("step-2".to_string())
        );
        let completed = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "completed".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        // Never infer from completed items.
        assert_eq!(current_in_progress_plan_item(&completed), None);
    }
}
