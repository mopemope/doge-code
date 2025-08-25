use crate::planning::task_types::*;
use tracing::info;

pub(crate) fn decompose_file_operation(input: &str) -> Vec<TaskStep> {
    if input.contains("読") || input.contains("read") || input.contains("表示") {
        vec![
            TaskStep::new(
                "identify_target".to_string(),
                "Identify target file".to_string(),
                StepType::Analysis,
                vec!["find_file".to_string()],
            )
            .with_duration(30),
            TaskStep::new(
                "read_file".to_string(),
                "Read file".to_string(),
                StepType::Implementation,
                vec!["fs_read".to_string()],
            )
            .with_dependencies(vec!["identify_target".to_string()])
            .with_duration(30),
        ]
    } else {
        vec![
            TaskStep::new(
                "prepare_content".to_string(),
                "Prepare write content".to_string(),
                StepType::Planning,
                vec![],
            )
            .with_duration(60),
            TaskStep::new(
                "write_file".to_string(),
                "Write to file".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string()],
            )
            .with_dependencies(vec!["prepare_content".to_string()])
            .with_duration(60),
        ]
    }
}

pub(crate) fn decompose_search_task(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "define_search_criteria".to_string(),
            "Define search criteria".to_string(),
            StepType::Planning,
            vec![],
        )
        .with_duration(30),
        TaskStep::new(
            "execute_search".to_string(),
            "Execute search".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "find_file".to_string()],
        )
        .with_dependencies(vec!["define_search_criteria".to_string()])
        .with_duration(60),
        TaskStep::new(
            "analyze_results".to_string(),
            "Analyze search results".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["execute_search".to_string()])
        .with_duration(90),
    ]
}

pub(crate) fn decompose_code_edit(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_target".to_string(),
            "Analyze target code".to_string(),
            StepType::Analysis,
            vec!["fs_read".to_string(), "get_symbol_info".to_string()],
        )
        .with_duration(60)
        .with_validation(vec!["File exists".to_string()]),
        TaskStep::new(
            "plan_changes".to_string(),
            "Create change plan".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_target".to_string()])
        .with_duration(90),
        TaskStep::new(
            "implement_changes".to_string(),
            "Implement changes".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["plan_changes".to_string()])
        .with_duration(180)
        .with_validation(vec!["No syntax errors".to_string()]),
        TaskStep::new(
            "validate_changes".to_string(),
            "Validate changes".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["implement_changes".to_string()])
        .with_duration(120)
        .with_validation(vec!["Compilation successful".to_string()]),
    ]
}

pub(crate) fn decompose_multi_file_edit(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_project_structure".to_string(),
            "Analyze project structure".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
        )
        .with_duration(120),
        TaskStep::new(
            "identify_affected_files".to_string(),
            "Identify affected files".to_string(),
            StepType::Analysis,
            vec!["search_text".to_string()],
        )
        .with_dependencies(vec!["analyze_project_structure".to_string()])
        .with_duration(90),
        TaskStep::new(
            "plan_coordinated_changes".to_string(),
            "Create coordinated change plan".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["identify_affected_files".to_string()])
        .with_duration(180),
        TaskStep::new(
            "implement_changes_sequentially".to_string(),
            "Implement changes sequentially".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["plan_coordinated_changes".to_string()])
        .with_duration(300),
        TaskStep::new(
            "validate_integration".to_string(),
            "Execute integration tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["implement_changes_sequentially".to_string()])
        .with_duration(180),
    ]
}

pub(crate) fn decompose_refactoring(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_current_structure".to_string(),
            "Analyze current code structure in detail".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "fs_read".to_string()],
        )
        .with_duration(180),
        TaskStep::new(
            "identify_refactoring_opportunities".to_string(),
            "Identify refactoring opportunities".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_current_structure".to_string()])
        .with_duration(120),
        TaskStep::new(
            "design_new_structure".to_string(),
            "Design new structure".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["identify_refactoring_opportunities".to_string()])
        .with_duration(240),
        TaskStep::new(
            "implement_refactoring".to_string(),
            "Implement refactoring".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["design_new_structure".to_string()])
        .with_duration(600),
        TaskStep::new(
            "update_dependencies".to_string(),
            "Update dependencies".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["implement_refactoring".to_string()])
        .with_duration(300),
        TaskStep::new(
            "run_comprehensive_tests".to_string(),
            "Execute comprehensive tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["update_dependencies".to_string()])
        .with_duration(240),
    ]
}

pub(crate) fn decompose_feature_implementation(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_requirements".to_string(),
            "Analyze requirements".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_duration(120),
        TaskStep::new(
            "design_architecture".to_string(),
            "Design architecture".to_string(),
            StepType::Planning,
            vec!["search_repomap".to_string()],
        )
        .with_dependencies(vec!["analyze_requirements".to_string()])
        .with_duration(180),
        TaskStep::new(
            "implement_core_logic".to_string(),
            "Implement core logic".to_string(),
            StepType::Implementation,
            vec!["fs_write".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["design_architecture".to_string()])
        .with_duration(480),
        TaskStep::new(
            "implement_interfaces".to_string(),
            "Implement interfaces".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["implement_core_logic".to_string()])
        .with_duration(240),
        TaskStep::new(
            "add_tests".to_string(),
            "Add tests".to_string(),
            StepType::Implementation,
            vec!["fs_write".to_string()],
        )
        .with_dependencies(vec!["implement_interfaces".to_string()])
        .with_duration(300),
        TaskStep::new(
            "validate_feature".to_string(),
            "Validate feature".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["add_tests".to_string()])
        .with_duration(180),
        TaskStep::new(
            "integration_test".to_string(),
            "Execute integration tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["validate_feature".to_string()])
        .with_duration(240),
    ]
}

pub(crate) fn decompose_complex_fallback(
    user_input: &str,
    classification: &TaskClassification,
) -> Vec<TaskStep> {
    info!("Using fallback decomposition for complex task");

    match classification.task_type {
        TaskType::ArchitecturalChange => decompose_architectural_change_fallback(user_input),
        TaskType::LargeRefactoring => decompose_large_refactoring_fallback(user_input),
        TaskType::ProjectRestructure => decompose_project_restructure_fallback(user_input),
        _ => {
            // Generic complex task decomposition
            vec![
                TaskStep::new(
                    "analyze_requirements".to_string(),
                    "Analyze requirements and current state in detail".to_string(),
                    StepType::Analysis,
                    vec!["search_repomap".to_string(), "fs_read".to_string()],
                )
                .with_duration(300),
                TaskStep::new(
                    "create_detailed_plan".to_string(),
                    "Create detailed execution plan".to_string(),
                    StepType::Planning,
                    vec!["get_symbol_info".to_string()],
                )
                .with_dependencies(vec!["analyze_requirements".to_string()])
                .with_duration(600),
                TaskStep::new(
                    "implement_incrementally".to_string(),
                    "Implement incrementally".to_string(),
                    StepType::Implementation,
                    vec!["edit".to_string(), "fs_write".to_string()],
                )
                .with_dependencies(vec!["create_detailed_plan".to_string()])
                .with_duration(1800),
                TaskStep::new(
                    "comprehensive_testing".to_string(),
                    "Comprehensive testing and validation".to_string(),
                    StepType::Validation,
                    vec!["execute_bash".to_string()],
                )
                .with_dependencies(vec!["implement_incrementally".to_string()])
                .with_duration(600)
                .with_validation(vec![
                    "All tests pass".to_string(),
                    "Compilation successful".to_string(),
                ]),
            ]
        }
    }
}

fn decompose_architectural_change_fallback(_user_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_current_architecture".to_string(),
            "Analyze current architecture in detail".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
        )
        .with_duration(600),
        TaskStep::new(
            "design_new_architecture".to_string(),
            "Design new architecture".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_current_architecture".to_string()])
        .with_duration(900),
        TaskStep::new(
            "create_migration_plan".to_string(),
            "Create migration plan".to_string(),
            StepType::Planning,
            vec!["search_text".to_string()],
        )
        .with_dependencies(vec!["design_new_architecture".to_string()])
        .with_duration(600),
        TaskStep::new(
            "implement_core_changes".to_string(),
            "Implement core changes".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["create_migration_plan".to_string()])
        .with_duration(2400),
        TaskStep::new(
            "update_dependent_modules".to_string(),
            "Update dependent modules".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["implement_core_changes".to_string()])
        .with_duration(1800),
        TaskStep::new(
            "comprehensive_integration_test".to_string(),
            "Comprehensive integration testing".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["update_dependent_modules".to_string()])
        .with_duration(900)
        .with_validation(vec![
            "All tests pass".to_string(),
            "Architecture works correctly".to_string(),
        ]),
    ]
}

fn decompose_large_refactoring_fallback(_user_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "comprehensive_code_analysis".to_string(),
            "Comprehensive code analysis".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "search_text".to_string()],
        )
        .with_duration(600),
        TaskStep::new(
            "identify_refactoring_targets".to_string(),
            "Identify refactoring targets".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["comprehensive_code_analysis".to_string()])
        .with_duration(450),
        TaskStep::new(
            "prioritize_refactoring_tasks".to_string(),
            "Prioritize refactoring tasks".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["identify_refactoring_targets".to_string()])
        .with_duration(300),
        TaskStep::new(
            "refactor_high_priority_modules".to_string(),
            "Refactor high-priority modules".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "create_patch".to_string()],
        )
        .with_dependencies(vec!["prioritize_refactoring_tasks".to_string()])
        .with_duration(2100),
        TaskStep::new(
            "update_tests_and_documentation".to_string(),
            "Update tests and documentation".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["refactor_high_priority_modules".to_string()])
        .with_duration(900),
        TaskStep::new(
            "validate_refactoring_results".to_string(),
            "Validate refactoring results".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["update_tests_and_documentation".to_string()])
        .with_duration(600)
        .with_validation(vec![
            "All tests pass".to_string(),
            "Code quality improved".to_string(),
        ]),
    ]
}

fn decompose_project_restructure_fallback(_user_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_current_structure".to_string(),
            "Analyze current project structure".to_string(),
            StepType::Analysis,
            vec!["fs_list".to_string(), "search_repomap".to_string()],
        )
        .with_duration(450),
        TaskStep::new(
            "design_new_structure".to_string(),
            "Design new project structure".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_current_structure".to_string()])
        .with_duration(600),
        TaskStep::new(
            "create_backup_plan".to_string(),
            "Create backup plan".to_string(),
            StepType::Planning,
            vec!["fs_list".to_string()],
        )
        .with_dependencies(vec!["design_new_structure".to_string()])
        .with_duration(300),
        TaskStep::new(
            "create_new_directories".to_string(),
            "Create new directory structure".to_string(),
            StepType::Implementation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["create_backup_plan".to_string()])
        .with_duration(300),
        TaskStep::new(
            "move_and_reorganize_files".to_string(),
            "Move and reorganize files".to_string(),
            StepType::Implementation,
            vec!["execute_bash".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["create_new_directories".to_string()])
        .with_duration(1800),
        TaskStep::new(
            "update_import_paths".to_string(),
            "Update import paths".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["move_and_reorganize_files".to_string()])
        .with_duration(1200),
        TaskStep::new(
            "update_build_configuration".to_string(),
            "Update build configuration".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["update_import_paths".to_string()])
        .with_duration(600),
        TaskStep::new(
            "comprehensive_build_test".to_string(),
            "Comprehensive build testing".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["update_build_configuration".to_string()])
        .with_duration(900)
        .with_validation(vec![
            "Project builds successfully".to_string(),
            "All tests pass".to_string(),
        ]),
    ]
}
