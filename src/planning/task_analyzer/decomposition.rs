mod code_edit;
mod fallback;
mod feature_impl;
mod file_operation;
mod multi_file_edit;
mod refactor;
mod search;

pub(crate) use code_edit::decompose_code_edit;
pub(crate) use fallback::decompose_complex_fallback;
pub(crate) use feature_impl::decompose_feature_implementation;
pub(crate) use file_operation::decompose_file_operation;
pub(crate) use multi_file_edit::decompose_multi_file_edit;
pub(crate) use refactor::decompose_refactoring;
pub(crate) use search::decompose_search_task;
