"""AWS Signature Version 4（S3 协议）签名实现，仅使用标准库。

参考：AWS S3 API ``Authenticating Requests (AWS Signature Version 4)``。
"""

import hashlib
import hmac
from datetime import datetime, timezone
from typing import Iterable, List, Mapping, Optional, Sequence, Tuple

ALGORITHM = "AWS4-HMAC-SHA256"
SERVICE = "s3"
TERMINATOR = "aws4_request"
UNSIGNED_PAYLOAD = "UNSIGNED-PAYLOAD"
MAX_PRESIGN_EXPIRES = 604800

_UNRESERVED = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~"
)

QueryPairs = Iterable[Tuple[str, object]]


def uri_encode(value, encode_slash: bool = True) -> str:
    """AWS 要求的 URI 编码：除未保留字符外全部 ``%XX``（大写十六进制）。"""
    if not isinstance(value, str):
        value = str(value)
    out = []
    for byte in value.encode("utf-8"):
        char = chr(byte)
        if char in _UNRESERVED:
            out.append(char)
        elif char == "/" and not encode_slash:
            out.append(char)
        else:
            out.append("%%%02X" % byte)
    return "".join(out)


def canonical_query_string(pairs: QueryPairs) -> str:
    """构造规范 query：编码后按键、再按值排序，排除 ``X-Amz-Signature``。"""
    encoded = []
    for key, value in pairs:
        key = str(key)
        if key == "X-Amz-Signature":
            continue
        value = "" if value is None else str(value)
        encoded.append((uri_encode(key, True), uri_encode(value, True)))
    encoded.sort()
    return "&".join("%s=%s" % (key, value) for key, value in encoded)


def _normalized_header_values(headers: Mapping[str, str], name: str) -> List[str]:
    values = []
    for key, value in headers.items():
        if key.lower() == name:
            values.append(" ".join(str(value).split()))
    return values


def canonical_headers(headers: Mapping[str, str], signed_headers: Sequence[str]) -> str:
    """构造 canonical headers 块（行尾带换行）。"""
    lines = []
    for name in sorted({h.lower() for h in signed_headers}):
        values = _normalized_header_values(headers, name)
        if not values:
            raise ValueError("签名头缺失: %s" % name)
        lines.append("%s:%s\n" % (name, ",".join(values)))
    return "".join(lines)


def canonical_request(
    method: str,
    path: str,
    query_string: str,
    headers: Mapping[str, str],
    signed_headers: Sequence[str],
    payload_hash: str,
) -> str:
    """构造 CanonicalRequest。``path`` 必须是已经编码好的原始请求路径。"""
    uri = path or "/"
    names = sorted({h.lower() for h in signed_headers})
    return "%s\n%s\n%s\n%s\n%s\n%s" % (
        method.upper(),
        uri,
        query_string,
        canonical_headers(headers, names),
        ";".join(names),
        payload_hash,
    )


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


EMPTY_SHA256 = sha256_hex(b"")


def format_amz_date(moment: Optional[datetime] = None) -> str:
    moment = moment or datetime.now(timezone.utc)
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=timezone.utc)
    else:
        moment = moment.astimezone(timezone.utc)
    return moment.strftime("%Y%m%dT%H%M%SZ")


def signing_key(secret_key: str, date_stamp: str, region: str, service: str = SERVICE) -> bytes:
    """派生 SigV4 签名密钥。"""
    key = ("AWS4" + secret_key).encode("utf-8")
    for part in (date_stamp, region, service, TERMINATOR):
        key = hmac.new(key, part.encode("utf-8"), hashlib.sha256).digest()
    return key


class SigV4Signer:
    """SigV4 header 签名与预签名 URL 生成。"""

    def __init__(self, access_key: str, secret_key: str, region: str) -> None:
        self.access_key = access_key
        self.secret_key = secret_key
        self.region = region

    def credential_scope(self, date_stamp: str) -> str:
        return "%s/%s/%s/%s" % (date_stamp, self.region, SERVICE, TERMINATOR)

    def credential(self, date_stamp: str) -> str:
        return "%s/%s" % (self.access_key, self.credential_scope(date_stamp))

    def string_to_sign(self, amz_date: str, date_stamp: str, canonical_req: str) -> str:
        return "%s\n%s\n%s\n%s" % (
            ALGORITHM,
            amz_date,
            self.credential_scope(date_stamp),
            sha256_hex(canonical_req.encode("utf-8")),
        )

    def compute_signature(self, date_stamp: str, string_to_sign: str) -> str:
        key = signing_key(self.secret_key, date_stamp, self.region, SERVICE)
        return hmac.new(key, string_to_sign.encode("utf-8"), hashlib.sha256).hexdigest()

    def sign_request(
        self,
        method: str,
        path: str,
        query_pairs: QueryPairs,
        headers: Mapping[str, str],
        payload_hash: str,
        now: Optional[datetime] = None,
        signed_headers: Optional[Sequence[str]] = None,
    ) -> dict:
        """返回带 ``Authorization``、``x-amz-date``、``x-amz-content-sha256`` 的头字典。

        传入的头必须包含 ``Host``。默认只签 host、内容哈希与日期三个头，
        这样流式上传时不会因为传输层追加的头导致签名不一致。
        """
        amz_date = format_amz_date(now)
        date_stamp = amz_date[:8]
        signed = sorted(
            {h.lower() for h in (signed_headers or ("host", "x-amz-content-sha256", "x-amz-date"))}
        )
        headers_with_amz = dict(headers)
        headers_with_amz["x-amz-date"] = amz_date
        headers_with_amz["x-amz-content-sha256"] = payload_hash
        canonical = canonical_request(
            method,
            path,
            canonical_query_string(query_pairs),
            headers_with_amz,
            signed,
            payload_hash,
        )
        signature = self.compute_signature(
            date_stamp, self.string_to_sign(amz_date, date_stamp, canonical)
        )
        headers_with_amz["Authorization"] = "%s Credential=%s,SignedHeaders=%s,Signature=%s" % (
            ALGORITHM,
            self.credential(date_stamp),
            ";".join(signed),
            signature,
        )
        return headers_with_amz

    def presign(
        self,
        method: str,
        path: str,
        host: str,
        expires_in: int,
        query_pairs: Optional[QueryPairs] = None,
        now: Optional[datetime] = None,
    ) -> str:
        """生成预签名 URL 的 query 串（含签名）。payload 固定为 ``UNSIGNED-PAYLOAD``。"""
        expires_in = int(expires_in)
        if not 1 <= expires_in <= MAX_PRESIGN_EXPIRES:
            raise ValueError("expires_in 必须在 1..%d 秒之间" % MAX_PRESIGN_EXPIRES)
        amz_date = format_amz_date(now)
        date_stamp = amz_date[:8]
        pairs = list(query_pairs or [])
        pairs.extend(
            [
                ("X-Amz-Algorithm", ALGORITHM),
                ("X-Amz-Credential", self.credential(date_stamp)),
                ("X-Amz-Date", amz_date),
                ("X-Amz-Expires", str(expires_in)),
                ("X-Amz-SignedHeaders", "host"),
            ]
        )
        query_string = canonical_query_string(pairs)
        canonical = canonical_request(
            method,
            path,
            query_string,
            {"host": host},
            ["host"],
            UNSIGNED_PAYLOAD,
        )
        signature = self.compute_signature(
            date_stamp, self.string_to_sign(amz_date, date_stamp, canonical)
        )
        return "%s&X-Amz-Signature=%s" % (query_string, signature)
