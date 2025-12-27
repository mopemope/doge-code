//! Autonomous error recovery and self-debugging system for Doge-Code
//! This module provides comprehensive error detection, diagnosis, and recovery capabilities

pub mod diagnosis;
pub mod strategies;
pub mod tool;
pub mod types;

pub use diagnosis::*;
pub use strategies::*;
pub use tool::*;
pub use types::*;
