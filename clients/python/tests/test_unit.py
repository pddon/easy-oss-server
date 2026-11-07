"""无需真实服务端的单元测试。

覆盖：错误映射、重试/超时、分页遍历、预签名 URL，以及四门语言统一契约中的
自动分片、批量分批、copy directive 推导、endpoint 路径前缀、元数据校验、
Content-MD5 校验等行为。
"""

import hashlib
import base64
import socketserver
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock
from urllib.parse import parse_qs, urlsplit

from oss_client import (
    BadDigestError,
    NoSuchBucketError,
    NoSuchKeyError,
    OSSClient,
    OSSClientError,
    OSSConfig,
    OSSServiceError,
    OSSTimeoutError,
)
from oss_client.errors import error_from_response

LIST_BUCKETS_XML = """<?xml version="1.0" encoding="UTF-8"?>
<ListAllMyBucketsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Owner><ID>app</ID><DisplayName>app</DisplayName></Owner>
  <Buckets>
    <Bucket><Name>bucket-a</Name><CreationDate>2026-01-01T00:00:00.000Z</CreationDate></Bucket>
  </Buckets>
</ListAllMyBucketsResult>"""

ERROR_XML = """<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>InternalError</Code><Message>try again</Message><RequestId>unknown</RequestId></Error>"""

NO_SUCH_BUCKET_XML = """<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>NoSuchBucket</Code><Message>no such bucket</Message><Resource>/b</Resource><RequestId>unknown</RequestId></Error>"""

DELETE_XML = """<?xml version="1.0" encoding="UTF-8"?>
<DeleteResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Deleted><Key>k</Key></Deleted>
</DeleteResult>"""

COPY_XML = """<?xml version="1.0" encoding="UTF-8"?>
<CopyObjectResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag>
  <LastModified>2026-01-01T00:00:00.000Z</LastModified>
</CopyObjectResult>"""

COMPLETE_XML = """<?xml version="1.0" encoding="UTF-8"?>
<CompleteMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Bucket>b</Bucket><Key>k</Key><ETag>&quot;final-etag-2&quot;</ETag>
</CompleteMultipartUploadResult>"""


def _list_page(prefix, keys, next_token=None):
    contents = "".join(
        "<Contents><Key>%s</Key><LastModified>2026-01-01T00:00:00.000Z</LastModified>"
        "<ETag>\"etag-%s\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Contents>"
        % (key, key)
        for key in keys
    )
    truncated = "true" if next_token else "false"
    next_xml = (
        "<NextContinuationToken>%s</NextContinuationToken>" % next_token
        if next_token
        else ""
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
        "<Name>b</Name><Prefix>%s</Prefix><KeyCount>%d</KeyCount><MaxKeys>1000</MaxKeys>"
        "<IsTruncated>%s</IsTruncated>%s%s"
        "</ListBucketResult>"
        % (prefix, len(keys), truncated, next_xml, contents)
    )


class _FakeHttpServer:
    """可编程的本地 HTTP 服务，用于验证客户端行为并记录请求细节。"""

    def __init__(self, dispatch):
        calls = []
        requests = []

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def _respond(self, method):
                length = int(self.headers.get("Content-Length") or 0)
                payload = self.rfile.read(length) if length else b""
                calls.append(self.path)
                requests.append(
                    (
                        method,
                        self.path,
                        {key.lower(): value for key, value in self.headers.items()},
                        payload,
                    )
                )
                result = dispatch(self.path)
                status, body = result[0], result[1]
                extra = result[2] if len(result) > 2 else {}
                raw = body.encode("utf-8") if isinstance(body, str) else (body or b"")
                self.send_response(status)
                headers = {"Content-Type": "application/xml"}
                headers.update(extra)
                for name, value in headers.items():
                    self.send_header(name, value)
                self.send_header(
                    "Content-Length", str(0 if method == "HEAD" else len(raw))
                )
                self.end_headers()
                if method != "HEAD" and raw:
                    self.wfile.write(raw)

            def do_GET(self):
                self._respond("GET")

            def do_PUT(self):
                self._respond("PUT")

            def do_POST(self):
                self._respond("POST")

            def do_DELETE(self):
                self._respond("DELETE")

            def do_HEAD(self):
                self._respond("HEAD")

            def log_message(self, *args):
                pass

        self.calls = calls
        self.requests = requests
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.httpd.daemon_threads = True
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    @property
    def endpoint(self):
        return "http://127.0.0.1:%d" % self.port

    def request_headers(self, method, index=0):
        matches = [item for item in self.requests if item[0] == method]
        return matches[index][2]

    def request_body(self, method, index=0):
        matches = [item for item in self.requests if item[0] == method]
        return matches[index][3]

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=2)


class ErrorMappingTest(unittest.TestCase):
    def test_xml_error_parsed_into_specific_exception(self):
        err = error_from_response(
            404, {"x-amz-request-id": "header-id"}, NO_SUCH_BUCKET_XML, operation="HeadBucket"
        )
        self.assertIsInstance(err, NoSuchBucketError)
        self.assertEqual(err.code, "NoSuchBucket")
        self.assertEqual(err.status_code, 404)
        self.assertEqual(err.status, 404)
        self.assertEqual(err.request_id, "unknown")
        self.assertEqual(err.resource, "/b")
        self.assertEqual(err.operation, "HeadBucket")
        self.assertIn("NoSuchBucket", str(err))

    def test_empty_body_falls_back_to_default_code(self):
        err = error_from_response(404, {}, b"", default_code="NoSuchKey")
        self.assertIsInstance(err, NoSuchKeyError)
        self.assertEqual(err.code, "NoSuchKey")

    def test_non_retryable_and_retryable_flags(self):
        forbidden = error_from_response(403, {}, b"", default_code="AccessDenied")
        self.assertFalse(forbidden.retryable)
        internal = error_from_response(500, {}, ERROR_XML)
        self.assertTrue(internal.retryable)

    def test_exception_fields_are_unified(self):
        err = error_from_response(500, {"x-amz-request-id": "rid"}, ERROR_XML)
        self.assertEqual(err.code, "InternalError")
        self.assertEqual(err.message, "try again")
        self.assertEqual(err.status, 500)
        self.assertEqual(err.request_id, "unknown")


class RetryAndTimeoutTest(unittest.TestCase):
    def test_retries_503_then_succeeds(self):
        responses = [(503, ERROR_XML), (503, ERROR_XML), (200, LIST_BUCKETS_XML)]

        def dispatch(_path):
            return responses.pop(0)

        with _FakeHttpServer(dispatch) as server:
            client = OSSClient(
                server.endpoint,
                "ak",
                "sk",
                timeout=2,
                max_retries=2,
                retry_backoff=0.01,
            )
            buckets = client.list_buckets()
            client.close()
            self.assertEqual([b.name for b in buckets], ["bucket-a"])
            self.assertEqual(buckets[0].created_at, "2026-01-01T00:00:00.000Z")
            self.assertEqual(len(server.calls), 3)

    def test_retry_budget_exhausted_raises_service_error(self):
        responses = [(503, ERROR_XML), (503, ERROR_XML)]

        def dispatch(_path):
            return responses.pop(0)

        with _FakeHttpServer(dispatch) as server:
            client = OSSClient(
                server.endpoint, "ak", "sk", timeout=2, max_retries=1, retry_backoff=0.01
            )
            with self.assertRaises(OSSServiceError) as ctx:
                client.list_buckets()
            client.close()
            self.assertEqual(ctx.exception.code, "InternalError")
            self.assertEqual(ctx.exception.status, 503)
            self.assertEqual(len(server.calls), 2)

    def test_404_is_not_retried(self):
        responses = [(404, NO_SUCH_BUCKET_XML)] * 5

        def dispatch(_path):
            return responses.pop(0)

        with _FakeHttpServer(dispatch) as server:
            client = OSSClient(
                server.endpoint, "ak", "sk", timeout=2, max_retries=3, retry_backoff=0.01
            )
            with self.assertRaises(NoSuchBucketError):
                client.list_objects_v2("b")
            client.close()
            self.assertEqual(len(server.calls), 1)

    def test_timeout_retries_and_reports_attempts(self):
        class SlowHandler(socketserver.BaseRequestHandler):
            def handle(self):
                try:
                    self.request.recv(1024)
                except OSError:
                    pass
                time.sleep(1.0)

        server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), SlowHandler)
        server.daemon_threads = True
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            client = OSSClient(
                "http://127.0.0.1:%d" % port,
                "ak",
                "sk",
                timeout=0.2,
                max_retries=1,
                retry_backoff=0,
            )
            with self.assertRaises(OSSTimeoutError) as ctx:
                client.list_buckets()
            client.close()
            self.assertEqual(ctx.exception.attempts, 2)
            self.assertEqual(ctx.exception.code, "TimeoutError")
            self.assertIsNone(ctx.exception.status)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_non_replayable_stream_is_not_retried(self):
        stream = mock.Mock()
        stream.read = mock.Mock(return_value=b"")
        stream.tell = mock.Mock(side_effect=OSError("no tell"))
        stream.seekable = mock.Mock(return_value=False)

        with self.assertRaises(OSSClientError):
            client = OSSClient("http://127.0.0.1:1", "ak", "sk", max_retries=3, timeout=0.5)
            client.put_object("b", "k", stream)
        client.close()


class PaginationTest(unittest.TestCase):
    def test_iter_objects_v2_walks_all_pages(self):
        first = _list_page("p/", ["p/a", "p/b"], next_token="t1")
        second = _list_page("p/", ["p/c", "p/d"], next_token="t2")
        third = _list_page("p/", ["p/e"], next_token=None)

        def dispatch(path):
            token = parse_qs(urlsplit(path).query).get("continuation-token", [None])[0]
            if token is None:
                return 200, first
            if token == "t1":
                return 200, second
            if token == "t2":
                return 200, third
            return 400, ERROR_XML

        with _FakeHttpServer(dispatch) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            keys = [item.key for item in client.iter_objects_v2("b", prefix="p/")]
            client.close()
            self.assertEqual(keys, ["p/a", "p/b", "p/c", "p/d", "p/e"])
            self.assertEqual(len(server.calls), 3)

    def test_list_objects_v2_exposes_items_and_next_token(self):
        page_xml = _list_page("p/", ["p/a"], next_token="next-1")

        with _FakeHttpServer(lambda _path: (200, page_xml)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            page = client.list_objects_v2("b/with space", prefix="p/", max_keys=1)
            client.close()
            self.assertEqual([item.key for item in page.items], ["p/a"])
            self.assertTrue(page.is_truncated)
            self.assertEqual(page.next_token, "next-1")
            self.assertEqual(page.common_prefixes, [])


class ContractTest(unittest.TestCase):
    def test_endpoint_with_path_prefix(self):
        page_xml = _list_page("", [])

        with _FakeHttpServer(lambda _path: (200, page_xml)) as server:
            client = OSSClient(server.endpoint + "/api", "ak", "sk", timeout=2)
            client.list_objects_v2("b")
            client.close()
            self.assertTrue(
                server.calls[0].startswith("/api/b?list-type=2")
            )

            url = client.presign_get("b", "dir/a b.txt", expires_in=600)
            parts = urlsplit(url)
            self.assertEqual(parts.path, "/api/b/dir/a%20b.txt")
            self.assertIn("X-Amz-Signature=", parts.query)

    def test_metadata_validation_rejects_illegal_key_without_request(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            for metadata in (
                {"bad key": "v"},
                {"含中文": "v"},
                {"ok": "中文值"},
                {"": "v"},
                {"ok": "tab\there"},
                {"ok": "line\r\nbreak"},
                {"ok": "\x1f"},
                {"ok": "\x7f"},
            ):
                with self.assertRaises(ValueError):
                    client.put_object("b", "k", b"x", {"metadata": metadata})
            self.assertEqual(server.calls, [])
            client.close()

    def test_metadata_is_lowercased_in_headers(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            client.put_object("b", "k", b"x", {"metadata": {"Foo-Bar": "Baz"}})
            client.close()
            headers = server.request_headers("PUT")
            self.assertEqual(headers["x-amz-meta-foo-bar"], "Baz")

    def test_content_md5_is_validated_locally(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            for invalid in ("not base64!!", base64.b64encode(b"short").decode()):
                with self.assertRaises(ValueError):
                    client.put_object("b", "k", b"x", {"content_md5": invalid})
            self.assertEqual(server.calls, [])

            digest = hashlib.md5(b"payload").digest()
            result = client.put_object("b", "k", b"payload", {"content_md5": digest})
            self.assertEqual(result.size, 7)
            client.close()
            headers = server.request_headers("PUT")
            self.assertEqual(
                headers["content-md5"], base64.b64encode(digest).decode("ascii")
            )

    def test_content_md5_mismatch_fails_locally_for_bytes(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            wrong = base64.b64encode(hashlib.md5(b"other").digest()).decode("ascii")
            with self.assertRaises(BadDigestError) as ctx:
                client.put_object("b", "k", b"payload", {"content_md5": wrong})
            self.assertEqual(ctx.exception.code, "BadDigest")
            self.assertIsNone(ctx.exception.status)
            self.assertEqual(server.calls, [])
            client.close()

    def test_content_md5_mismatch_fails_locally_for_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "payload.bin"
            path.write_bytes(b"payload")
            with _FakeHttpServer(lambda _p: (200, b"")) as server:
                client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
                wrong = base64.b64encode(hashlib.md5(b"other").digest()).decode("ascii")
                with self.assertRaises(BadDigestError):
                    client.upload_file("b", "k", path, {"content_md5": wrong})
                self.assertEqual(server.calls, [])

                good = base64.b64encode(hashlib.md5(b"payload").digest()).decode("ascii")
                result = client.upload_file("b", "k", path, {"content_md5": good})
                self.assertEqual(result.size, 7)
                client.close()
                self.assertEqual(server.request_headers("PUT")["content-md5"], good)

    def test_delete_objects_splits_batches_of_1000(self):
        with _FakeHttpServer(lambda _path: (200, DELETE_XML)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            keys = ["k%04d" % index for index in range(2501)]
            result = client.delete_objects("b", keys, quiet=True)
            client.close()

            self.assertEqual(len(server.calls), 3)
            bodies = [item[3] for item in server.requests if item[0] == "POST"]
            self.assertEqual(bodies[0].count(b"<Key>"), 1000)
            self.assertEqual(bodies[1].count(b"<Key>"), 1000)
            self.assertEqual(bodies[2].count(b"<Key>"), 501)
            for body in bodies:
                self.assertIn(b"<Quiet>true</Quiet>", body)
            self.assertEqual(len(result.deleted), 3)

    def test_delete_objects_quiet_defaults_to_false(self):
        with _FakeHttpServer(lambda _path: (200, DELETE_XML)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            client.delete_objects("b", ["a", "b"])
            client.close()
            body = server.request_body("POST")
            self.assertNotIn(b"<Quiet>", body)

    def test_delete_objects_empty_keys_returns_empty_without_request(self):
        with _FakeHttpServer(lambda _path: (200, DELETE_XML)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            result = client.delete_objects("b", [])
            client.close()
            self.assertEqual(result.deleted, [])
            self.assertEqual(result.errors, [])
            self.assertEqual(server.calls, [])

    def test_complete_multipart_upload_sorts_parts(self):
        with _FakeHttpServer(lambda _path: (200, COMPLETE_XML)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            etag = client.complete_multipart_upload(
                "b",
                "k",
                "uid",
                [
                    {"part_number": 3, "etag": "etag3"},
                    {"part_number": 1, "etag": "etag1"},
                    {"part_number": 2, "etag": "etag2"},
                ],
            )
            client.close()
            self.assertEqual(etag, "final-etag-2")
            body = server.request_body("POST").decode("utf-8")
            first = body.index("<PartNumber>1</PartNumber>")
            second = body.index("<PartNumber>2</PartNumber>")
            third = body.index("<PartNumber>3</PartNumber>")
            self.assertLess(first, second)
            self.assertLess(second, third)

    def test_presign_default_expires_is_900(self):
        client = OSSClient("http://127.0.0.1:9999", "ak", "sk")
        for url in (client.presign_get("b", "k"), client.presign_put("b", "k")):
            query = parse_qs(urlsplit(url).query)
            self.assertEqual(query["X-Amz-Expires"], ["900"])
        client.close()

    def test_none_bucket_uses_config_default(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", bucket="demo", timeout=2)
            client.put_object(None, "k", b"x")
            client.close()
            self.assertEqual(server.calls[0], "/demo/k")

    def test_missing_default_bucket_raises_with_reason(self):
        client = OSSClient("http://127.0.0.1:9999", "ak", "sk")
        with self.assertRaises(ValueError) as ctx:
            client.put_object(None, "k", b"x")
        self.assertIn("默认桶", str(ctx.exception))
        client.close()

    def test_unknown_length_stream_never_multiparts(self):
        class _StubResponse:
            body = b""

            def header(self, name, default=None):
                return None

        stream = mock.Mock()
        stream.read = mock.Mock(return_value=b"")
        stream.tell = mock.Mock(side_effect=OSError("不支持 tell"))
        stream.seekable = mock.Mock(return_value=False)

        client = OSSClient("http://127.0.0.1:9999", "ak", "sk")
        with mock.patch.object(
            client,
            "create_multipart_upload",
            side_effect=AssertionError("未知长度的流不应走分片"),
        ), mock.patch.object(client, "_request", return_value=_StubResponse()):
            result = client.put_object("b", "k", stream, {"part_size": 10})
        client.close()
        self.assertIsNone(result.size)

    def test_copy_object_directive_defaults(self):
        with _FakeHttpServer(lambda _path: (200, COPY_XML)) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            copied = client.copy_object("src", "a.txt", "dst", "b.txt")
            self.assertEqual(
                copied.etag, hashlib.md5(b"").hexdigest()
            )
            first = server.request_headers("PUT", 0)
            self.assertEqual(first["x-amz-metadata-directive"], "COPY")
            self.assertNotIn("x-amz-meta-stage", first)

            client.copy_object(
                "src", "a.txt", "dst", "c.txt", metadata={"stage": "new"}
            )
            second = server.request_headers("PUT", 1)
            self.assertEqual(second["x-amz-metadata-directive"], "REPLACE")
            self.assertEqual(second["x-amz-meta-stage"], "new")
            self.assertEqual(
                second["x-amz-copy-source"], "/src/a.txt"
            )

            client.copy_object(
                "src",
                "a.txt",
                "dst",
                "d.txt",
                metadata={"stage": "x"},
                metadata_directive="COPY",
            )
            third = server.request_headers("PUT", 2)
            self.assertEqual(third["x-amz-metadata-directive"], "COPY")

            # 空 metadata dict 一律按 COPY（不因“传了但为空”判成 REPLACE）
            client.copy_object("src", "a.txt", "dst", "e.txt", metadata={})
            fourth = server.request_headers("PUT", 3)
            self.assertEqual(fourth["x-amz-metadata-directive"], "COPY")
            self.assertNotIn("x-amz-meta-stage", fourth)
            client.close()

    def test_put_object_auto_multipart_switches_to_multipart(self):
        client = OSSClient("http://127.0.0.1:9999", "ak", "sk", timeout=2)
        uploads = []

        def fake_part(bucket, key, upload_id, part_number, body, content_md5=None):
            uploads.append(
                (
                    part_number,
                    len(body),
                    content_md5
                    == base64.b64encode(hashlib.md5(body).digest()).decode("ascii"),
                )
            )
            return "etag-%d" % part_number

        with mock.patch.object(
            client, "create_multipart_upload", return_value="upload-1"
        ) as create, mock.patch.object(
            client, "_upload_part", side_effect=fake_part
        ) as upload_part, mock.patch.object(
            client, "complete_multipart_upload", return_value="final-etag"
        ) as complete:
            result = client.put_object(
                "b",
                "big.bin",
                b"x" * 100,
                {"part_size": 40, "content_type": "application/octet-stream"},
            )
        client.close()

        create.assert_called_once()
        self.assertEqual(uploads, [(1, 40, True), (2, 40, True), (3, 20, True)])
        self.assertEqual(upload_part.call_count, 3)
        complete.assert_called_once()
        self.assertEqual(result.etag, "final-etag")
        self.assertEqual(result.size, 100)

    def test_put_object_keeps_single_put_when_at_or_below_threshold(self):
        with _FakeHttpServer(lambda _path: (200, b"")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)

            def fail(*_args, **_kwargs):
                raise AssertionError("不应触发分片上传")

            with mock.patch.object(client, "create_multipart_upload", new=fail):
                small = client.put_object(
                    "b", "small.bin", b"x" * 40, {"part_size": 40}
                )
                disabled = client.put_object(
                    "b",
                    "big.bin",
                    b"x" * 80,
                    {"part_size": 40, "auto_multipart": False},
                )
            client.close()
            self.assertEqual(small.size, 40)
            self.assertEqual(disabled.size, 80)
            self.assertEqual(len(server.calls), 2)

    def test_head_bucket_returns_exists_and_region(self):
        responses = [(200, b"", {"x-amz-bucket-region": "cn-north-1"}), (404, b"")]

        def dispatch(_path):
            return responses.pop(0) if responses else (404, b"")

        with _FakeHttpServer(dispatch) as server:
            client = OSSClient(server.endpoint, "ak", "sk", region="fallback", timeout=2)
            info = client.head_bucket("exists-bucket")
            self.assertTrue(info.exists)
            self.assertEqual(info.region, "cn-north-1")

            missing = client.head_bucket("missing-bucket")
            self.assertFalse(missing.exists)
            self.assertEqual(missing.region, "fallback")
            self.assertFalse(client.bucket_exists("missing-bucket"))
            client.close()

    def test_get_object_returns_bytes_and_supports_range(self):
        with _FakeHttpServer(lambda _path: (206, b"0123456789")) as server:
            client = OSSClient(server.endpoint, "ak", "sk", timeout=2)
            body = client.get_object("b", "k", (0, 9))
            client.close()
            self.assertEqual(body, b"0123456789")
            self.assertEqual(server.request_headers("GET")["range"], "bytes=0-9")


class ClientBasicsTest(unittest.TestCase):
    def test_config_validation(self):
        with self.assertRaises(ValueError):
            OSSConfig(endpoint="ftp://host", access_key="ak", secret_key="sk")
        with self.assertRaises(ValueError):
            OSSConfig(endpoint="http://host?a=1", access_key="ak", secret_key="sk")
        with self.assertRaises(ValueError):
            OSSConfig(endpoint="http://host", access_key="", secret_key="sk")
        with self.assertRaises(ValueError):
            OSSConfig(endpoint="http://host", access_key="ak", secret_key="sk", timeout=0)

        config = OSSConfig(
            endpoint="http://host:8080/api/", access_key="ak", secret_key="sk"
        )
        self.assertEqual(config.endpoint, "http://host:8080/api")
        self.assertEqual(config.path_prefix, "/api")

    def test_presigned_urls_for_get_and_put(self):
        client = OSSClient(
            "http://127.0.0.1:9999", "ak", "sk", region="us-east-1", bucket="b"
        )
        get_url = client.presign_get("b", "dir/a b.txt", expires_in=600)
        put_url = client.presign_put("b", "dir/a b.txt", expires_in=600)
        for url in (get_url, put_url):
            parts = urlsplit(url)
            self.assertEqual(parts.scheme, "http")
            self.assertEqual(parts.netloc, "127.0.0.1:9999")
            self.assertEqual(parts.path, "/b/dir/a%20b.txt")
            query = parse_qs(parts.query)
            self.assertEqual(query["X-Amz-Algorithm"], ["AWS4-HMAC-SHA256"])
            self.assertEqual(query["X-Amz-Expires"], ["600"])
            self.assertIn("X-Amz-Signature", query)
        client.close()

    def test_presign_requires_bucket(self):
        client = OSSClient("http://127.0.0.1:9999", "ak", "sk")
        with self.assertRaises(ValueError):
            client.presign_get(None, "k")
        client.close()

    def test_context_manager_and_close_are_idempotent(self):
        with OSSClient("http://127.0.0.1:9999", "ak", "sk") as client:
            client.close()
            client.close()
        self.assertIsNone(client._conn)


if __name__ == "__main__":
    unittest.main()
