//! 带链路上下文的后台任务。
//!
//! `tokio::spawn` 出来的新任务**不继承**两样东西：
//!
//! 1. task-local 的 [`CURRENT_TRACE_ID`](crate::telemetry::scope)——因此
//!    `try_current_trace_id()` 在新任务里返回 `None`，日志落到 `trace_id: "unknown"`；
//! 2. 当前 `tracing` span——因此事件不带 `spans`，`method` / `path` 一起丢掉。
//!
//! 只做 `tokio::spawn(future.instrument(Span::current()))` 是不够的：`spans` 有了，
//! 但 `trace_id` 仍是 `unknown`（`Instrument` 只管 span，不管 task-local）。所以这里
//! 在新任务**内部**再套一层 [`scope`]，两者一起搬过去，效果等价于「在请求内同步执行」。
//!
//! 典型用途：把「不阻塞响应」的异步写库（审计日志、埋点落库）挂到当前请求的链路上，
//! 事后能按 `trace_id` 把一次请求和它产生的后台语句串起来。

use std::future::Future;

use tokio::task::JoinHandle;
use tracing::Instrument as _;

use crate::telemetry::layer::{TraceId, scope, try_current_trace_id};

/// 在「当前 span + 当前 trace-id」上下文内启动后台任务。
///
/// 语义与 [`tokio::spawn`] 一致（返回 [`JoinHandle`]、任务 panic 不影响调用方），
/// 只是把调用点的 span 与 trace-id 一并带进新任务。
///
/// 没有可继承的上下文时（启动期、后台轮询任务）退化为普通 `spawn`，日志里
/// `trace_id` 仍是 `unknown`——那里本来就没有链路可言，不伪造 id。
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_in_trace(try_current_trace_id(), future)
}

/// 显式给出 trace-id 的版本；`None` 时只继承当前 span。
///
/// 给**已经离开请求作用域**的位置用：例如挂在请求栈最外侧的中间件，它在
/// `next.run(req).await` 返回之后才 spawn，此时 task-local 已随作用域结束而回收，
/// 只能从响应头 `X-Trace-Id` 把 id 取回来再传进来。
pub fn spawn_in_trace<F>(trace_id: Option<TraceId>, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // 必须在 spawn 之前取：新任务里 `Span::current()` 已经是空的
    let span = tracing::Span::current();
    tokio::spawn(async move {
        let future = future.instrument(span);
        match trace_id {
            Some(trace_id) => scope(trace_id, future).await,
            None => future.await,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn should_propagate_trace_id_across_spawn() {
        let trace_id = TraceId::new();
        let expected = trace_id.clone();
        let actual = scope(trace_id, async move {
            spawn(async { try_current_trace_id() })
                .await
                .expect("任务不应 panic")
        })
        .await;
        assert_eq!(actual, Some(expected), "新任务必须继承调用点的 trace-id");
    }

    #[tokio::test]
    async fn should_return_none_outside_scope() {
        let actual = spawn(async { try_current_trace_id() })
            .await
            .expect("任务不应 panic");
        assert!(actual.is_none(), "作用域外不得伪造 trace-id");
    }

    #[tokio::test]
    async fn should_use_explicit_trace_id() {
        let trace_id = TraceId::new();
        let expected = trace_id.clone();
        let actual = spawn_in_trace(Some(trace_id), async { try_current_trace_id() })
            .await
            .expect("任务不应 panic");
        assert_eq!(actual, Some(expected), "显式 trace-id 必须生效");
    }
}
