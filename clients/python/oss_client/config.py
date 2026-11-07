"""客户端配置。"""

from dataclasses import dataclass, field
from typing import Optional
from urllib.parse import urlsplit


@dataclass
class OSSConfig:
    """OSS 客户端配置。

    Args:
        endpoint: 服务地址，例如 ``http://127.0.0.1:8080``。允许携带路径前缀，
            例如 ``http://127.0.0.1:8080/api``，SDK 会在该前缀后拼接
            ``/{bucket}/{key}``，签名时也使用完整路径。
        access_key: 应用的 App ID。
        secret_key: 应用的 App Secret。
        region: 参与 SigV4 签名的作用域，服务端不校验，默认 ``us-east-1``。
        bucket: 默认桶名；各 API 的 ``bucket`` 参数可覆盖。
        timeout: 单次网络操作超时（秒）。
        max_retries: 可重试错误的最大重试次数（不含首次请求）。
        retry_backoff: 重试退避基数（秒），按指数增长。
        user_agent: HTTP User-Agent。
    """

    endpoint: str
    access_key: str
    secret_key: str
    region: str = "us-east-1"
    bucket: Optional[str] = None
    timeout: float = 30.0
    max_retries: int = 3
    retry_backoff: float = 0.5
    user_agent: str = "oss-client-python/0.1.0"
    path_prefix: str = field(init=False, default="")

    def __post_init__(self) -> None:
        if not isinstance(self.endpoint, str) or not self.endpoint.strip():
            raise ValueError("endpoint 不能为空")
        self.endpoint = self.endpoint.strip().rstrip("/")
        parts = urlsplit(self.endpoint)
        if parts.scheme not in ("http", "https") or not parts.netloc:
            raise ValueError("endpoint 必须是 http:// 或 https:// 地址: %r" % self.endpoint)
        if parts.query or parts.fragment:
            raise ValueError("endpoint 不允许包含 query/fragment: %r" % self.endpoint)
        # 路径前缀（不含首尾斜杠之外的多余部分），例如 "/api"
        self.path_prefix = parts.path if parts.path != "/" else ""
        if not self.access_key or not self.secret_key:
            raise ValueError("access_key 与 secret_key 不能为空")
        if not self.region or not str(self.region).strip():
            self.region = "us-east-1"
        if self.bucket == "":
            self.bucket = None
        if self.timeout is None or float(self.timeout) <= 0:
            raise ValueError("timeout 必须大于 0")
        self.timeout = float(self.timeout)
        if int(self.max_retries) < 0:
            raise ValueError("max_retries 不能为负数")
        self.max_retries = int(self.max_retries)
        if float(self.retry_backoff) < 0:
            raise ValueError("retry_backoff 不能为负数")
        self.retry_backoff = float(self.retry_backoff)
