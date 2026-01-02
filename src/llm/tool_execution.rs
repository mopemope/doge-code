mod agent_loop;
pub mod compaction;
mod diff_collection;
mod dispatch;
mod error;
mod requests;
mod streaming;
pub mod ui_rendering;

pub use agent_loop::run_agent_loop;
pub use diff_collection::collect_diff_review_payload;
pub use dispatch::dispatch_tool_call;
pub use streaming::run_agent_streaming_once;
