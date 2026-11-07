"""异常层级与 S3 XML 错误解析。"""

import xml.etree.ElementTree as ET
from typing import Optional

from .xmlutil import child_text, local_name


class OSSError(Exception):
    """SDK 所有异常的基类。"""


class OSSClientError(OSSError):
    """网络层错误（连接失败、响应读取失败等）。

    与其它语言客户端一致，异常对象统一暴露 ``code``、``message``、
    ``status``、``request_id`` 四个字段；网络错误没有 HTTP 状态码，
    ``status`` 与 ``request_id`` 为 ``None``。
    """

    def __init__(
        self, message: str, attempts: int = 1, code: str = "NetworkError"
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.status = None
        self.request_id = None
        self.attempts = attempts


class OSSTimeoutError(OSSClientError):
    """请求超时。已按配置完成重试；``attempts`` 为实际尝试次数。"""

    def __init__(self, message: str, attempts: int = 1) -> None:
        super().__init__(message, attempts=attempts, code="TimeoutError")


class OSSServiceError(OSSError):
    """服务端返回的 S3 错误。

    Attributes:
        code: S3 错误码，例如 ``NoSuchKey``。
        message: 错误描述。
        status_code: HTTP 状态码。
        request_id: 服务端 RequestId（本服务固定为 ``unknown``，也可能缺省）。
        resource: 错误 XML 中的 Resource。
        operation: 触发错误的 SDK 操作，例如 ``GetObject``。
    """

    def __init__(
        self,
        code: str,
        message: str,
        status_code: int,
        request_id: Optional[str] = None,
        resource: Optional[str] = None,
        operation: Optional[str] = None,
    ) -> None:
        self.code = code
        self.message = message
        self.status_code = status_code
        self.request_id = request_id
        self.resource = resource
        self.operation = operation
        super().__init__("%s (HTTP %s): %s" % (code, status_code, message))

    @property
    def status(self) -> int:
        """``status_code`` 的别名。"""
        return self.status_code

    @property
    def retryable(self) -> bool:
        """服务端错误是否属于 SDK 会自动重试的范围。"""
        return self.status_code in (429, 500, 502, 503, 504)


# 常见错误码对应的专用异常。未列出的错误码统一抛出 OSSServiceError。
class NoSuchBucketError(OSSServiceError):
    pass


class NoSuchKeyError(OSSServiceError):
    pass


class NoSuchUploadError(OSSServiceError):
    pass


class BucketNotEmptyError(OSSServiceError):
    pass


class BucketAlreadyExistsError(OSSServiceError):
    pass


class BucketAlreadyOwnedByYouError(OSSServiceError):
    pass


class AccessDeniedError(OSSServiceError):
    pass


class InvalidAccessKeyIdError(OSSServiceError):
    pass


class SignatureDoesNotMatchError(OSSServiceError):
    pass


class RequestTimeTooSkewedError(OSSServiceError):
    pass


class PreconditionFailedError(OSSServiceError):
    pass


class InvalidRangeError(OSSServiceError):
    pass


class EntityTooLargeError(OSSServiceError):
    pass


class InvalidArgumentError(OSSServiceError):
    pass


class InvalidBucketNameError(OSSServiceError):
    pass


class MalformedXMLError(OSSServiceError):
    pass


class BadDigestError(OSSServiceError):
    pass


class InvalidPartError(OSSServiceError):
    pass


class InvalidPartOrderError(OSSServiceError):
    pass


class OSSNotImplementedError(OSSServiceError):
    pass


class InternalError(OSSServiceError):
    pass


_ERROR_CLASSES = {
    "NoSuchBucket": NoSuchBucketError,
    "NoSuchKey": NoSuchKeyError,
    "NoSuchUpload": NoSuchUploadError,
    "BucketNotEmpty": BucketNotEmptyError,
    "BucketAlreadyExists": BucketAlreadyExistsError,
    "BucketAlreadyOwnedByYou": BucketAlreadyOwnedByYouError,
    "AccessDenied": AccessDeniedError,
    "InvalidAccessKeyId": InvalidAccessKeyIdError,
    "SignatureDoesNotMatch": SignatureDoesNotMatchError,
    "RequestTimeTooSkewed": RequestTimeTooSkewedError,
    "PreconditionFailed": PreconditionFailedError,
    "InvalidRange": InvalidRangeError,
    "EntityTooLarge": EntityTooLargeError,
    "InvalidArgument": InvalidArgumentError,
    "InvalidBucketName": InvalidBucketNameError,
    "MalformedXML": MalformedXMLError,
    "BadDigest": BadDigestError,
    "InvalidPart": InvalidPartError,
    "InvalidPartOrder": InvalidPartOrderError,
    "NotImplemented": OSSNotImplementedError,
    "InternalError": InternalError,
}

_STATUS_DEFAULT_CODE = {
    400: "InvalidArgument",
    403: "AccessDenied",
    404: "NoSuchKey",
    409: "BucketAlreadyExists",
    412: "PreconditionFailed",
    413: "EntityTooLarge",
    416: "InvalidRange",
    501: "NotImplemented",
}


def error_from_response(
    status_code: int,
    headers=None,
    body=None,
    default_code: Optional[str] = None,
    operation: Optional[str] = None,
) -> OSSServiceError:
    """从 HTTP 响应构造具体异常。

    对于 HEAD 请求（响应没有 body），按 ``default_code`` 或状态码给出兜底错误码。
    """
    code = None
    message = None
    request_id = None
    resource = None

    if body:
        text = body.encode("utf-8") if isinstance(body, str) else body
        try:
            root = ET.fromstring(text)
            if local_name(root.tag) == "Error":
                code = child_text(root, "Code") or None
                message = child_text(root, "Message") or None
                request_id = child_text(root, "RequestId") or None
                resource = child_text(root, "Resource") or None
        except ET.ParseError:
            pass

    if headers is not None:
        try:
            request_id = request_id or headers.get("x-amz-request-id")
        except (AttributeError, TypeError):
            pass

    if not code:
        code = default_code or _STATUS_DEFAULT_CODE.get(status_code, "HttpError")
    if not message:
        message = "HTTP %s" % status_code

    cls = _ERROR_CLASSES.get(code, OSSServiceError)
    return cls(
        code=code,
        message=message,
        status_code=status_code,
        request_id=request_id,
        resource=resource,
        operation=operation,
    )
