//! Owned, bounded blocking I/O with cooperative cancellation.
use anyhow::Result;
use std::io::{self, Read};
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub(crate) fn check(cancel: Option<&CancellationToken>) -> Result<()> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
    }
    Ok(())
}

pub(crate) struct CancelOnDrop(pub CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) async fn blocking<T, F>(parent: CancellationToken, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(CancellationToken) -> Result<T> + Send + 'static,
{
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let guard = CancelOnDrop(parent.child_token());
    check(Some(&guard.0))?;
    let permit = tokio::select! {
        biased;
        _ = guard.0.cancelled() => { check(Some(&guard.0))?; unreachable!() }
        permit = WORKERS.get_or_init(|| Arc::new(Semaphore::new(4))).clone().acquire_owned() => permit?,
    };
    check(Some(&guard.0))?;
    let token = guard.0.clone();
    // Never race cancellation against the join: normal cancellation waits for
    // the worker to observe its token. Drop requests stop, with no side effects
    // or context updates owned by the detached worker.
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        check(Some(&token))?;
        let result = work(token.clone());
        check(Some(&token))?;
        result
    })
    .await?;
    check(Some(&guard.0))?;
    result
}

pub(crate) struct CancellableReader<'a, R> {
    pub inner: R,
    pub cancel: Option<&'a CancellationToken>,
}
impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        check(self.cancel).map_err(io::Error::other)?;
        let count = self.inner.read(bytes)?;
        check(self.cancel).map_err(io::Error::other)?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn canceled(error: &anyhow::Error) {
        assert!(
            matches!(
                error.downcast_ref::<crate::llm::LlmErrorKind>(),
                Some(crate::llm::LlmErrorKind::Cancelled)
            ),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellable_io_keeps_timer_live_and_joins_worker() {
        let token = CancellationToken::new();
        let stopped = Arc::new(AtomicBool::new(false));
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker_stopped = stopped.clone();
        let work = tokio::spawn(blocking(token.clone(), move |cancel| {
            let _ = started.send(());
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            // Cancellation must wait for completion, even after notification.
            std::thread::sleep(Duration::from_millis(30));
            worker_stopped.store(true, Ordering::SeqCst);
            Ok(())
        }));
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_millis(5)),
        )
        .await
        .unwrap();
        token.cancel();
        canceled(
            &tokio::time::timeout(Duration::from_secs(2), work)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
        );
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancellable_io_precancel_does_not_start_and_drop_requests_stop() {
        let token = CancellationToken::new();
        token.cancel();
        canceled(
            &blocking(token, |_| -> Result<()> {
                panic!("precancel worker started")
            })
            .await
            .unwrap_err(),
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let work = tokio::spawn(blocking(CancellationToken::new(), move |cancel| {
            let _ = started.send(());
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            worker_stopped.store(true, Ordering::SeqCst);
            Ok(())
        }));
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        work.abort();
        let _ = work.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !stopped.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn cancellable_io_checks_each_scanner_chunk() {
        struct CancelAfterRead {
            token: CancellationToken,
            reads: usize,
        }
        impl Read for CancelAfterRead {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                assert_eq!(self.reads, 1);
                bytes.fill(b'x');
                self.token.cancel();
                Ok(bytes.len())
            }
        }
        let token = CancellationToken::new();
        let reader = CancellableReader {
            inner: CancelAfterRead {
                token: token.clone(),
                reads: 0,
            },
            cancel: Some(&token),
        };
        assert!(super::super::text_scan::read_page(reader, 0, 1, 20).is_err());
    }

    #[tokio::test]
    async fn cancellable_io_read_routes_preserve_utf8_and_precancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        std::fs::write(&path, b"ok\n\xff").unwrap();
        let config = Arc::new(crate::config::AppConfig {
            project_root: dir.path().to_owned(),
            ..Default::default()
        });
        let options = super::super::read::FsReadOptions {
            limit: Some(1),
            ..Default::default()
        };
        assert!(
            super::super::read::fs_read_async(
                path.display().to_string(),
                options,
                config.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        let token = CancellationToken::new();
        token.cancel();
        canceled(
            &super::super::read_many::fs_read_many_files_async(
                vec!["[".into()],
                None,
                None,
                config,
                Default::default(),
                token,
            )
            .await
            .unwrap_err(),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellable_io_no_late_context_while_waiting_for_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let config = Arc::new(crate::config::AppConfig {
            project_root: dir.path().to_owned(),
            ..Default::default()
        });
        let fs = Arc::new(crate::tools::FsTools::new(
            Arc::new(tokio::sync::RwLock::new(None)),
            config,
        ));
        let context_lock = fs.context_manager.write().await;
        let token = CancellationToken::new();
        let run_fs = fs.clone();
        let run_token = token.clone();
        let run = tokio::spawn(async move {
            run_fs
                .fs_read_async(path.display().to_string(), Default::default(), run_token)
                .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        token.cancel();
        canceled(
            &tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
        );
        drop(context_lock);
        tokio::task::yield_now().await;
        assert!(
            fs.context_manager
                .read()
                .await
                .get_context_prompt()
                .await
                .is_empty()
        );
    }
}
