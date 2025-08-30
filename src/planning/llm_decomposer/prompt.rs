use super::ProjectContext;
use crate::planning::task_types::TaskClassification;

pub(crate) fn build_decomposition_prompt(
    task_description: &str,
    classification: &TaskClassification,
    project_context: &ProjectContext,
) -> String {
    format!(
        r#"
# Task Decomposition Expert System

You are an experienced software engineering expert.
You specialize in decomposing complex tasks into actionable, concrete steps.

## Task to Decompose
**Task**: {}

## Task Classification Information
- **Type**: {:?}
- **Complexity**: {:.2}/1.0
- **Estimated Steps**: {}
- **Risk Level**: {:?}
- **Confidence**: {:.1}%

## Project Context
- **Project Type**: {}
- **Main Languages**: {}
- **Key Files**: {}
- **Architecture**: {}

## Available Tools
- fs_read: Read files
- fs_write: Write files
- edit: Edit files (partial changes)
- search_text: Search text
- find_file: Find files
- get_symbol_info: Get symbol information
- search_repomap: Search repository map
- execute_bash: Execute shell commands
- create_patch: Create patches
- apply_patch: Apply patches

## Decomposition Requirements

以下のJSON形式で、実行可能なステップに分解してください：

```json
{{
  "reasoning": "分解の理由と戦略の説明",
  "complexity_assessment": "複雑さの詳細評価",
  "risks": ["リスク1", "リスク2"],
  "prerequisites": ["前提条件1", "前提条件2"],
  "steps": [
    {{
      "id": "step_1",
      "description": "ステップの簡潔な説明",
      "step_type": "analysis|planning|implementation|validation|cleanup",
      "dependencies": [],
      "estimated_duration_minutes": 5,
      "required_tools": ["tool1", "tool2"],
      "validation_criteria": ["検証条件1", "検証条件2"],
      "detailed_instructions": "詳細な実行手順",
      "potential_issues": ["潜在的な問題1", "潜在的な問題2"]
    }}
  ]
}}
```

## Key Guidelines

1. **Specificity**: Each step must be clear and actionable
2. **Gradualism**: Complex tasks should be divided into small steps
3. **Dependencies**: Clearly define dependencies between steps
4. **Validation**: Set appropriate validation criteria for each step
5. **Risk Management**: Identify potential issues in advance
6. **Tool Utilization**: Effectively use available tools
7. **Time Estimation**: Provide realistic time estimates

Pay special attention to the following points:
- Always analyze the current state before changing files
- Execute large changes in stages
- Validate with compilation/testing at each stage
- Consider backup and rollback procedures
- Include error handling and recovery procedures

Respond in JSON format.
"#,
        task_description,
        classification.task_type,
        classification.complexity_score,
        classification.estimated_steps,
        classification.risk_level,
        classification.confidence * 100.0,
        project_context.project_type,
        project_context.main_languages.join(", "),
        project_context.key_files.join(", "),
        project_context.architecture_notes
    )
}
