//! UI channel utilities for safe message sending.
//!
//! This module provides helper functions for sending messages to the UI channel
//! with proper error handling and logging.

use std::sync::mpsc::Sender;
use tracing::warn;

/// Send a message to the UI channel with proper error handling.
///
/// If the channel is disconnected, logs a warning instead of silently ignoring.
#[inline]
pub fn send_ui_message(tx: &Sender<String>, msg: impl Into<String>) {
    if tx.send(msg.into()).is_err() {
        warn!("UI channel disconnected, message not sent");
    }
}

/// Send a formatted message to the UI channel.
///
/// This is a convenience macro that formats a message and sends it to the UI channel.
#[macro_export]
macro_rules! send_ui {
    ($tx:expr, $($arg:tt)*) => {
        $crate::tui::channel::send_ui_message($tx, format!($($arg)*))
    };
}

/// Extension trait for Sender to add ergonomic error handling.
pub trait SenderExt {
    /// Send a message, logging a warning if the channel is disconnected.
    fn send_logged(&self, msg: impl Into<String>);
}

impl SenderExt for Sender<String> {
    #[inline]
    fn send_logged(&self, msg: impl Into<String>) {
        send_ui_message(self, msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn test_send_ui_message_success() {
        let (tx, rx) = channel();
        send_ui_message(&tx, "test message");
        assert_eq!(rx.recv().unwrap(), "test message");
    }

    #[test]
    fn test_send_ui_message_disconnected() {
        let (tx, rx) = channel::<String>();
        drop(rx); // Disconnect receiver
        // Should not panic, just logs a warning
        send_ui_message(&tx, "test message");
    }

    #[test]
    fn test_sender_ext_trait() {
        let (tx, rx) = channel();
        tx.send_logged("test via trait");
        assert_eq!(rx.recv().unwrap(), "test via trait");
    }
}
