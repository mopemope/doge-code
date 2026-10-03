//! Side-effect-free argument checks for built-in mutations and processes.
//! Use the dispatcher's typed inputs before executing any sibling in a batch.
use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn typed<T: DeserializeOwned>(arguments: &Value) -> Result<()> {
    serde_json::from_value::<T>(arguments.clone())
        .map(|_| ())
        .map_err(Into::into)
}

pub(super) fn validate_builtin_arguments(name: &str, arguments: &Value) -> Result<()> {
    ensure!(arguments.is_object(), "tool arguments must be an object");
    let check = match name {
        "find_file" => {
            typed::<crate::tools::find_file::FindFileArgs>(arguments)?;
            serde_json::from_value::<crate::tools::find_file::FindFileOptions>(arguments.clone())?
                .validate()
        }
        "fs_write" => typed::<crate::tools::write::FsWriteArgs>(arguments),
        "edit" => typed::<crate::tools::edit::EditParams>(arguments),
        "apply_patch" => typed::<crate::tools::apply_patch::ApplyPatchParams>(arguments),
        "execute_process" => typed::<crate::execution::ExecuteProcessParams>(arguments),
        "plan_write" => typed::<crate::tools::plan::PlanWriteArgs>(arguments),
        "requirements_write" => {
            typed::<crate::tools::requirements::RequirementsWriteArgs>(arguments)
        }
        "task" => typed::<crate::tools::task::TaskParams>(arguments),
        "execute_bash" | "execute_shell" => required_strings(arguments, &["command"]),
        "write_memory" => required_strings(arguments, &["key", "content"]),
        "run_workflow" => required_strings(arguments, &["workflow_name"]),
        "doc_generate" => required_strings(arguments, &["path"]),
        _ => Ok(()),
    };
    check.with_context(|| format!("invalid {name} arguments"))
}

fn required_strings(arguments: &Value, names: &[&str]) -> Result<()> {
    for name in names {
        ensure!(
            arguments.get(name).is_some_and(Value::is_string),
            "{name} must be a string"
        );
    }
    Ok(())
}
