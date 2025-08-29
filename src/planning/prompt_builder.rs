use crate::planning::execution_context::ExecutionContext;
use crate::planning::task_types::TaskStep;

/// Build analysis prompt
pub fn build_analysis_prompt(step: &TaskStep, context: &ExecutionContext) -> String {
    let mut prompt = format!(
        r#"# Analysis Task Execution

## Step Information
- **ID**: {}
- **Description**: {}
- **Type**: Analysis

## Execution Instructions
{}

## Available Tools
{}

## Prerequisites
- Analyze the project codebase in detail
- Read relevant files as needed
- Include specific and practical information in the analysis results

## Completion Criteria
{}

Execute the analysis and report the results in detail.
"#,
        step.id,
        step.description,
        step.prompt_template.as_deref().unwrap_or(&step.description),
        step.required_tools.join(", "),
        step.validation_criteria.join("\n- ")
    );

    // Include results from previous steps
    if !context.completed_steps().is_empty() {
        prompt.push_str("\n## Previous Step Results\n");
        for (i, result) in context.completed_steps().iter().enumerate() {
            if result.success {
                prompt.push_str(&format!(
                    "{}. {}: {}\n",
                    i + 1,
                    result.step_id,
                    result.output
                ));
            }
        }
    }

    prompt
}

/// Build planning prompt
pub fn build_planning_prompt(step: &TaskStep, context: &ExecutionContext) -> String {
    let mut prompt = format!(
        r#"# Planning Task Execution

## Step Information
- **ID**: {}
- **Description**: {}
- **Type**: Planning

## Execution Instructions
{}

## Available Tools
{}

## Prerequisites
- Create a detailed execution plan based on the analysis results
- Clarify the implementation order and dependencies
- Include risks and countermeasures

## Completion Criteria
{}

Create a detailed execution plan.
"#,
        step.id,
        step.description,
        step.prompt_template.as_deref().unwrap_or(&step.description),
        step.required_tools.join(", "),
        step.validation_criteria.join("\n- ")
    );

    // Include analysis results
    if !context.completed_steps().is_empty() {
        prompt.push_str("\n## Analysis Results\n");
        for result in context.completed_steps() {
            if result.success && result.step_id.contains("analysis") {
                prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
            }
        }
    }

    prompt
}

/// Build implementation prompt
pub fn build_implementation_prompt(step: &TaskStep, context: &ExecutionContext) -> String {
    let mut prompt = format!(
        r#"# Implementation Task Execution

## Step Information
- **ID**: {}
- **Description**: {}
- **Type**: Implementation

## Execution Instructions
{}

## Available Tools
{}

## Important Notes
- Always check the current content before modifying files
- Implement in stages, running compilation/tests at each stage
- Appropriately fix any errors that occur
- Clearly document changes made

## Completion Criteria
{}

Execute the implementation. Make sure compilation succeeds.
"#,
        step.id,
        step.description,
        step.prompt_template.as_deref().unwrap_or(&step.description),
        step.required_tools.join(", "),
        step.validation_criteria.join("\n- ")
    );

    // Include planning results
    if !context.completed_steps().is_empty() {
        prompt.push_str("\n## Execution Plan\n");
        for result in context.completed_steps() {
            if result.success
                && (result.step_id.contains("planning") || result.step_id.contains("analysis"))
            {
                prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
            }
        }
    }

    prompt
}

/// Build validation prompt
pub fn build_validation_prompt(step: &TaskStep, context: &ExecutionContext) -> String {
    let mut prompt = format!(
        r#"# Validation Task Execution

## Step Information
- **ID**: {}
- **Description**: {}
- **Type**: Validation

## Execution Instructions
{}

## Available Tools
{}

## Validation Items
- Verify that the implementation works correctly
- Confirm there are no compilation errors
- Ensure tests pass
- Verify that expected behavior is met

## Completion Criteria
{}

Execute comprehensive validation and report the results.
"#,
        step.id,
        step.description,
        step.prompt_template.as_deref().unwrap_or(&step.description),
        step.required_tools.join(", "),
        step.validation_criteria.join("\n- ")
    );

    // Include implementation results
    if !context.completed_steps().is_empty() {
        prompt.push_str("\n## Implementation Results\n");
        for result in context.completed_steps() {
            if result.success && result.step_id.contains("implementation") {
                prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
            }
        }
    }

    prompt
}

/// Build cleanup prompt
pub fn build_cleanup_prompt(step: &TaskStep, _context: &ExecutionContext) -> String {
    format!(
        r#"# Cleanup Task Execution

## Step Information
- **ID**: {}
- **Description**: {}
- **Type**: Cleanup

## Execution Instructions
{}

## Available Tools
{}

## Cleanup Items
- Remove unnecessary files and comments
- Format the code properly
- Update documentation
- Perform final operation verification

## Completion Criteria
{}

Execute cleanup and organize the project.
"#,
        step.id,
        step.description,
        step.prompt_template.as_deref().unwrap_or(&step.description),
        step.required_tools.join(", "),
        step.validation_criteria.join("\n- ")
    )
}
