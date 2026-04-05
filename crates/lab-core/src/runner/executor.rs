use std::future::Future;
use std::pin::Pin;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::Result;

/// Core async future type — type-erased for composition.
///
/// Inspired by act's `type Executor func(ctx context.Context) error`.
pub type BoxFuture<'a, T = Result<()>> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A composable async unit of work.
pub type Executor = Box<dyn FnOnce(ExecutorCtx) -> BoxFuture<'static> + Send>;

/// Shared context threaded through chains.
#[derive(Clone)]
pub struct ExecutorCtx {
    cancelled: Arc<AtomicBool>,
}

impl ExecutorCtx {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Default for ExecutorCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a no-op.
pub fn noop() -> Executor {
    Box::new(|_ctx| Box::pin(async { Ok(()) }))
}

/// Create from an async function.
pub fn from_fn<F, Fut>(f: F) -> Executor
where
    F: FnOnce(ExecutorCtx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    Box::new(move |ctx| Box::pin(f(ctx)))
}

/// Run executors sequentially. Stops on first error.
///
/// Equivalent to act's `NewPipelineExecutor()`.
pub fn pipeline(executors: Vec<Executor>) -> Executor {
    Box::new(move |ctx: ExecutorCtx| {
        Box::pin(async move {
            for ex in executors {
                if ctx.is_cancelled() {
                    return Err(crate::error::LabError::Other("cancelled".into()));
                }
                ex(ctx.clone()).await?;
            }
            Ok(())
        })
    })
}

/// Run executors concurrently with a semaphore limit.
///
/// Equivalent to act's `NewParallelExecutor()`.
pub fn parallel(executors: Vec<Executor>, max_concurrent: usize) -> Executor {
    Box::new(move |ctx: ExecutorCtx| {
        Box::pin(async move {
            let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrent));
            let mut handles = Vec::new();

            for ex in executors {
                let sem = semaphore.clone();
                let ctx = ctx.clone();
                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    ex(ctx).await
                });
                handles.push(handle);
            }

            let mut errors = Vec::new();
            for handle in handles {
                match handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => errors.push(e),
                    Err(e) => errors.push(crate::error::LabError::Other(e.to_string())),
                }
            }

            if let Some(first_error) = errors.into_iter().next() {
                Err(first_error)
            } else {
                Ok(())
            }
        })
    })
}

/// Chain: run `first`, then `second` only if `first` succeeds.
pub fn then(first: Executor, second: Executor) -> Executor {
    Box::new(move |ctx: ExecutorCtx| {
        Box::pin(async move {
            first(ctx.clone()).await?;
            second(ctx).await
        })
    })
}

/// Run `main_fn`, then always run `cleanup` regardless of outcome.
pub fn finally(main_fn: Executor, cleanup: Executor) -> Executor {
    Box::new(move |ctx: ExecutorCtx| {
        Box::pin(async move {
            let result = main_fn(ctx.clone()).await;
            let cleanup_result = cleanup(ctx).await;
            result.and(cleanup_result)
        })
    })
}

/// Conditionally run an executor.
pub fn when<F>(condition: F, ex: Executor) -> Executor
where
    F: FnOnce(&ExecutorCtx) -> bool + Send + 'static,
{
    Box::new(move |ctx: ExecutorCtx| {
        Box::pin(async move {
            if condition(&ctx) {
                ex(ctx).await
            } else {
                Ok(())
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_pipeline_runs_sequentially() {
        let counter = Arc::new(AtomicU32::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();

        let ex = pipeline(vec![
            from_fn(move |_| async move {
                c1.store(1, Ordering::SeqCst);
                Ok(())
            }),
            from_fn(move |_| async move {
                assert_eq!(c2.load(Ordering::SeqCst), 1);
                c2.store(2, Ordering::SeqCst);
                Ok(())
            }),
        ]);

        ex(ExecutorCtx::new()).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_parallel_runs_all() {
        let counter = Arc::new(AtomicU32::new(0));

        let executors: Vec<Executor> = (0..4)
            .map(|_| {
                let c = counter.clone();
                from_fn(move |_| async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .collect();

        let ex = parallel(executors, 2);
        ex(ExecutorCtx::new()).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn test_finally_runs_on_error() {
        let cleanup_ran = Arc::new(AtomicU32::new(0));
        let c = cleanup_ran.clone();

        let ex = finally(
            from_fn(|_| async { Err(crate::error::LabError::Other("fail".into())) }),
            from_fn(move |_| async move {
                c.store(1, Ordering::SeqCst);
                Ok(())
            }),
        );

        let result = ex(ExecutorCtx::new()).await;
        assert!(result.is_err());
        assert_eq!(cleanup_ran.load(Ordering::SeqCst), 1);
    }
}
