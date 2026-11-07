"""真实服务端集成测试：启动 oss-server 实例，覆盖统一契约的主要链路。

默认监听 127.0.0.1:18103，数据目录 %TEMP%\\oss-server-python-data。
设置 OSS_SKIP_INTEGRATION=1 可跳过。
"""

import base64
import hashlib
import os
import shutil
import tempfile
import time
import unittest
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from unittest import mock

try:
    from ._server import ENDPOINT, TestServer
except ImportError:  # discover -s tests 时按顶层模块导入
    from _server import ENDPOINT, TestServer

from oss_client import (
    BadDigestError,
    BucketAlreadyOwnedByYouError,
    BucketNotEmptyError,
    InvalidAccessKeyIdError,
    NoSuchBucketError,
    NoSuchKeyError,
    NoSuchUploadError,
    OSSClient,
    OSSClientError,
    OSSConfig,
    SignatureDoesNotMatchError,
)

SERVER = TestServer()
MAIN_BUCKET = "py-sdk-main-%s" % uuid.uuid4().hex[:10]
CLIENT = None
TMP_DIR = None


def setUpModule():
    if os.environ.get("OSS_SKIP_INTEGRATION"):
        raise unittest.SkipTest("OSS_SKIP_INTEGRATION 已设置")
    global CLIENT, TMP_DIR
    credentials = SERVER.start()
    TMP_DIR = Path(tempfile.mkdtemp(prefix="oss-client-python-it-"))
    CLIENT = OSSClient(
        OSSConfig(
            endpoint=ENDPOINT,
            access_key=credentials["app_id"],
            secret_key=credentials["app_secret"],
            bucket=MAIN_BUCKET,
            timeout=30.0,
            max_retries=2,
            retry_backoff=0.1,
        )
    )
    CLIENT.create_bucket(MAIN_BUCKET)


def tearDownModule():
    global CLIENT, TMP_DIR
    if CLIENT is not None:
        CLIENT.close()
        CLIENT = None
    SERVER.stop()
    if TMP_DIR is not None:
        shutil.rmtree(str(TMP_DIR), ignore_errors=True)
        TMP_DIR = None


def _fetch(url: str, method: str = "GET", data=None):
    request = urllib.request.Request(url, data=data, method=method)
    with urllib.request.urlopen(request, timeout=15) as response:
        return response.status, response.read()


class IntegrationTest(unittest.TestCase):
    # ------------------------------------------------------------------
    def test_01_bucket_lifecycle(self):
        name = "py-sdk-bkt-%s" % uuid.uuid4().hex[:10]
        CLIENT.create_bucket(name)
        self.assertTrue(CLIENT.bucket_exists(name))
        head = CLIENT.head_bucket(name)
        self.assertTrue(head.exists)
        self.assertEqual(head.region, "us-east-1")
        self.assertEqual(CLIENT.get_bucket_location(name), "us-east-1")
        listed = CLIENT.list_buckets()
        self.assertIn(name, [bucket.name for bucket in listed])
        self.assertTrue(all(bucket.created_at for bucket in listed))

        with self.assertRaises(BucketAlreadyOwnedByYouError) as ctx:
            CLIENT.create_bucket(name)
        self.assertEqual(ctx.exception.status, 409)
        self.assertEqual(ctx.exception.code, "BucketAlreadyOwnedByYou")

        CLIENT.put_object(name, "non-empty.txt", b"x")
        with self.assertRaises(BucketNotEmptyError):
            CLIENT.delete_bucket(name)

        CLIENT.delete_object(name, "non-empty.txt")
        CLIENT.delete_bucket(name)
        self.assertFalse(CLIENT.bucket_exists(name))
        self.assertFalse(CLIENT.head_bucket(name).exists)
        # 服务端已修复：删桶后 GetBucketLocation 与 HeadBucket 一致返回 404
        with self.assertRaises(NoSuchBucketError) as ctx:
            CLIENT.get_bucket_location(name)
        self.assertEqual(ctx.exception.status, 404)
        self.assertEqual(ctx.exception.code, "NoSuchBucket")

    # ------------------------------------------------------------------
    def test_02_object_roundtrip(self):
        token = uuid.uuid4().hex[:8]
        key = "it/%s/目录/hello world 中文.txt" % token
        content = ("hello oss 中文 " * 20).encode("utf-8")

        put = CLIENT.put_object(
            MAIN_BUCKET,
            key,
            content,
            {
                "content_type": "text/plain; charset=utf-8",
                "metadata": {"Foo": "Bar", "num": "42"},
            },
        )
        self.assertEqual(put.etag, hashlib.md5(content).hexdigest())
        self.assertEqual(put.size, len(content))

        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key), content)

        head = CLIENT.head_object(MAIN_BUCKET, key)
        self.assertEqual(head.size, len(content))
        self.assertEqual(head.etag, put.etag)
        self.assertEqual(head.content_type, "text/plain; charset=utf-8")
        self.assertTrue(head.last_modified)
        self.assertEqual(head.metadata["foo"], "Bar")
        self.assertEqual(head.metadata["num"], "42")

        self.assertEqual(
            CLIENT.get_object(MAIN_BUCKET, key, "bytes=0-9"), content[:10]
        )
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key, (0, 9)), content[:10])
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key, "bytes=-5"), content[-5:])

        big = os.urandom(1024 * 1024)
        big_key = "it/%s/big.bin" % token
        source_path = TMP_DIR / ("source-%s.bin" % token)
        source_path.write_bytes(big)
        uploaded = CLIENT.upload_file(
            MAIN_BUCKET,
            big_key,
            str(source_path),
            {"content_type": "application/octet-stream"},
        )
        self.assertEqual(uploaded.size, len(big))
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, big_key).size, len(big))

        stream_key = "it/%s/stream.bin" % token
        with open(str(source_path), "rb") as file_obj:
            streamed = CLIENT.put_object(MAIN_BUCKET, stream_key, file_obj)
        self.assertEqual(streamed.size, len(big))
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, stream_key).size, len(big))

        download_path = TMP_DIR / ("download-%s.bin" % token)
        CLIENT.get_object_to_file(MAIN_BUCKET, big_key, download_path)
        self.assertEqual(download_path.read_bytes(), big)

        partial_path = TMP_DIR / ("partial-%s.bin" % token)
        CLIENT.get_object_to_file(MAIN_BUCKET, big_key, str(partial_path), (0, 1023))
        self.assertEqual(partial_path.read_bytes(), big[:1024])

        missing_key = "it/%s/missing.txt" % token
        with self.assertRaises(NoSuchKeyError) as ctx:
            CLIENT.get_object(MAIN_BUCKET, missing_key)
        self.assertEqual(ctx.exception.status, 404)
        self.assertEqual(ctx.exception.code, "NoSuchKey")
        self.assertEqual(ctx.exception.request_id, "unknown")
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, missing_key))

    # ------------------------------------------------------------------
    def test_03_list_objects_pagination(self):
        token = uuid.uuid4().hex[:8]
        prefix = "it/%s/page/" % token
        for index in range(7):
            CLIENT.put_object(MAIN_BUCKET, "%sk%d.txt" % (prefix, index), b"v%d" % index)

        seen = []
        token_value = None
        pages = 0
        while True:
            page = CLIENT.list_objects_v2(
                MAIN_BUCKET,
                prefix=prefix,
                max_keys=3,
                continuation_token=token_value,
            )
            pages += 1
            self.assertLessEqual(len(page.items), 3)
            seen.extend(item.key for item in page.items)
            if not page.is_truncated:
                self.assertIsNone(page.next_token)
                break
            self.assertIsNotNone(page.next_token)
            token_value = page.next_token

        expected = ["%sk%d.txt" % (prefix, index) for index in range(7)]
        self.assertEqual(sorted(seen), expected)
        self.assertEqual(len(seen), len(set(seen)))
        self.assertGreaterEqual(pages, 3)

        iterated = [
            item.key for item in CLIENT.iter_objects_v2(MAIN_BUCKET, prefix=prefix, max_keys=2)
        ]
        self.assertEqual(sorted(iterated), expected)

        after = CLIENT.list_objects_v2(
            MAIN_BUCKET, prefix=prefix, start_after="%sk3.txt" % prefix, max_keys=100
        )
        self.assertEqual(
            [item.key for item in after.items],
            ["%sk%d.txt" % (prefix, index) for index in range(4, 7)],
        )

        delimiter_prefix = "it/%s/delim/" % token
        CLIENT.put_object(MAIN_BUCKET, delimiter_prefix + "a1.txt", b"a")
        CLIENT.put_object(MAIN_BUCKET, delimiter_prefix + "sub/x.txt", b"x")
        CLIENT.put_object(MAIN_BUCKET, delimiter_prefix + "sub2/y.txt", b"y")
        grouped = CLIENT.list_objects_v2(MAIN_BUCKET, prefix=delimiter_prefix, delimiter="/")
        self.assertEqual(
            [item.key for item in grouped.items], [delimiter_prefix + "a1.txt"]
        )
        self.assertEqual(
            grouped.common_prefixes,
            [delimiter_prefix + "sub/", delimiter_prefix + "sub2/"],
        )

    # ------------------------------------------------------------------
    def test_04_copy_and_batch_delete(self):
        token = uuid.uuid4().hex[:8]
        source = "it/%s/copy/source.txt" % token
        dest = "it/%s/copy/dest.txt" % token
        body = b"copy source body"
        CLIENT.put_object(
            MAIN_BUCKET,
            source,
            body,
            {"content_type": "text/plain", "metadata": {"stage": "original"}},
        )

        copied = CLIENT.copy_object(MAIN_BUCKET, source, MAIN_BUCKET, dest)
        self.assertEqual(copied.etag, hashlib.md5(body).hexdigest())
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, dest), body)
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, dest).metadata["stage"], "original")

        replaced = CLIENT.copy_object(
            MAIN_BUCKET, source, MAIN_BUCKET, dest, metadata={"stage": "new"}
        )
        self.assertTrue(replaced.etag)
        head = CLIENT.head_object(MAIN_BUCKET, dest)
        self.assertEqual(head.metadata, {"stage": "new"})
        # REPLACE 未显式指定 Content-Type 时服务端沿用源对象的类型
        self.assertEqual(head.content_type, "text/plain")

        explicit_copy = CLIENT.copy_object(
            MAIN_BUCKET,
            source,
            MAIN_BUCKET,
            dest,
            metadata={"stage": "ignored"},
            metadata_directive="COPY",
        )
        self.assertTrue(explicit_copy.etag)
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, dest).metadata["stage"], "original")

        # 空 metadata dict 一律按 COPY，不因“传了但为空”判成 REPLACE
        CLIENT.copy_object(MAIN_BUCKET, source, MAIN_BUCKET, dest, metadata={})
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, dest).metadata["stage"], "original")

        keys = ["it/%s/batch/a.txt" % token, "it/%s/batch/b.txt" % token]
        for key in keys:
            CLIENT.put_object(MAIN_BUCKET, key, b"batch")
        missing = "it/%s/batch/missing.txt" % token
        result = CLIENT.delete_objects(MAIN_BUCKET, keys + [missing])
        self.assertFalse(result.errors)
        self.assertEqual(sorted(result.deleted), sorted(keys + [missing]))
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, keys[0]))
        with self.assertRaises(NoSuchKeyError):
            CLIENT.get_object(MAIN_BUCKET, keys[0])

        # 空 keys 返回空结果，不抛异常
        empty = CLIENT.delete_objects(MAIN_BUCKET, [])
        self.assertEqual(empty.deleted, [])
        self.assertEqual(empty.errors, [])

    # ------------------------------------------------------------------
    def test_05_presigned_get_and_put(self):
        token = uuid.uuid4().hex[:8]
        key = "it/%s/presign/download.txt" % token
        body = b"presigned download body"
        CLIENT.put_object(MAIN_BUCKET, key, body, {"content_type": "text/plain"})

        url = CLIENT.presign_get(MAIN_BUCKET, key, expires_in=600)
        self.assertIn("X-Amz-Algorithm=AWS4-HMAC-SHA256", url)
        self.assertIn("X-Amz-Expires=600", url)
        self.assertIn("X-Amz-Signature=", url)
        status, fetched = _fetch(url)
        self.assertEqual(status, 200)
        self.assertEqual(fetched, body)

        # 默认过期时间统一为 900 秒
        self.assertIn("X-Amz-Expires=900", CLIENT.presign_get(MAIN_BUCKET, key))
        self.assertIn("X-Amz-Expires=900", CLIENT.presign_put(MAIN_BUCKET, key + ".put"))

        upload_key = "it/%s/presign/upload.txt" % token
        put_url = CLIENT.presign_put(MAIN_BUCKET, upload_key, expires_in=600)
        status, _ = _fetch(put_url, method="PUT", data=b"uploaded via presign")
        self.assertEqual(status, 200)
        self.assertEqual(
            CLIENT.get_object(MAIN_BUCKET, upload_key), b"uploaded via presign"
        )

        expired_url = CLIENT.presign_get(MAIN_BUCKET, key, expires_in=1)
        time.sleep(2.5)
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            _fetch(expired_url)
        self.assertEqual(ctx.exception.code, 403)
        error_body = ctx.exception.read()
        self.assertIn(b"AccessDenied", error_body)

    # ------------------------------------------------------------------
    def test_06_multipart_upload(self):
        token = uuid.uuid4().hex[:8]
        key = "it/%s/multipart/object.bin" % token
        data1 = b"A" * (1024 * 1024)
        data2 = b"B" * (512 * 1024)

        upload_id = CLIENT.create_multipart_upload(
            MAIN_BUCKET,
            key,
            content_type="application/octet-stream",
            metadata={"origin": "multipart"},
        )
        etag1 = CLIENT.upload_part(MAIN_BUCKET, key, upload_id, 1, data1)
        etag2 = CLIENT.upload_part(MAIN_BUCKET, key, upload_id, 2, data2)
        self.assertNotIn('"', etag1)
        self.assertEqual(etag1, hashlib.md5(data1).hexdigest())
        self.assertEqual(etag2, hashlib.md5(data2).hexdigest())

        listed = CLIENT.list_parts(MAIN_BUCKET, key, upload_id)
        self.assertEqual([part.part_number for part in listed.parts], [1, 2])
        self.assertEqual([part.size for part in listed.parts], [len(data1), len(data2)])

        final_etag = CLIENT.complete_multipart_upload(
            MAIN_BUCKET,
            key,
            upload_id,
            [
                # 故意乱序提交，SDK 会按 part_number 升序排序
                {"part_number": 2, "etag": etag2},
                {"part_number": 1, "etag": etag1},
            ],
        )
        expected_etag = (
            hashlib.md5(bytes.fromhex(etag1) + bytes.fromhex(etag2)).hexdigest() + "-2"
        )
        self.assertEqual(final_etag, expected_etag)
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, key).size, len(data1) + len(data2))
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key), data1 + data2)
        self.assertEqual(
            CLIENT.head_object(MAIN_BUCKET, key).metadata["origin"], "multipart"
        )
        with self.assertRaises(NoSuchUploadError):
            CLIENT.list_parts(MAIN_BUCKET, key, upload_id)

        # 低层 abort：取消后 upload 与分片都应消失
        abort_key = "it/%s/multipart/aborted.bin" % token
        abort_id = CLIENT.create_multipart_upload(MAIN_BUCKET, abort_key)
        CLIENT.upload_part(MAIN_BUCKET, abort_key, abort_id, 1, b"tmp")
        CLIENT.abort_multipart_upload(MAIN_BUCKET, abort_key, abort_id)
        with self.assertRaises(NoSuchUploadError):
            CLIENT.list_parts(MAIN_BUCKET, abort_key, abort_id)
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, abort_key))

        # 高层 upload_file：超过阈值自动分片
        large_path = TMP_DIR / ("multipart-%s.bin" % token)
        large_data = os.urandom(3 * 1024 * 1024 + 512 * 1024)
        large_path.write_bytes(large_data)
        large_key = "it/%s/multipart/large.bin" % token
        recorded = {}
        original_create = CLIENT.create_multipart_upload

        def spy_create(bucket, upload_key, content_type=None, metadata=None):
            upload = original_create(bucket, upload_key, content_type, metadata)
            recorded["upload_id"] = upload
            return upload

        with mock.patch.object(CLIENT, "create_multipart_upload", new=spy_create):
            uploaded = CLIENT.upload_file(
                MAIN_BUCKET, large_key, str(large_path), {"part_size": 1024 * 1024}
            )
        self.assertEqual(uploaded.size, len(large_data))
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, large_key), large_data)
        with self.assertRaises(NoSuchUploadError):
            CLIENT.list_parts(MAIN_BUCKET, large_key, recorded["upload_id"])

        # 高层 upload_file：分片失败自动 abort
        fail_key = "it/%s/multipart/fail.bin" % token
        fail_path = TMP_DIR / ("fail-%s.bin" % token)
        fail_path.write_bytes(os.urandom(2 * 1024 * 1024))
        failed_upload = {}
        original_upload_part = CLIENT._upload_part

        def spy_fail_create(bucket, upload_key, content_type=None, metadata=None):
            upload = original_create(bucket, upload_key, content_type, metadata)
            failed_upload["upload_id"] = upload
            return upload

        def failing_upload_part(
            bucket, upload_key, upload_id, part_number, data, content_md5=None
        ):
            if part_number == 2:
                raise OSSClientError("模拟分片网络失败")
            return original_upload_part(
                bucket,
                upload_key,
                upload_id,
                part_number,
                data,
                content_md5=content_md5,
            )

        with mock.patch.object(
            CLIENT, "create_multipart_upload", new=spy_fail_create
        ), mock.patch.object(
            CLIENT, "_upload_part", new=failing_upload_part
        ), mock.patch.object(
            CLIENT, "abort_multipart_upload", wraps=CLIENT.abort_multipart_upload
        ) as abort_spy:
            with self.assertRaises(OSSClientError):
                CLIENT.upload_file(
                    MAIN_BUCKET, fail_key, fail_path, {"part_size": 1024 * 1024}
                )
            abort_spy.assert_called_once()

        with self.assertRaises(NoSuchUploadError):
            CLIENT.list_parts(MAIN_BUCKET, fail_key, failed_upload["upload_id"])
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, fail_key))

    # ------------------------------------------------------------------
    def test_07_put_object_auto_multipart(self):
        token = uuid.uuid4().hex[:8]
        key = "it/%s/auto/big.bin" % token
        data = os.urandom(2 * 1024 * 1024 + 512 * 1024)
        recorded = {}
        original_create = CLIENT.create_multipart_upload

        def spy_create(bucket, upload_key, content_type=None, metadata=None):
            upload = original_create(bucket, upload_key, content_type, metadata)
            recorded["upload_id"] = upload
            return upload

        with mock.patch.object(CLIENT, "create_multipart_upload", new=spy_create):
            result = CLIENT.put_object(
                MAIN_BUCKET,
                key,
                data,
                {
                    "part_size": 1024 * 1024,
                    "content_type": "application/octet-stream",
                    "metadata": {"mode": "auto"},
                },
            )
        self.assertIn("upload_id", recorded)
        self.assertEqual(result.size, len(data))
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key), data)
        self.assertEqual(CLIENT.head_object(MAIN_BUCKET, key).metadata["mode"], "auto")
        with self.assertRaises(NoSuchUploadError):
            CLIENT.list_parts(MAIN_BUCKET, key, recorded["upload_id"])

        # 阈值内（或显式关闭）仍走单次 PutObject
        small_key = "it/%s/auto/small.bin" % token
        small = b"x" * 4096
        result = CLIENT.put_object(
            MAIN_BUCKET, small_key, small, {"part_size": 8 * 1024 * 1024}
        )
        self.assertEqual(result.etag, hashlib.md5(small).hexdigest())
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, small_key), small)

    # ------------------------------------------------------------------
    def test_08_content_md5_and_metadata_validation(self):
        token = uuid.uuid4().hex[:8]
        body = b"md5 checked body"
        digest = hashlib.md5(body).digest()

        key = "it/%s/md5/good.txt" % token
        result = CLIENT.put_object(
            MAIN_BUCKET,
            key,
            body,
            {"content_md5": base64.b64encode(digest).decode("ascii")},
        )
        self.assertEqual(result.etag, digest.hex())
        self.assertEqual(CLIENT.get_object(MAIN_BUCKET, key), body)

        # upload_file 同样支持 content_md5
        file_key = "it/%s/md5/file.txt" % token
        file_path = TMP_DIR / ("md5-%s.txt" % token)
        file_path.write_bytes(body)
        uploaded = CLIENT.upload_file(
            MAIN_BUCKET, file_key, file_path, {"content_md5": digest}
        )
        self.assertEqual(uploaded.etag, digest.hex())

        bad_key = "it/%s/md5/bad.txt" % token
        wrong = base64.b64encode(hashlib.md5(b"other").digest()).decode("ascii")
        with self.assertRaises(BadDigestError) as ctx:
            CLIENT.put_object(MAIN_BUCKET, bad_key, body, {"content_md5": wrong})
        # bytes 可重放：SDK 本地整对象校验直接拒绝，不会发出请求
        self.assertEqual(ctx.exception.code, "BadDigest")
        self.assertIsNone(ctx.exception.status)
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, bad_key))

        # 非法元数据在本地被拒绝，不会发出请求
        rejected_key = "it/%s/meta/rejected.txt" % token
        with self.assertRaises(ValueError):
            CLIENT.put_object(
                MAIN_BUCKET, rejected_key, b"x", {"metadata": {"bad key": "v"}}
            )
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, rejected_key))

        with self.assertRaises(ValueError):
            CLIENT.put_object(
                MAIN_BUCKET, rejected_key, b"x", {"metadata": {"note": "中文值"}}
            )
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, rejected_key))

        # 非法 content_md5 同样在本地被拒绝
        with self.assertRaises(ValueError):
            CLIENT.put_object(MAIN_BUCKET, rejected_key, body, {"content_md5": "bad!"})
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, rejected_key))

    # ------------------------------------------------------------------
    def test_09_auth_and_error_behaviors(self):
        wrong_secret = OSSClient(
            OSSConfig(
                endpoint=ENDPOINT,
                access_key=SERVER.app_id,
                secret_key="wrong-secret",
                bucket=MAIN_BUCKET,
                timeout=10,
                max_retries=0,
            )
        )
        with self.assertRaises(SignatureDoesNotMatchError) as ctx:
            wrong_secret.list_objects_v2(MAIN_BUCKET)
        self.assertEqual(ctx.exception.status, 403)
        wrong_secret.close()

        wrong_key = OSSClient(
            OSSConfig(
                endpoint=ENDPOINT,
                access_key="unknown-app-id",
                secret_key="whatever",
                bucket=MAIN_BUCKET,
                timeout=10,
                max_retries=0,
            )
        )
        with self.assertRaises(InvalidAccessKeyIdError) as ctx:
            wrong_key.list_objects_v2(MAIN_BUCKET)
        self.assertEqual(ctx.exception.status, 403)
        wrong_key.close()

        unknown_bucket = "py-sdk-missing-%s" % uuid.uuid4().hex[:10]
        with self.assertRaises(NoSuchBucketError):
            CLIENT.list_objects_v2(unknown_bucket)
        self.assertFalse(CLIENT.bucket_exists(unknown_bucket))
        self.assertFalse(CLIENT.head_bucket(unknown_bucket).exists)

        # 删除不存在的对象按 S3 语义返回成功
        CLIENT.delete_object(MAIN_BUCKET, "it/never-exists-%s.txt" % uuid.uuid4().hex)

    # ------------------------------------------------------------------
    def test_10_upload_part_content_md5_is_enforced(self):
        """自动分片逐片发送 Content-MD5；改坏其中一片的 MD5 应被服务端拒绝。"""
        import oss_client.client as client_module

        token = uuid.uuid4().hex[:8]
        key = "it/%s/md5/part.bin" % token
        data = os.urandom(2 * 1024 * 1024 + 123)

        original = client_module._md5_base64
        computed = []

        def faulty_part_md5(chunk):
            computed.append(len(chunk))
            if len(computed) == 2:
                # 第 2 片的 MD5 故意写错
                return base64.b64encode(hashlib.md5(b"wrong-part").digest()).decode("ascii")
            return original(chunk)

        with mock.patch.object(client_module, "_md5_base64", new=faulty_part_md5):
            with self.assertRaises(BadDigestError) as ctx:
                CLIENT.put_object(
                    MAIN_BUCKET, key, data, {"part_size": 1024 * 1024}
                )

        self.assertEqual(ctx.exception.status, 400)
        self.assertEqual(ctx.exception.code, "BadDigest")
        # 第 2 片即失败，后续分片不再上传
        self.assertEqual(len(computed), 2)
        self.assertFalse(CLIENT.object_exists(MAIN_BUCKET, key))


if __name__ == "__main__":
    unittest.main()
