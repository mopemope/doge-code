pub mod commands;
pub mod commands_sessions;

pub mod diff_review;
pub mod event_handlers;
pub mod event_loop;
pub mod llm_response_handler;
pub mod rendering;
pub mod state;
pub mod state_render;
pub mod style_utils;
pub mod theme;
pub mod view;

#[cfg(test)]
mod test_token_display;

#[cfg(test)]
mod test_scroll_basic;
#[cfg(test)]
mod test_scroll_formatting;
#[cfg(test)]
mod test_scroll_logging;
#[cfg(test)]
mod test_scroll_render;

#[cfg(test)]
mod mouse_test;

#[cfg(test)]
mod integration_test;
