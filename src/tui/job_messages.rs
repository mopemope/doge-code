//! Scope the legacy string sender at the foreground owner boundary. The
//! forwarding thread owns only UI messages and exits when the job's senders
//! drop; its guard joins it before JobManager publishes terminal completion.
use std::sync::mpsc::{Sender, channel};

pub(crate) struct Forwarder(Option<std::thread::JoinHandle<()>>);
impl Drop for Forwarder {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            let _ = thread.join();
        }
    }
}
pub(crate) fn scoped_sender(
    tx: Option<Sender<String>>,
    id: crate::jobs::JobId,
) -> (Option<Sender<String>>, Forwarder) {
    let Some(tx) = tx else {
        return (None, Forwarder(None));
    };
    let (sender, rx) = channel::<String>();
    let thread = std::thread::spawn(move || {
        for message in rx {
            if tx.send(format!("::job_message:{id}:{message}")).is_err() {
                break;
            }
        }
    });
    (Some(sender), Forwarder(Some(thread)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_messages_drain_before_completion_and_preserve_body() {
        let (tx, rx) = channel();
        let forwarder;
        let sender;
        (sender, forwarder) = scoped_sender(Some(tx), crate::jobs::JobId(7));
        sender
            .as_ref()
            .unwrap()
            .send("::status:done:answer:with:colons".into())
            .unwrap();
        drop(sender);
        drop(forwarder);
        assert_eq!(
            rx.recv().unwrap(),
            "::job_message:job-7:::status:done:answer:with:colons"
        );
        assert!(rx.try_recv().is_err());
    }
}
