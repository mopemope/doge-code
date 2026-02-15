pub const IGNORE_FILE: &str = ".dogeignore";

pub mod llm;
pub use llm::*;
pub mod verification;
pub use verification::*;
pub mod watch;
pub use watch::*;
pub mod mcp;
pub use mcp::*;
pub mod test_fix;
pub use test_fix::*;

pub mod app;
pub use app::*;
pub mod loading;
pub use loading::*;

#[cfg(test)]
mod tests;
