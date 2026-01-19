// Event handlers split into submodules

mod file_search;
mod history_search;
mod normal;

pub use file_search::handle_file_search_key;
pub use history_search::handle_history_search_key;
pub use normal::handle_normal_mode_key;
