"""SDK 返回的数据模型（与其它语言客户端保持同一结构）。"""

from dataclasses import dataclass, field
from typing import Dict, List, Optional


@dataclass
class BucketInfo:
    """桶信息。

    - ``list_buckets`` 返回的每项含 ``name``、``created_at``。
    - ``head_bucket`` 返回的桶信息含 ``exists``、``region``。
    """

    name: str
    created_at: Optional[str] = None
    exists: Optional[bool] = None
    region: Optional[str] = None


@dataclass
class ObjectSummary:
    key: str
    size: int
    etag: str
    last_modified: Optional[str] = None


@dataclass
class ListObjectsV2Result:
    name: str = ""
    prefix: str = ""
    max_keys: int = 0
    key_count: int = 0
    is_truncated: bool = False
    items: List[ObjectSummary] = field(default_factory=list)
    common_prefixes: List[str] = field(default_factory=list)
    continuation_token: Optional[str] = None
    next_token: Optional[str] = None
    delimiter: Optional[str] = None
    start_after: Optional[str] = None


@dataclass
class PutObjectResult:
    etag: str
    size: Optional[int] = None


@dataclass
class CopyObjectResult:
    etag: str
    last_modified: Optional[str] = None


@dataclass
class HeadObjectResult:
    size: int
    etag: str
    content_type: str
    last_modified: Optional[str]
    metadata: Dict[str, str]


@dataclass
class PartResult:
    part_number: int
    etag: str
    size: Optional[int] = None
    last_modified: Optional[str] = None


@dataclass
class ListPartsResult:
    bucket: str
    key: str
    upload_id: str
    parts: List[PartResult] = field(default_factory=list)
    is_truncated: bool = False


@dataclass
class DeleteError:
    key: str
    code: str
    message: str


@dataclass
class DeleteObjectsResult:
    deleted: List[str] = field(default_factory=list)
    errors: List[DeleteError] = field(default_factory=list)
