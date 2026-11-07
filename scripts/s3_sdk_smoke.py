#!/usr/bin/env python3
"""S3 兼容层冒烟测试（使用官方 boto3 SDK，而不是自研签名）。

用法：
    python scripts/s3_sdk_smoke.py --endpoint http://127.0.0.1:19000 \
        --access-key <App ID> --secret-key <App Secret> [--bucket s3-smoke]

覆盖：建桶/列桶/HeadBucket、Put/Get/Head/Delete/Copy、ListObjectsV2 分页与
delimiter、批量删除、分片上传、预签名 GET/PUT。
"""

import argparse
import sys

import boto3
from botocore.client import Config
from botocore.exceptions import ClientError


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--access-key", required=True)
    parser.add_argument("--secret-key", required=True)
    parser.add_argument("--bucket", default="s3-sdk-smoke")
    args = parser.parse_args()

    client = boto3.client(
        "s3",
        endpoint_url=args.endpoint,
        aws_access_key_id=args.access_key,
        aws_secret_access_key=args.secret_key,
        region_name="us-east-1",
        config=Config(s3={"addressing_style": "path"}),
    )

    bucket = args.bucket
    checks = []

    def ok(name, fn):
        fn()
        checks.append(name)
        print(f"  PASS  {name}")

    # 清空历史数据（忽略错误）
    try:
        listed = client.list_objects_v2(Bucket=bucket)
        for obj in listed.get("Contents", []):
            client.delete_object(Bucket=bucket, Key=obj["Key"])
        client.delete_bucket(Bucket=bucket)
    except ClientError:
        pass

    ok("create_bucket", lambda: client.create_bucket(Bucket=bucket))
    ok("head_bucket", lambda: client.head_bucket(Bucket=bucket))
    ok("list_buckets", lambda: client.list_buckets())

    ok(
        "put_object",
        lambda: client.put_object(
            Bucket=bucket,
            Key="hello.txt",
            Body=b"hello from boto3",
            ContentType="text/plain",
            Metadata={"color": "blue"},
        ),
    )
    body = client.get_object(Bucket=bucket, Key="hello.txt")["Body"].read()
    assert body == b"hello from boto3", body
    ok("get_object", lambda: None)

    head = client.head_object(Bucket=bucket, Key="hello.txt")
    assert head["ContentLength"] == 16, head
    assert head["Metadata"].get("color") == "blue", head
    ok("head_object_metadata", lambda: None)

    ranged = client.get_object(Bucket=bucket, Key="hello.txt", Range="bytes=0-4")
    assert ranged["Body"].read() == b"hello", ranged
    ok("range_get", lambda: None)

    ok(
        "copy_object",
        lambda: client.copy_object(
            Bucket=bucket,
            Key="copy.txt",
            CopySource={"Bucket": bucket, "Key": "hello.txt"},
        ),
    )

    for key in ["p/1.txt", "p/2.txt", "q/1.txt"]:
        client.put_object(Bucket=bucket, Key=key, Body=b"x")

    listed = client.list_objects_v2(Bucket=bucket, Prefix="p/")
    assert {o["Key"] for o in listed.get("Contents", [])} == {"p/1.txt", "p/2.txt"}, listed
    ok("list_v2_prefix", lambda: None)

    page1 = client.list_objects_v2(Bucket=bucket, MaxKeys=2)
    assert page1["IsTruncated"] and "NextContinuationToken" in page1, page1
    page2 = client.list_objects_v2(
        Bucket=bucket, MaxKeys=2, ContinuationToken=page1["NextContinuationToken"]
    )
    assert page2["KeyCount"] == 2 and not page2["IsTruncated"], page2
    ok("list_v2_pagination", lambda: None)

    grouped = client.list_objects_v2(Bucket=bucket, Delimiter="/")
    prefixes = {p["Prefix"] for p in grouped.get("CommonPrefixes", [])}
    assert {"p/", "q/"} <= prefixes, grouped
    ok("list_v2_delimiter", lambda: None)

    ok(
        "delete_objects",
        lambda: client.delete_objects(
            Bucket=bucket,
            Delete={
                "Objects": [{"Key": "p/1.txt"}, {"Key": "q/1.txt"}, {"Key": "missing"}],
                "Quiet": False,
            },
        ),
    )

    # ---- 分片上传 ----
    upload_id = client.create_multipart_upload(Bucket=bucket, Key="multi.bin", Metadata={"m": "1"})[
        "UploadId"
    ]
    etags = []
    for number, payload in [(1, b"A" * 1024), (2, b"B" * 1024)]:
        part = client.upload_part(
            Bucket=bucket, Key="multi.bin", UploadId=upload_id, PartNumber=number, Body=payload
        )
        etags.append({"PartNumber": number, "ETag": part["ETag"]})

    parts = client.list_parts(Bucket=bucket, Key="multi.bin", UploadId=upload_id)
    assert len(parts["Parts"]) == 2, parts
    ok("multipart_list_parts", lambda: None)

    client.complete_multipart_upload(
        Bucket=bucket,
        Key="multi.bin",
        UploadId=upload_id,
        MultipartUpload={"Parts": etags},
    )
    multi = client.get_object(Bucket=bucket, Key="multi.bin")
    assert len(multi["Body"].read()) == 2048, multi
    assert multi["Metadata"].get("m") == "1", multi
    ok("multipart_complete", lambda: None)

    upload_id = client.create_multipart_upload(Bucket=bucket, Key="abort.bin")["UploadId"]
    client.upload_part(Bucket=bucket, Key="abort.bin", UploadId=upload_id, PartNumber=1, Body=b"z")
    client.abort_multipart_upload(Bucket=bucket, Key="abort.bin", UploadId=upload_id)
    ok("multipart_abort", lambda: None)

    # ---- 预签名 GET / PUT ----
    url = client.generate_presigned_url(
        "get_object", Params={"Bucket": bucket, "Key": "hello.txt"}, ExpiresIn=300
    )
    import urllib.request

    with urllib.request.urlopen(url) as response:
        assert response.read() == b"hello from boto3", response.status
    ok("presigned_get", lambda: None)

    put_url = client.generate_presigned_url(
        "put_object", Params={"Bucket": bucket, "Key": "presigned.txt"}, ExpiresIn=300
    )
    request = urllib.request.Request(put_url, data=b"presigned payload", method="PUT")
    with urllib.request.urlopen(request) as response:
        assert response.status == 200, response.status
    assert client.get_object(Bucket=bucket, Key="presigned.txt")["Body"].read() == b"presigned payload"
    ok("presigned_put", lambda: None)

    # ---- 删除 ----
    for key in ["hello.txt", "copy.txt", "p/2.txt", "multi.bin", "presigned.txt"]:
        client.delete_object(Bucket=bucket, Key=key)
    assert client.list_objects_v2(Bucket=bucket).get("KeyCount", 0) == 0
    client.delete_bucket(Bucket=bucket)
    ok("delete_objects_and_bucket", lambda: None)

    print(f"\nALL {len(checks)} CHECKS PASSED")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001
        print(f"\nFAILED: {type(exc).__name__}: {exc}", file=sys.stderr)
        raise
