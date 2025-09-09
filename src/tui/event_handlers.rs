// Event handlers split into submodules

mod normal;
mod shell;

pub use normal::handle_normal_mode_key;
pub use shell::handle_shell_mode_key;
