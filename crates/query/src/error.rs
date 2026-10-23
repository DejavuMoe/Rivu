//! 查询错误：校验错误（将来的 400）、超时与资源（503 / 429）、内部错误（500，不向外暴露细节）。

/// 查询内核的错误。路由层按变体映射状态码，见 [`QueryError::http_status_hint`]。
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// 请求不合法：`path` 是出错字段的路径（如 `filters[0][2]`，顶层问题为空串），`message` 是面向调用方的英文说明。
    #[error("invalid query{}: {message}", if path.is_empty() { String::new() } else { format!(" at `{path}`") })]
    Validation { path: String, message: String },
    /// ClickHouse 的执行时限（`max_execution_time`）到了。
    #[error("the query exceeded its time limit")]
    Timeout,
    /// 内存、行数或并发等资源限制。
    #[error("the query exceeded a resource limit: {0}")]
    ResourceLimit(String),
    /// 其余错误（连接、协议、意外的服务端响应）。消息只写日志，不返回给调用方。
    #[error("internal query error: {0:#}")]
    Internal(#[source] anyhow::Error),
}

impl QueryError {
    pub fn validation(path: impl Into<String>, message: impl Into<String>) -> Self {
        QueryError::Validation {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn internal(err: impl Into<anyhow::Error>) -> Self {
        QueryError::Internal(err.into())
    }

    /// 将来路由层使用的状态码：校验 400、超时 503、资源限制 429、内部 500。
    pub fn http_status_hint(&self) -> u16 {
        match self {
            QueryError::Validation { .. } => 400,
            QueryError::Timeout => 503,
            QueryError::ResourceLimit(_) => 429,
            QueryError::Internal(_) => 500,
        }
    }

    /// 校验错误的 `(path, message)`。
    pub fn as_validation(&self) -> Option<(&str, &str)> {
        match self {
            QueryError::Validation { path, message } => Some((path, message)),
            _ => None,
        }
    }
}
