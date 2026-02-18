pub const IGNORE_FILE: &str = ".dogeignore";

pub mod llm;
pub use llm::*;

pub mod watch;
pub use watch::*;
pub mod mcp;
pub use mcp::*;

pub mod app;
pub use app::*;
pub mod loading;
pub use loading::*;

#[cfg(test)]
mod tests;
