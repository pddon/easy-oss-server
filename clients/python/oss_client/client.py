"""OSS 客户端主体：与其它语言客户端统一的 S3 兼容契约。

统一规则：方法名在 Python 侧使用 snake_case，参数顺序、参数含义与返回值
结构与其他语言完全一致（见 README 的契约表）。
"""

import base64
import hashlib
import http.client
import os
import socket
import time
from collections.abc import Mapping as MappingABC
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple
from xml.sax.saxutils import escape as xml_escape

from .config import OSSConfig
from .errors import (
    BadDigestError,
    NoSuchBucketError,
    NoSuchKeyError,
    OSSClientError,
    OSSError,
    OSSTimeoutError,
    error_from_response,
)
from .models import (
    BucketInfo,
    CopyObjectResult,
    DeleteError,
    DeleteObjectsResult,
    HeadObjectResult,
    ListObjectsV2Result,
    ListPartsResult,
    ObjectSummary,
    PartResult,
    PutObjectResult,
)
from .signer import (
    EMPTY_SHA256,
    UNSIGNED_PAYLOAD,
    SigV4Signer,
    canonical_query_string,
    uri_encode,
)
from .xmlutil import child_text, find_all, iter_all, parse_xml

RETRYABLE_STATUS = frozenset((429, 500, 502, 503, 504))
DEFAULT_PART_SIZE = 8 * 1024 * 1024
DEFAULT_CHUNK_SIZE = 1024 * 1024
MAX_DELETE_BATCH = 1000

# RFC 7230 token 字符集
TOKEN_CHARS = frozenset(
    "!#$%&'*+-.^_`|~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
)
OPTION_KEYS = frozenset(
    ("content_type", "metadata", "content_md5", "part_size", "auto_multipart")
)


def validate_metadata(metadata) -> Dict[str, str]:
    """校验自定义元数据：key 是 RFC 7230 token，value 只允许可打印 ASCII（0x20..0x7E）。

    非法时在本地抛出 ``ValueError``，不会发出请求。
    """
    if metadata is None:
        return {}
    if not isinstance(metadata, MappingABC):
        raise ValueError("metadata 必须是 dict（key/value 均为字符串）")
    result: Dict[str, str] = {}
    for key, value in metadata.items():
        if not isinstance(key, str) or not key:
            raise ValueError("metadata key 不能为空且必须是字符串")
        bad = [char for char in key if char not in TOKEN_CHARS]
        if bad:
            raise ValueError(
                "metadata key %r 含非法字符 %r（必须符合 RFC 7230 token 字符集）"
                % (key, "".join(bad))
            )
        if isinstance(value, bytes):
            value = value.decode("ascii", "strict")
        elif not isinstance(value, str):
            value = str(value)
        if any(ord(char) < 0x20 or ord(char) > 0x7E for char in value):
            raise ValueError(
                "metadata %r 的 value 只允许可打印 ASCII（0x20..0x7E）" % key
            )
        result[key.lower()] = value
    return result


def normalize_content_md5(content_md5) -> str:
    """把 ``content_md5`` 归一化为 base64 字符串，并在本地校验格式。"""
    if isinstance(content_md5, str):
        text = content_md5.strip()
        try:
            digest = base64.b64decode(text, validate=True)
        except (ValueError, TypeError) as exc:
            raise ValueError("content_md5 必须是合法的 base64 编码: %r" % content_md5) from exc
        if len(digest) != 16:
            raise ValueError("content_md5 解码后必须是 16 字节（MD5 摘要）")
        return base64.b64encode(digest).decode("ascii")
    if isinstance(content_md5, (bytes, bytearray)) and len(content_md5) == 16:
        return base64.b64encode(bytes(content_md5)).decode("ascii")
    raise ValueError("content_md5 必须是 base64 字符串或 16 字节的 MD5 摘要")


@dataclass
class _PutOptions:
    content_type: Optional[str] = None
    metadata: Dict[str, str] = field(default_factory=dict)
    content_md5: Optional[str] = None
    part_size: int = DEFAULT_PART_SIZE
    auto_multipart: bool = True


def parse_put_options(options) -> _PutOptions:
    """解析统一的 ``options`` 字典（四门语言字段名一致）。"""
    if options is None:
        return _PutOptions()
    if not isinstance(options, MappingABC):
        raise ValueError("options 必须是 dict")
    unknown = sorted(set(options) - OPTION_KEYS)
    if unknown:
        raise ValueError("options 含未知字段: %s" % ", ".join(unknown))

    part_size = options.get("part_size")
    part_size = DEFAULT_PART_SIZE if part_size is None else int(part_size)
    if part_size <= 0:
        raise ValueError("part_size 必须大于 0")

    auto_multipart = options.get("auto_multipart")
    if auto_multipart is None:
        auto_multipart = True

    return _PutOptions(
        content_type=options.get("content_type"),
        metadata=validate_metadata(options.get("metadata")),
        content_md5=(
            normalize_content_md5(options["content_md5"])
            if options.get("content_md5") is not None
            else None
        ),
        part_size=part_size,
        auto_multipart=bool(auto_multipart),
    )


class _Response:
    """非流式响应。"""

    def __init__(self, status: int, headers, body: bytes) -> None:
        self.status_code = status
        self.headers = headers
        self.body = body

    @property
    def status(self) -> int:
        return self.status_code

    def header(self, name: str, default: Optional[str] = None) -> Optional[str]:
        return self.headers.get(name, default)


class _StreamingResponse:
    """流式响应；``with`` 结束后自动释放连接。"""

    def __init__(self, client: "OSSClient", response) -> None:
        self._client = client
        self._response = response
        self._closed = False
        self._eof = False
        self.status_code = response.status
        self.headers = response.headers

    def read(self, size: int = -1) -> bytes:
        data = self._response.read(size)
        if not data:
            self._eof = True
        return data

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._eof:
            self._client._release(self._response)
        else:
            try:
                self._response.close()
            except Exception:
                pass
            self._client._close_connection()

    def __enter__(self) -> "_StreamingResponse":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.close()


class _RequestBody:
    """统一 bytes / str / 路径 / 文件对象 的请求体准备。

    - bytes/str：内容直接进内存，可重放。
    - ``os.PathLike``：SDK 打开并负责关闭，可重放。
    - 文件对象：流式读取；可 seek 时可重放，否则只尝试一次（chunked）。
    """

    def __init__(self, data) -> None:
        self.kind = "none"
        self.value = None
        self.payload_hash = EMPTY_SHA256
        self.length = None
        self.replayable = True
        self.needs_chunked = False
        self._file = None
        self._owned = False
        self._start = None
        self._offset = 0

        if data is None:
            return
        if isinstance(data, str):
            data = data.encode("utf-8")
        if isinstance(data, (bytes, bytearray, memoryview)):
            value = bytes(data)
            self.kind = "bytes"
            self.value = value
            self.payload_hash = hashlib.sha256(value).hexdigest()
            self.length = len(value)
            return
        if isinstance(data, os.PathLike):
            self._open_file(open(data, "rb"), owned=True)
            return
        if hasattr(data, "read"):
            self._open_file(data, owned=False)
            return
        raise TypeError("不支持的数据类型: %r（应为 bytes/str/Path/文件对象）" % type(data))

    def _open_file(self, file_obj, owned: bool) -> None:
        self.kind = "file"
        self._file = file_obj
        self._owned = owned
        self.payload_hash = UNSIGNED_PAYLOAD
        try:
            self._start = file_obj.tell()
        except (AttributeError, OSError, ValueError):
            self._start = None

        length = None
        try:
            if getattr(file_obj, "seekable", lambda: False)():
                if self._start is not None:
                    file_obj.seek(0, os.SEEK_END)
                    length = file_obj.tell() - self._start
                    file_obj.seek(self._start)
        except (AttributeError, OSError, ValueError):
            length = None
        if length is None:
            try:
                buffer = file_obj.getbuffer()
                length = len(buffer) - (self._start or 0)
                del buffer
            except (AttributeError, TypeError, ValueError):
                length = None

        self.length = length
        self.replayable = self._start is not None and getattr(
            file_obj, "seekable", lambda: False
        )()
        self.needs_chunked = length is None

    def body_for_attempt(self):
        if self.kind == "bytes":
            self._offset = 0
            return self.value
        if self.kind == "file" and self._start is not None:
            try:
                self._file.seek(self._start)
            except (AttributeError, OSError, ValueError):
                pass
        return self._file if self.kind == "file" else None

    def read(self, size: int) -> bytes:
        """供分片上传顺序读取（bytes 按偏移切片，文件顺序读取）。"""
        if self.kind == "none":
            return b""
        if self.kind == "bytes":
            chunk = self.value[self._offset:self._offset + size]
            self._offset += len(chunk)
            return chunk
        return self._file.read(size)

    def rewind(self) -> None:
        """把读取位置复位到起始处（仅对可重放请求体有效）。"""
        if self.kind == "bytes":
            self._offset = 0
        elif self.kind == "file" and self._start is not None:
            try:
                self._file.seek(self._start)
            except (AttributeError, OSError, ValueError):
                pass

    def close(self) -> None:
        if self._owned and self._file is not None:
            try:
                self._file.close()
            except Exception:
                pass


def _unquote_etag(value: Optional[str]) -> str:
    if not value:
        return ""
    return value.strip().strip('"')


def _md5_base64(data: bytes) -> str:
    """按 S3 的 Content-MD5 格式返回 base64 编码的 MD5。"""
    return base64.b64encode(hashlib.md5(data).digest()).decode("ascii")


def _metadata_from_headers(headers) -> Dict[str, str]:
    result = {}
    for name, value in headers.items():
        lower = name.lower()
        if lower.startswith("x-amz-meta-"):
            result[lower[len("x-amz-meta-"):]] = value
    return result


def _int_header(headers, name: str, default: int = 0) -> int:
    value = headers.get(name)
    if value is None:
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _range_header(range_value) -> dict:
    """把 ``range`` 参数（字符串或 ``(start, end)`` 元组）转成请求头。"""
    if range_value is None:
        return {}
    if isinstance(range_value, tuple):
        start, end = range_value
        return {"Range": "bytes=%s-%s" % (start, "" if end is None else end)}
    return {"Range": str(range_value)}


class OSSClient:
    """S3 兼容 OSS 客户端。

    既可以传单个配置对象，也可以直接传连接参数::

        OSSClient(OSSConfig(endpoint=..., access_key=..., secret_key=...))
        OSSClient("http://127.0.0.1:8080", "app-id", "app-secret", bucket="my-bucket")

    所有方法第一个参数都是 ``bucket``，传 ``None`` 时使用配置中的默认桶。
    支持 ``with`` 语法，退出时关闭底层连接。
    """

    def __init__(
        self,
        endpoint=None,
        access_key: Optional[str] = None,
        secret_key: Optional[str] = None,
        *,
        region: Optional[str] = None,
        bucket: Optional[str] = None,
        timeout: Optional[float] = None,
        max_retries: Optional[int] = None,
        retry_backoff: Optional[float] = None,
        config: Optional[OSSConfig] = None,
    ) -> None:
        if isinstance(endpoint, OSSConfig):
            config = endpoint
            endpoint = None
        if config is None:
            config = OSSConfig(
                endpoint=endpoint,
                access_key=access_key,
                secret_key=secret_key,
                region=region or "us-east-1",
                bucket=bucket,
                timeout=30.0 if timeout is None else timeout,
                max_retries=3 if max_retries is None else max_retries,
                retry_backoff=0.5 if retry_backoff is None else retry_backoff,
            )
        self.config = config
        self._signer = SigV4Signer(config.access_key, config.secret_key, config.region)
        self._conn = None

        from urllib.parse import urlsplit

        parts = urlsplit(config.endpoint)
        self._scheme = parts.scheme.lower()
        self._hostname = parts.hostname
        self._port = parts.port
        self._netloc = parts.netloc
        # endpoint 允许带路径前缀，例如 http://host:8080/api
        self._prefix = config.path_prefix

    # ------------------------------------------------------------------
    # 生命周期
    # ------------------------------------------------------------------
    def close(self) -> None:
        """关闭底层连接；可重复调用。"""
        self._close_connection()

    def __enter__(self) -> "OSSClient":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.close()

    def __repr__(self) -> str:
        return "OSSClient(endpoint=%r, region=%r, bucket=%r)" % (
            self.config.endpoint,
            self.config.region,
            self.config.bucket,
        )

    # ------------------------------------------------------------------
    # HTTP 基础设施
    # ------------------------------------------------------------------
    def _connection(self):
        if self._conn is None or self._conn.sock is None:
            if self._scheme == "https":
                self._conn = http.client.HTTPSConnection(
                    self._hostname, self._port, timeout=self.config.timeout
                )
            else:
                self._conn = http.client.HTTPConnection(
                    self._hostname, self._port, timeout=self.config.timeout
                )
        return self._conn

    def _close_connection(self) -> None:
        if self._conn is not None:
            try:
                self._conn.close()
            except Exception:
                pass
        self._conn = None

    def _release(self, response) -> None:
        if getattr(response, "will_close", False):
            self._close_connection()

    def _backoff(self, attempt: int) -> None:
        delay = self.config.retry_backoff * (2 ** attempt)
        if delay > 0:
            time.sleep(delay)

    def _resolve_bucket(self, bucket: Optional[str]) -> str:
        name = bucket if bucket is not None else self.config.bucket
        if not name:
            raise ValueError(
                "bucket 参数为 None 且配置中没有默认桶（OSSConfig.bucket）；"
                "请显式传入 bucket，或在创建客户端时设置默认桶"
            )
        return name

    def _path(self, bucket: Optional[str] = None, key: Optional[str] = None) -> str:
        path = self._prefix + "/"
        if bucket:
            path += uri_encode(bucket, encode_slash=True)
            if key is not None:
                path += "/" + uri_encode(key, encode_slash=False)
        return path

    def _parse_root(self, data: bytes):
        try:
            return parse_xml(data)
        except ValueError as exc:
            raise OSSClientError("服务端响应不是合法 XML: %s" % exc) from exc

    def _request(
        self,
        method: str,
        bucket: Optional[str] = None,
        key: Optional[str] = None,
        query: Optional[Sequence[Tuple[str, object]]] = None,
        headers: Optional[dict] = None,
        body=None,
        expected: Sequence[int] = (200,),
        not_found_code: str = "NoSuchKey",
        operation: Optional[str] = None,
        stream: bool = False,
    ):
        path = self._path(bucket, key)
        pairs = list(query or [])
        query_string = canonical_query_string(pairs)
        target = path + ("?" + query_string if query_string else "")

        provided_body = isinstance(body, _RequestBody)
        body_obj = body if provided_body else _RequestBody(body)
        base_headers = dict(headers or {})
        base_headers.setdefault("Host", self._netloc)
        base_headers.setdefault("User-Agent", self.config.user_agent)
        if body_obj.kind != "none" and body_obj.length is not None:
            base_headers.setdefault("Content-Length", str(body_obj.length))

        attempts = self.config.max_retries + 1 if body_obj.replayable else 1
        try:
            for attempt in range(attempts):
                try:
                    signed = self._signer.sign_request(
                        method, path, pairs, base_headers, body_obj.payload_hash
                    )
                    conn = self._connection()
                    conn.request(
                        method,
                        target,
                        body=body_obj.body_for_attempt(),
                        headers=signed,
                        encode_chunked=body_obj.needs_chunked,
                    )
                    response = conn.getresponse()
                    status = response.status
                    if status in expected and stream:
                        return _StreamingResponse(self, response)
                    data = response.read()
                except socket.timeout as exc:
                    self._close_connection()
                    if attempt + 1 < attempts:
                        self._backoff(attempt)
                        continue
                    raise OSSTimeoutError(
                        "%s %s 请求超时（%s 次尝试）" % (method, path, attempt + 1),
                        attempts=attempt + 1,
                    ) from exc
                except (OSError, http.client.HTTPException) as exc:
                    self._close_connection()
                    if attempt + 1 < attempts:
                        self._backoff(attempt)
                        continue
                    raise OSSClientError(
                        "%s %s 请求失败: %s（%s 次尝试）" % (method, path, exc, attempt + 1),
                        attempts=attempt + 1,
                    ) from exc

                if status in expected:
                    self._release(response)
                    return _Response(status, response.headers, data)

                self._release(response)
                if status in RETRYABLE_STATUS and attempt + 1 < attempts:
                    self._close_connection()
                    self._backoff(attempt)
                    continue
                raise error_from_response(
                    status,
                    response.headers,
                    data,
                    default_code=not_found_code,
                    operation=operation,
                )
            raise OSSClientError("%s %s 请求失败" % (method, path))
        finally:
            if not provided_body:
                body_obj.close()

    # ------------------------------------------------------------------
    # 桶
    # ------------------------------------------------------------------
    def list_buckets(self) -> List[BucketInfo]:
        """ListBuckets，每项含 ``name`` 与 ``created_at``。"""
        resp = self._request("GET", expected=(200,), operation="ListBuckets")
        root = self._parse_root(resp.body)
        buckets = []
        for element in iter_all(root, "Bucket"):
            buckets.append(
                BucketInfo(
                    name=child_text(element, "Name"),
                    created_at=child_text(element, "CreationDate") or None,
                )
            )
        return buckets

    def create_bucket(self, bucket: Optional[str]) -> None:
        """创建桶；重名时抛出 ``BucketAlreadyOwnedByYouError``。"""
        name = self._resolve_bucket(bucket)
        self._request(
            "PUT",
            bucket=name,
            expected=(200, 201),
            not_found_code="NoSuchBucket",
            operation="CreateBucket",
        )

    def delete_bucket(self, bucket: Optional[str]) -> None:
        """删除桶；桶内仍有 active 对象时抛出 ``BucketNotEmptyError``。"""
        name = self._resolve_bucket(bucket)
        self._request(
            "DELETE",
            bucket=name,
            expected=(204, 200),
            not_found_code="NoSuchBucket",
            operation="DeleteBucket",
        )

    def head_bucket(self, bucket: Optional[str]) -> BucketInfo:
        """HeadBucket，返回桶信息（``exists`` 与 ``region``）。

        ``region`` 取响应头 ``x-amz-bucket-region``，缺失时回退配置的 region。
        桶不存在时返回 ``exists=False``，不抛出异常。
        """
        name = self._resolve_bucket(bucket)
        try:
            resp = self._request(
                "HEAD",
                bucket=name,
                expected=(200,),
                not_found_code="NoSuchBucket",
                operation="HeadBucket",
            )
        except NoSuchBucketError:
            return BucketInfo(name=name, exists=False, region=self.config.region)
        return BucketInfo(
            name=name,
            exists=True,
            region=resp.header("x-amz-bucket-region") or self.config.region,
        )

    def bucket_exists(self, bucket: Optional[str]) -> bool:
        """桶是否存在；404 返回 ``False``，其余错误照常抛出。"""
        try:
            return bool(self.head_bucket(bucket).exists)
        except NoSuchBucketError:
            return False

    def get_bucket_location(self, bucket: Optional[str]) -> str:
        """GetBucketLocation；本服务恒返回空，SDK 归一化为 ``us-east-1``。"""
        name = self._resolve_bucket(bucket)
        resp = self._request(
            "GET",
            bucket=name,
            query=[("location", "")],
            expected=(200,),
            not_found_code="NoSuchBucket",
            operation="GetBucketLocation",
        )
        root = self._parse_root(resp.body)
        text = (root.text or "").strip() or child_text(root, "LocationConstraint").strip()
        return text or "us-east-1"

    # ------------------------------------------------------------------
    # 对象读写
    # ------------------------------------------------------------------
    def put_object(
        self,
        bucket: Optional[str],
        key: str,
        body,
        options: Optional[dict] = None,
    ) -> PutObjectResult:
        """上传对象。

        ``body`` 支持 bytes、str（按内容处理）、``os.PathLike`` 以及可读文件
        对象（流式上传，不整体读入内存）。

        ``options`` 字段（四门语言一致）：

        - ``content_type``：对象 Content-Type。
        - ``metadata``：自定义元数据（key 符合 RFC 7230 token，key/value 为 ASCII）。
        - ``content_md5``：base64 字符串或 16 字节 MD5 摘要。
        - ``part_size``：自动分片阈值，默认 8MB。
        - ``auto_multipart``：默认 ``True``，超过 ``part_size`` 自动走分片上传。
        """
        name = self._resolve_bucket(bucket)
        opts = parse_put_options(options)
        body_obj = _RequestBody(body)
        try:
            return self._upload(name, key, body_obj, opts)
        finally:
            body_obj.close()

    def upload_file(
        self,
        bucket: Optional[str],
        key: str,
        path,
        options: Optional[dict] = None,
    ) -> PutObjectResult:
        """按文件路径上传；``options`` 与 ``put_object`` 完全一致。"""
        if not isinstance(path, (str, os.PathLike)):
            raise TypeError("upload_file 的 path 必须是文件路径（str 或 os.PathLike）")
        name = self._resolve_bucket(bucket)
        opts = parse_put_options(options)
        # upload_file 的 path 语义固定为文件路径，str 必须先归一为 PathLike
        body_obj = _RequestBody(Path(path))
        try:
            return self._upload(name, key, body_obj, opts)
        finally:
            body_obj.close()

    def _upload(
        self, bucket: str, key: str, body_obj: "_RequestBody", opts: _PutOptions
    ) -> PutObjectResult:
        # 可重放的请求体允许额外做本地整对象 MD5 校验；不可重放流直接跳过。
        if opts.content_md5 is not None and body_obj.replayable:
            self._verify_whole_md5(bucket, key, body_obj, opts.content_md5)

        size = body_obj.length
        if opts.auto_multipart and size is not None and size > opts.part_size:
            return self._multipart_upload(bucket, key, body_obj, opts, size)

        headers = {}
        if opts.content_type:
            headers["Content-Type"] = opts.content_type
        for meta_key, meta_value in opts.metadata.items():
            headers["x-amz-meta-%s" % meta_key] = meta_value
        if opts.content_md5:
            headers["Content-MD5"] = opts.content_md5

        resp = self._request(
            "PUT",
            bucket=bucket,
            key=key,
            headers=headers,
            body=body_obj,
            expected=(200,),
            not_found_code="NoSuchBucket",
            operation="PutObject",
        )
        return PutObjectResult(etag=_unquote_etag(resp.header("ETag")), size=size)

    def _verify_whole_md5(
        self, bucket: str, key: str, body_obj: "_RequestBody", expected: str
    ) -> None:
        """本地校验整对象 MD5；不匹配时抛 ``BadDigestError``（不发请求）。"""
        if body_obj.kind == "bytes":
            actual = _md5_base64(body_obj.value)
        else:
            hasher = hashlib.md5()
            body_obj.rewind()
            while True:
                chunk = body_obj.read(DEFAULT_CHUNK_SIZE)
                if not chunk:
                    break
                hasher.update(chunk)
            body_obj.rewind()
            actual = base64.b64encode(hasher.digest()).decode("ascii")
        if actual != expected:
            raise BadDigestError(
                "BadDigest",
                "content_md5 与本地计算的整对象 MD5 不一致",
                None,
                None,
                resource="/%s/%s" % (bucket, key),
                operation="PutObject",
            )

    def _multipart_upload(
        self,
        bucket: str,
        key: str,
        body_obj: "_RequestBody",
        opts: _PutOptions,
        size: int,
    ) -> PutObjectResult:
        upload_id = self.create_multipart_upload(
            bucket, key, opts.content_type, opts.metadata
        )
        parts = []
        part_number = 1
        body_obj.rewind()
        try:
            while True:
                chunk = body_obj.read(opts.part_size)
                if not chunk:
                    break
                # 自动分片不发整对象 MD5，改为逐片计算并作为该分片的 Content-MD5
                etag = self._upload_part(
                    bucket,
                    key,
                    upload_id,
                    part_number,
                    chunk,
                    content_md5=_md5_base64(chunk),
                )
                parts.append({"part_number": part_number, "etag": etag})
                part_number += 1
            if not parts:
                raise OSSClientError("未读取到任何数据，无法完成分片上传")
            etag = self.complete_multipart_upload(bucket, key, upload_id, parts)
            return PutObjectResult(etag=etag, size=size)
        except BaseException:
            try:
                self.abort_multipart_upload(bucket, key, upload_id)
            except OSSError:
                pass
            raise

    def get_object(
        self, bucket: Optional[str], key: str, range=None
    ) -> bytes:
        """下载对象内容并返回 ``bytes``（流式下载请用 ``get_object_to_file``）。

        ``range`` 支持 ``"bytes=0-9"`` 字符串或 ``(start, end)`` 元组（end 可为 None）。
        """
        name = self._resolve_bucket(bucket)
        resp = self._request(
            "GET",
            bucket=name,
            key=key,
            headers=_range_header(range),
            expected=(200, 206),
            not_found_code="NoSuchKey",
            operation="GetObject",
        )
        return resp.body

    def get_object_to_file(
        self,
        bucket: Optional[str],
        key: str,
        path,
        range=None,
    ) -> None:
        """流式下载到本地文件（``path`` 为 str 或 PathLike），支持 Range。"""
        if not isinstance(path, (str, os.PathLike)):
            raise TypeError("get_object_to_file 的 path 必须是文件路径")
        name = self._resolve_bucket(bucket)
        stream = self._request(
            "GET",
            bucket=name,
            key=key,
            headers=_range_header(range),
            expected=(200, 206),
            not_found_code="NoSuchKey",
            operation="GetObject",
            stream=True,
        )
        try:
            with open(path, "wb") as target:
                while True:
                    chunk = stream.read(DEFAULT_CHUNK_SIZE)
                    if not chunk:
                        break
                    target.write(chunk)
        finally:
            stream.close()

    def head_object(self, bucket: Optional[str], key: str) -> HeadObjectResult:
        """获取对象元信息（大小、ETag、Content-Type、元数据、最后修改时间）。"""
        name = self._resolve_bucket(bucket)
        resp = self._request(
            "HEAD",
            bucket=name,
            key=key,
            expected=(200,),
            not_found_code="NoSuchKey",
            operation="HeadObject",
        )
        return HeadObjectResult(
            size=_int_header(resp.headers, "Content-Length"),
            etag=_unquote_etag(resp.header("ETag")),
            content_type=resp.header("Content-Type") or "application/octet-stream",
            last_modified=resp.header("Last-Modified"),
            metadata=_metadata_from_headers(resp.headers),
        )

    def object_exists(self, bucket: Optional[str], key: str) -> bool:
        """对象是否存在；404 返回 ``False``。"""
        try:
            self.head_object(bucket, key)
            return True
        except NoSuchKeyError:
            return False

    def delete_object(self, bucket: Optional[str], key: str) -> None:
        """删除对象（服务端为软删除进回收站；对象不存在也返回成功）。"""
        name = self._resolve_bucket(bucket)
        self._request(
            "DELETE",
            bucket=name,
            key=key,
            expected=(204, 200),
            not_found_code="NoSuchKey",
            operation="DeleteObject",
        )

    def delete_objects(
        self, bucket: Optional[str], keys: Iterable[str], quiet: bool = False
    ) -> DeleteObjectsResult:
        """批量删除。超过 1000 个 Key 时 SDK 自动分批（每批 1000 个）。"""
        name = self._resolve_bucket(bucket)
        key_list = list(keys)
        result = DeleteObjectsResult()
        for offset in range(0, len(key_list), MAX_DELETE_BATCH):
            batch = self._delete_objects_batch(
                name, key_list[offset:offset + MAX_DELETE_BATCH], quiet
            )
            result.deleted.extend(batch.deleted)
            result.errors.extend(batch.errors)
        return result

    def _delete_objects_batch(
        self, bucket: str, keys: List[str], quiet: bool
    ) -> DeleteObjectsResult:
        parts = ["<Delete>"]
        for key in keys:
            parts.append("<Object><Key>%s</Key></Object>" % xml_escape(key))
        if quiet:
            parts.append("<Quiet>true</Quiet>")
        parts.append("</Delete>")
        body = '<?xml version="1.0" encoding="UTF-8"?>' + "".join(parts)

        resp = self._request(
            "POST",
            bucket=bucket,
            query=[("delete", "")],
            headers={"Content-Type": "application/xml"},
            body=body,
            expected=(200,),
            not_found_code="NoSuchBucket",
            operation="DeleteObjects",
        )
        root = self._parse_root(resp.body)
        deleted = [child_text(node, "Key") for node in find_all(root, "Deleted")]
        errors = [
            DeleteError(
                key=child_text(node, "Key"),
                code=child_text(node, "Code"),
                message=child_text(node, "Message"),
            )
            for node in find_all(root, "Error")
        ]
        return DeleteObjectsResult(deleted=deleted, errors=errors)

    def copy_object(
        self,
        source_bucket: Optional[str],
        source_key: str,
        dest_bucket: Optional[str],
        dest_key: str,
        metadata: Optional[dict] = None,
        metadata_directive: Optional[str] = None,
    ) -> CopyObjectResult:
        """服务端复制对象。

        未显式指定 ``metadata_directive`` 时：传了 ``metadata`` 按 ``REPLACE``，
        否则按 ``COPY``。
        """
        src_bucket = self._resolve_bucket(source_bucket)
        dst_bucket = self._resolve_bucket(dest_bucket)
        meta = validate_metadata(metadata)
        directive = metadata_directive
        if directive is None:
            directive = "REPLACE" if meta else "COPY"
        directive = str(directive).upper()
        if directive not in ("COPY", "REPLACE"):
            raise ValueError("metadata_directive 只能是 COPY 或 REPLACE")

        headers = {
            "x-amz-copy-source": "/%s/%s"
            % (uri_encode(src_bucket, True), uri_encode(source_key, False)),
            "x-amz-metadata-directive": directive,
        }
        if directive == "REPLACE":
            for meta_key, meta_value in meta.items():
                headers["x-amz-meta-%s" % meta_key] = meta_value

        resp = self._request(
            "PUT",
            bucket=dst_bucket,
            key=dest_key,
            headers=headers,
            expected=(200,),
            not_found_code="NoSuchKey",
            operation="CopyObject",
        )
        root = self._parse_root(resp.body)
        return CopyObjectResult(
            etag=_unquote_etag(child_text(root, "ETag")),
            last_modified=child_text(root, "LastModified") or None,
        )

    # ------------------------------------------------------------------
    # 列表
    # ------------------------------------------------------------------
    def list_objects_v2(
        self,
        bucket: Optional[str],
        prefix: Optional[str] = None,
        delimiter: Optional[str] = None,
        max_keys: int = 1000,
        continuation_token: Optional[str] = None,
        start_after: Optional[str] = None,
    ) -> ListObjectsV2Result:
        """ListObjectsV2，返回 ``items`` / ``common_prefixes`` / ``is_truncated`` / ``next_token``。"""
        name = self._resolve_bucket(bucket)
        query: List[Tuple[str, object]] = [("list-type", "2")]
        if prefix is not None:
            query.append(("prefix", prefix))
        if delimiter is not None:
            query.append(("delimiter", delimiter))
        if max_keys is not None:
            query.append(("max-keys", str(int(max_keys))))
        if continuation_token is not None:
            query.append(("continuation-token", continuation_token))
        if start_after is not None:
            query.append(("start-after", start_after))

        resp = self._request(
            "GET",
            bucket=name,
            query=query,
            expected=(200,),
            not_found_code="NoSuchBucket",
            operation="ListObjectsV2",
        )
        root = self._parse_root(resp.body)
        items = []
        for node in find_all(root, "Contents"):
            items.append(
                ObjectSummary(
                    key=child_text(node, "Key"),
                    size=int(child_text(node, "Size", "0") or 0),
                    etag=_unquote_etag(child_text(node, "ETag")),
                    last_modified=child_text(node, "LastModified") or None,
                )
            )
        common_prefixes = [
            child_text(node, "Prefix")
            for node in find_all(root, "CommonPrefixes")
            if child_text(node, "Prefix")
        ]
        is_truncated = child_text(root, "IsTruncated").strip().lower() == "true"
        key_count = int(child_text(root, "KeyCount", "0") or 0)
        if not key_count:
            key_count = len(items) + len(common_prefixes)

        return ListObjectsV2Result(
            name=child_text(root, "Name") or name,
            prefix=child_text(root, "Prefix") or (prefix or ""),
            max_keys=int(child_text(root, "MaxKeys", str(max_keys)) or max_keys),
            key_count=key_count,
            is_truncated=is_truncated,
            items=items,
            common_prefixes=common_prefixes,
            continuation_token=child_text(root, "ContinuationToken") or None,
            next_token=child_text(root, "NextContinuationToken") or None,
            delimiter=child_text(root, "Delimiter") or None,
            start_after=child_text(root, "StartAfter") or None,
        )

    def iter_objects_v2(
        self,
        bucket: Optional[str],
        prefix: Optional[str] = None,
        delimiter: Optional[str] = None,
        max_keys: int = 1000,
        start_after: Optional[str] = None,
    ):
        """分页遍历对象，逐条 yield ``ObjectSummary``。"""
        token = None
        first = True
        while True:
            page = self.list_objects_v2(
                bucket=bucket,
                prefix=prefix,
                delimiter=delimiter,
                max_keys=max_keys,
                continuation_token=token,
                start_after=start_after if first else None,
            )
            for item in page.items:
                yield item
            first = False
            if not page.is_truncated or not page.next_token:
                return
            token = page.next_token

    # ------------------------------------------------------------------
    # 分片上传
    # ------------------------------------------------------------------
    def create_multipart_upload(
        self,
        bucket: Optional[str],
        key: str,
        content_type: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> str:
        """初始化分片上传，返回 ``upload_id``。"""
        name = self._resolve_bucket(bucket)
        headers = {}
        if content_type:
            headers["Content-Type"] = content_type
        for meta_key, meta_value in validate_metadata(metadata).items():
            headers["x-amz-meta-%s" % meta_key] = meta_value
        resp = self._request(
            "POST",
            bucket=name,
            key=key,
            query=[("uploads", "")],
            headers=headers,
            expected=(200,),
            not_found_code="NoSuchBucket",
            operation="CreateMultipartUpload",
        )
        root = self._parse_root(resp.body)
        upload_id = child_text(root, "UploadId")
        if not upload_id:
            raise OSSClientError("CreateMultipartUpload 响应缺少 UploadId")
        return upload_id

    def upload_part(
        self,
        bucket: Optional[str],
        key: str,
        upload_id: str,
        part_number: int,
        body,
    ) -> str:
        """上传一个分片，返回 ETag 字符串（已去掉引号）。"""
        return self._upload_part(bucket, key, upload_id, part_number, body)

    def _upload_part(
        self,
        bucket: Optional[str],
        key: str,
        upload_id: str,
        part_number: int,
        body,
        content_md5: Optional[str] = None,
    ) -> str:
        name = self._resolve_bucket(bucket)
        headers = {"Content-MD5": content_md5} if content_md5 else None
        resp = self._request(
            "PUT",
            bucket=name,
            key=key,
            query=[("partNumber", str(int(part_number))), ("uploadId", upload_id)],
            headers=headers,
            body=body,
            expected=(200,),
            not_found_code="NoSuchUpload",
            operation="UploadPart",
        )
        return _unquote_etag(resp.header("ETag"))

    def complete_multipart_upload(
        self,
        bucket: Optional[str],
        key: str,
        upload_id: str,
        parts: Iterable,
    ) -> str:
        """合并分片，返回最终 ETag 字符串。

        ``parts`` 元素可为 ``PartResult``、``(part_number, etag)`` 元组，
        或 ``{"part_number": n, "etag": etag}`` 字典。
        """
        name = self._resolve_bucket(bucket)
        normalized = []
        for part in parts:
            if isinstance(part, PartResult):
                normalized.append((part.part_number, part.etag))
            elif isinstance(part, MappingABC):
                normalized.append((int(part["part_number"]), str(part["etag"])))
            else:
                number, etag = part
                normalized.append((int(number), str(etag)))
        if not normalized:
            raise ValueError("parts 不能为空")
        # 统一契约：提交前一律按 part_number 升序排序，不依赖调用方顺序
        normalized.sort(key=lambda item: item[0])

        body_parts = ["<CompleteMultipartUpload>"]
        for number, etag in normalized:
            body_parts.append(
                "<Part><PartNumber>%d</PartNumber><ETag>\"%s\"</ETag></Part>"
                % (number, _unquote_etag(etag))
            )
        body_parts.append("</CompleteMultipartUpload>")
        body = '<?xml version="1.0" encoding="UTF-8"?>' + "".join(body_parts)

        resp = self._request(
            "POST",
            bucket=name,
            key=key,
            query=[("uploadId", upload_id)],
            headers={"Content-Type": "application/xml"},
            body=body,
            expected=(200,),
            not_found_code="NoSuchUpload",
            operation="CompleteMultipartUpload",
        )
        root = self._parse_root(resp.body)
        return _unquote_etag(child_text(root, "ETag"))

    def abort_multipart_upload(
        self, bucket: Optional[str], key: str, upload_id: str
    ) -> None:
        """取消分片上传并清理临时分片。"""
        name = self._resolve_bucket(bucket)
        self._request(
            "DELETE",
            bucket=name,
            key=key,
            query=[("uploadId", upload_id)],
            expected=(204, 200),
            not_found_code="NoSuchUpload",
            operation="AbortMultipartUpload",
        )

    def list_parts(
        self, bucket: Optional[str], key: str, upload_id: str
    ) -> ListPartsResult:
        """列出某个 upload 已上传的分片（本服务一次性返回全部）。"""
        name = self._resolve_bucket(bucket)
        resp = self._request(
            "GET",
            bucket=name,
            key=key,
            query=[("uploadId", upload_id)],
            expected=(200,),
            not_found_code="NoSuchUpload",
            operation="ListParts",
        )
        root = self._parse_root(resp.body)
        parts = []
        for node in find_all(root, "Part"):
            parts.append(
                PartResult(
                    part_number=int(child_text(node, "PartNumber", "0") or 0),
                    etag=_unquote_etag(child_text(node, "ETag")),
                    size=int(child_text(node, "Size", "0") or 0),
                    last_modified=child_text(node, "LastModified") or None,
                )
            )
        return ListPartsResult(
            bucket=child_text(root, "Bucket") or name,
            key=child_text(root, "Key") or key,
            upload_id=child_text(root, "UploadId") or upload_id,
            parts=parts,
            is_truncated=child_text(root, "IsTruncated").strip().lower() == "true",
        )

    # ------------------------------------------------------------------
    # 预签名 URL
    # ------------------------------------------------------------------
    def presign_url(
        self,
        method: str,
        bucket: Optional[str],
        key: Optional[str] = None,
        expires_in: int = 3600,
        query: Optional[Sequence[Tuple[str, object]]] = None,
    ) -> str:
        """生成预签名 URL，支持 GET/PUT/DELETE/HEAD 等方法。"""
        name = self._resolve_bucket(bucket)
        path = self._path(name, key)
        query_string = self._signer.presign(
            method.upper(),
            path,
            self._netloc,
            expires_in,
            query_pairs=query,
        )
        return "%s://%s%s?%s" % (self._scheme, self._netloc, path, query_string)

    def presign_get(
        self, bucket: Optional[str], key: str, expires_in: int = 900
    ) -> str:
        """生成临时下载链接（默认 900 秒有效）。"""
        return self.presign_url("GET", bucket, key, expires_in=expires_in)

    def presign_put(
        self, bucket: Optional[str], key: str, expires_in: int = 900
    ) -> str:
        """生成临时上传链接（第三方可直接 PUT 上传，默认 900 秒有效）。"""
        return self.presign_url("PUT", bucket, key, expires_in=expires_in)
