//! 业务错误码定义。
//!
//! 错误码分段与 HTTP 映射见 `docs/09-error-and-response-spec.md`；**一码一态**：
//! 每个码唯一映射到一个 HTTP 状态码（[`ErrorCode::status`] 为纯函数）。
//!
//! 框架内置码占用 `x0001..x0999`，`x1000+` 区间留给业务方自定义，
//! 可经 [`ErrorCode::new`] 构造。

use http::StatusCode;

/// 业务错误码。
///
/// 该类型为**不透明 newtype**：外部只能通过 [`ErrorCode::new`] 或本模块提供的
/// 常量构造，避免出现未定义的码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct ErrorCode(i32);

impl ErrorCode {
    /// 成功。
    pub const OK: Self = Self(0);

    // —— 10xxx 通用 / 请求 ——
    /// 参数校验失败（400）。
    pub const INVALID_PARAM: Self = Self(10001);
    /// 请求体过大（413）。
    pub const PAYLOAD_TOO_LARGE: Self = Self(10002);
    /// 请求超时（入站）（408）。
    pub const REQUEST_TIMEOUT: Self = Self(10003);
    /// 不支持的媒体类型（415）。
    pub const UNSUPPORTED_MEDIA_TYPE: Self = Self(10004);
    /// 请求体解析失败（400）。
    pub const MALFORMED_BODY: Self = Self(10005);
    /// 上游调用超时（出站）（504）。
    pub const GATEWAY_TIMEOUT: Self = Self(10006);
    /// 方法不允许（405）。
    pub const METHOD_NOT_ALLOWED: Self = Self(10007);
    /// 密文无效或已被篡改（400）。
    ///
    /// 专用于「认证标签校验失败 / 密文头非法」：这类密文**可能由攻击者提供**
    /// （加密 cookie、加密 query 参数、被替换的密文列），属于预期内的输入错误，
    /// 不能表现成 `50001`——否则既误导排障，又给了「刷 500 触发告警」的手段。
    pub const DECRYPT_FAILED: Self = Self(10008);

    // —— 20xxx 认证 / 授权 ——
    /// 未认证（401）。
    pub const UNAUTHENTICATED: Self = Self(20001);
    /// 无权限（403）。
    pub const FORBIDDEN: Self = Self(20002);
    /// 凭证已过期（401）。
    pub const TOKEN_EXPIRED: Self = Self(20003);
    /// 权限不足（403）。
    pub const PERMISSION_DENIED: Self = Self(20004);

    // —— 30xxx 资源 / 业务 ——
    /// 资源不存在（404）。
    pub const NOT_FOUND: Self = Self(30001);
    /// 资源冲突（409）。
    pub const CONFLICT: Self = Self(30002);
    /// 资源已锁定（423）。
    pub const LOCKED: Self = Self(30003);

    // —— 40xxx 限流 / 风控 ——
    /// 请求过于频繁（429）。
    pub const RATE_LIMITED: Self = Self(40001);
    /// IP 受限（403）。
    ///
    /// 语义上是「拒绝」而非「太快了」：用 429 会和限流共用一个状态码，
    /// WAF 规则与告警无法只凭状态码区分「被封禁」和「超配额」。
    pub const IP_BLOCKED: Self = Self(40002);

    // —— 50xxx 服务端 / 依赖 ——
    /// 服务内部错误（500）。
    pub const INTERNAL: Self = Self(50001);
    /// 数据访问失败（500）。
    pub const DB_ERROR: Self = Self(50002);
    /// 上游依赖返回错误（502）。
    pub const UPSTREAM_ERROR: Self = Self(50003);
    /// 缓存访问失败（500）。
    pub const CACHE_ERROR: Self = Self(50004);
    /// 服务暂不可用（含未就绪）（503）。
    pub const SERVICE_UNAVAILABLE: Self = Self(50005);
    /// 下游熔断中，请求被快速拒绝（503）。
    ///
    /// **本进程自身的拒绝**，不是下游故障：上游不得据此重试，也不得计入自己的
    /// 熔断失败率——否则一次依赖故障会沿调用链级联熔断。
    pub const CIRCUIT_OPEN: Self = Self(50006);
    /// 并发舱壁已满，请求被快速拒绝（503）。
    ///
    /// 同 [`CIRCUIT_OPEN`](Self::CIRCUIT_OPEN)：自身拒绝，不可重试、不计入上游熔断。
    pub const BULKHEAD_FULL: Self = Self(50007);
    /// 模板渲染失败（500）。
    ///
    /// 客户端只收到固定文案；模板名 / 行号 / 引擎明细只进日志（见 `docs/20` §7.2）。
    /// 「模板不存在」默认也走本码——模板名若来自 URL，由业务用
    /// `TemplateError::is_not_found()` 自行转 `30001`。
    pub const TEMPLATE_ERROR: Self = Self(50008);

    /// 由原始 `i32` 构造错误码。
    ///
    /// 业务方在 `x1000+` 区间自定义码值时使用。**非法码值一律归一到
    /// [`ErrorCode::INTERNAL`]**（fail-closed）：
    ///
    /// - 合法区间为 `10000..=59999`（见 `docs/09` §5 的分段表）；
    /// - `0`（成功码）与区间外的值都不能表示错误，归一到 `50001`，
    ///   避免"错误被当成成功返回 200"（fail-open）或落到未知分段。
    ///
    /// 需要严格校验时用 [`ErrorCode::try_new`]。
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        if Self::is_valid_raw(raw) {
            Self(raw)
        } else {
            Self::INTERNAL
        }
    }

    /// 严格构造：码值非法时返回 `None`。
    ///
    /// 与 [`ErrorCode::new`] 的区别是**不静默归一**，便于在配置校验、
    /// 反序列化等入口把非法码值当错误上报。
    #[must_use]
    pub const fn try_new(raw: i32) -> Option<Self> {
        if Self::is_valid_raw(raw) {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// 该码值是否落在合法**错误**码区间（`10000..=59999`）。
    ///
    /// 注意 [`ErrorCode::OK`]（`0`）不是错误码，返回 `false`。
    #[must_use]
    pub const fn is_valid(self) -> bool {
        Self::is_valid_raw(self.0)
    }

    /// 合法码值判定（`const`，供 [`ErrorCode::new`] 使用）。
    const fn is_valid_raw(raw: i32) -> bool {
        raw >= 10_000 && raw <= 59_999
    }

    /// 返回底层 `i32` 码值。
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// 返回该码唯一对应的 HTTP 状态码（一码一态）。
    ///
    /// 未登记的自定义码按其所属分段回退到一个合理的状态码。
    /// 码值经 [`ErrorCode::new`] / [`ErrorCode::try_new`] 构造后必然落在
    /// `10000..=59999`，因此回退分支只可能命中该区间。
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self.0 {
            0 => StatusCode::OK,
            10001 | 10005 => StatusCode::BAD_REQUEST,
            10002 => StatusCode::PAYLOAD_TOO_LARGE,
            10003 => StatusCode::REQUEST_TIMEOUT,
            10004 => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            10006 => StatusCode::GATEWAY_TIMEOUT,
            10007 => StatusCode::METHOD_NOT_ALLOWED,
            10008 => StatusCode::BAD_REQUEST,
            20001 | 20003 => StatusCode::UNAUTHORIZED,
            20002 | 20004 => StatusCode::FORBIDDEN,
            30001 => StatusCode::NOT_FOUND,
            30002 => StatusCode::CONFLICT,
            30003 => StatusCode::LOCKED,
            40001 => StatusCode::TOO_MANY_REQUESTS,
            40002 => StatusCode::FORBIDDEN,
            50001 | 50002 | 50004 => StatusCode::INTERNAL_SERVER_ERROR,
            50003 => StatusCode::BAD_GATEWAY,
            50005..=50007 => StatusCode::SERVICE_UNAVAILABLE,
            50008 => StatusCode::INTERNAL_SERVER_ERROR,
            // 分段回退
            raw if raw >= 10000 && raw < 20000 => StatusCode::BAD_REQUEST,
            raw if raw >= 20000 && raw < 30000 => StatusCode::FORBIDDEN,
            raw if raw >= 30000 && raw < 40000 => StatusCode::NOT_FOUND,
            raw if raw >= 40000 && raw < 50000 => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// 返回该码的默认人类可读文案。
    ///
    /// 未登记的自定义码返回空串，由调用方提供具体 `message`。
    #[must_use]
    pub const fn default_message(self) -> &'static str {
        match self.0 {
            0 => "ok",
            10001 => "参数校验失败",
            10002 => "请求体过大",
            10003 => "请求超时（入站）",
            10004 => "不支持的媒体类型",
            10005 => "请求体解析失败",
            10006 => "上游调用超时（出站）",
            10007 => "方法不允许",
            10008 => "密文无效或已被篡改",
            20001 => "未认证",
            20002 => "无权限",
            20003 => "凭证已过期",
            20004 => "权限不足",
            30001 => "资源不存在",
            30002 => "资源冲突",
            30003 => "资源已锁定",
            40001 => "请求过于频繁",
            40002 => "IP 受限",
            50001 => "服务内部错误",
            50002 => "数据访问失败",
            50003 => "上游依赖返回错误",
            50004 => "缓存访问失败",
            50005 => "服务暂不可用",
            50006 => "下游熔断中，请稍后重试",
            50007 => "并发已满，请稍后重试",
            50008 => "模板渲染失败",
            _ => "",
        }
    }
}

/// 非法错误码值。
///
/// 由 [`ErrorCode::try_from`]（`i32` → [`ErrorCode`]）在码值落在合法区间外时返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidErrorCode(i32);

impl InvalidErrorCode {
    /// 被拒绝的原始码值。
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

impl std::fmt::Display for InvalidErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "非法错误码 {}：合法区间为 10000..=59999", self.0)
    }
}

impl std::error::Error for InvalidErrorCode {}

/// 严格转换：**不提供** `From<i32>`，避免任意整数经 `?` / `.into()` 隐式变成错误码。
impl TryFrom<i32> for ErrorCode {
    type Error = InvalidErrorCode;

    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::try_new(raw).ok_or(InvalidErrorCode(raw))
    }
}

impl From<ErrorCode> for i32 {
    fn from(code: ErrorCode) -> Self {
        code.0
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_map_code_to_unique_status_when_builtin() {
        assert_eq!(ErrorCode::OK.status(), StatusCode::OK);
        assert_eq!(ErrorCode::INVALID_PARAM.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            ErrorCode::PAYLOAD_TOO_LARGE.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            ErrorCode::UNAUTHENTICATED.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ErrorCode::NOT_FOUND.status(), StatusCode::NOT_FOUND);
        // 密文无效是「输入错误」，不能表现成 500
        assert_eq!(ErrorCode::DECRYPT_FAILED.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            ErrorCode::DECRYPT_FAILED.default_message(),
            "密文无效或已被篡改"
        );
        assert_eq!(
            ErrorCode::RATE_LIMITED.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        // IP 封禁是「拒绝」不是「太快」：不能与限流共用 429
        assert_eq!(ErrorCode::IP_BLOCKED.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            ErrorCode::INTERNAL.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ErrorCode::SERVICE_UNAVAILABLE.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // 模板渲染失败是「本进程 bug / 配置问题」，不是服务不可用：
        // 归 503 会让上游/SLB 把它当成容量问题重试，掩盖真实原因。
        assert_eq!(
            ErrorCode::TEMPLATE_ERROR.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(ErrorCode::TEMPLATE_ERROR.default_message(), "模板渲染失败");
        assert_eq!(ErrorCode::TEMPLATE_ERROR.as_i32(), 50008);
    }

    #[test]
    fn should_roundtrip_raw_code_when_constructed() {
        let code = ErrorCode::new(31000);
        assert_eq!(code.as_i32(), 31000);
        assert_eq!(code.status(), StatusCode::NOT_FOUND);
        assert_eq!(code.default_message(), "");
        assert!(code.is_valid());
    }

    #[test]
    fn should_normalize_out_of_range_code_to_internal() {
        // 0（成功码）不能当错误码用：否则会以 200 返回（fail-open）
        assert_eq!(ErrorCode::new(0), ErrorCode::INTERNAL);
        assert_eq!(
            ErrorCode::new(0).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        // 区间外 / 负数一律 fail-closed
        assert_eq!(ErrorCode::new(-1), ErrorCode::INTERNAL);
        assert_eq!(ErrorCode::new(9999), ErrorCode::INTERNAL);
        assert_eq!(ErrorCode::new(60_000), ErrorCode::INTERNAL);
        assert!(ErrorCode::try_new(0).is_none(), "成功码不是合法错误码");
    }

    #[test]
    fn should_reject_invalid_code_when_try_new() {
        assert_eq!(ErrorCode::try_new(31000), Some(ErrorCode::new(31000)));
        assert_eq!(ErrorCode::try_new(0), None);
        assert_eq!(ErrorCode::try_new(-1), None);
        assert_eq!(ErrorCode::try_new(60_000), None);

        let error = ErrorCode::try_from(60_000).expect_err("区间外码值应被拒绝");
        assert_eq!(error.raw(), 60_000);
        assert!(error.to_string().contains("60000"));
    }
}
