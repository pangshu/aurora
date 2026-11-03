//! 渲染结果（[`Rendered`]）。
//!
//! 不直接用 `axum::response::Html<String>`：那是个「只管 body」的类型，
//! 既不能携带状态码，也不方便与统一错误体区分。自有类型让 handler 签名
//! 一眼能看出「这个端点返回 HTML 而不是 `ApiResponse`」。

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// 已渲染的 HTML 响应（`text/html; charset=utf-8`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    body: String,
    status: StatusCode,
}

impl Rendered {
    /// 以 `200 OK` 构造。
    #[must_use]
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            status: StatusCode::OK,
        }
    }

    /// 指定状态码（如「渲染成功但业务上要 201 / 404」）。
    #[must_use]
    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// 响应体。
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// 取出响应体。
    #[must_use]
    pub fn into_body(self) -> String {
        self.body
    }

    /// 状态码。
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for Rendered {
    fn into_response(self) -> Response {
        // 手工装配而不是 `(status, [(CONTENT_TYPE, ...)], body)`：
        // 后者依赖「header 部件晚于 body 应用」的实现细节，一旦顺序变了
        // 就会退化成 `text/plain`——浏览器直接把 HTML 当文本显示。
        let mut response = self.body.into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        *response.status_mut() = self.status;
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn should_render_as_html() {
        let response = Rendered::new("<h1>hi</h1>").into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8"),
            "charset 必须显式声明，否则中文在部分浏览器下乱码"
        );
    }

    #[tokio::test]
    async fn should_keep_custom_status() {
        let response = Rendered::new("").with_status(StatusCode::NOT_FOUND).into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
