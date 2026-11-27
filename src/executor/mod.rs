pub mod compiler;
pub mod manager;
pub mod runner;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ExecutionError {
    #[error("宏执行已停止")]
    Interrupted,
    #[error("宏执行超时")]
    TimedOut,
    #[error("{0}")]
    Failed(String),
}
