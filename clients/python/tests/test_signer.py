"""SigV4 单元测试：使用 AWS 官方文档测试向量（非自签自验）。"""

import unittest
from datetime import datetime, timezone

from oss_client.signer import (
    ALGORITHM,
    EMPTY_SHA256,
    SigV4Signer,
    canonical_query_string,
    canonical_request,
    uri_encode,
)

ACCESS_KEY = "AKIAIOSFODNN7EXAMPLE"
SECRET_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
HOST = "examplebucket.s3.amazonaws.com"
MOMENT = datetime(2013, 5, 24, 0, 0, 0, tzinfo=timezone.utc)


class SigV4OfficialVectorTest(unittest.TestCase):
    def setUp(self):
        self.signer = SigV4Signer(ACCESS_KEY, SECRET_KEY, "us-east-1")

    def test_get_object_header_signature_matches_aws_doc(self):
        """AWS S3 文档 Example: GET Object（header 签名）。"""
        headers = {"Host": HOST, "Range": "bytes=0-9"}
        signed = self.signer.sign_request(
            "GET",
            "/test.txt",
            [],
            headers,
            EMPTY_SHA256,
            now=MOMENT,
            signed_headers=["host", "range", "x-amz-content-sha256", "x-amz-date"],
        )

        self.assertEqual(signed["x-amz-date"], "20130524T000000Z")
        self.assertEqual(signed["x-amz-content-sha256"], EMPTY_SHA256)
        self.assertEqual(
            signed["Authorization"],
            "AWS4-HMAC-SHA256 "
            "Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,"
            "SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,"
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
        )

        canonical = canonical_request(
            "GET",
            "/test.txt",
            "",
            dict(signed),
            ["host", "range", "x-amz-content-sha256", "x-amz-date"],
            EMPTY_SHA256,
        )
        self.assertEqual(
            canonical,
            "GET\n"
            "/test.txt\n"
            "\n"
            "host:examplebucket.s3.amazonaws.com\n"
            "range:bytes=0-9\n"
            "x-amz-content-sha256:" + EMPTY_SHA256 + "\n"
            "x-amz-date:20130524T000000Z\n"
            "\n"
            "host;range;x-amz-content-sha256;x-amz-date\n" + EMPTY_SHA256,
        )

    def test_list_objects_header_signature_matches_aws_doc(self):
        """AWS S3 文档 Example: GET Bucket (List Objects)，query 参与签名。"""
        signed = self.signer.sign_request(
            "GET",
            "/",
            [("max-keys", 2), ("prefix", "J")],
            {"Host": HOST},
            EMPTY_SHA256,
            now=MOMENT,
        )
        self.assertIn(
            "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7",
            signed["Authorization"],
        )

        canonical = canonical_request(
            "GET",
            "/",
            "max-keys=2&prefix=J",
            dict(signed),
            ["host", "x-amz-content-sha256", "x-amz-date"],
            EMPTY_SHA256,
        )
        self.assertIn("max-keys=2&prefix=J", canonical)

    def test_presigned_get_matches_aws_doc(self):
        """AWS S3 文档 Example: 预签名 GET URL。"""
        query = self.signer.presign(
            "GET",
            "/test.txt",
            HOST,
            86400,
            now=MOMENT,
        )
        self.assertEqual(
            query,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256"
            "&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
            "&X-Amz-Date=20130524T000000Z"
            "&X-Amz-Expires=86400"
            "&X-Amz-SignedHeaders=host"
            "&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404",
        )

    def test_canonical_query_sorts_and_excludes_signature(self):
        pairs = [
            ("b", "2"),
            ("a", "1"),
            ("X-Amz-Signature", "deadbeef"),
            ("a", "0"),
        ]
        self.assertEqual(canonical_query_string(pairs), "a=0&a=1&b=2")
        self.assertEqual(
            canonical_query_string(
                [("prefix", "J"), ("lifecycle", "delimiter/"), ("max-keys", "2")]
            ),
            "lifecycle=delimiter%2F&max-keys=2&prefix=J",
        )

    def test_uri_encode(self):
        self.assertEqual(uri_encode("a b/c~"), "a%20b%2Fc~")
        self.assertEqual(uri_encode("a b/c~", encode_slash=False), "a%20b/c~")
        self.assertEqual(uri_encode("\u4e2d"), "%E4%B8%AD")
        self.assertEqual(uri_encode("AZaz09-_.~"), "AZaz09-_.~")

    def test_presign_expiry_validation(self):
        with self.assertRaises(ValueError):
            self.signer.presign("GET", "/k", HOST, 0, now=MOMENT)
        with self.assertRaises(ValueError):
            self.signer.presign("GET", "/k", HOST, 604801, now=MOMENT)

    def test_presign_supports_get_and_put(self):
        for method in ("GET", "PUT"):
            query = self.signer.presign(method, "/k", HOST, 600, now=MOMENT)
            self.assertTrue(query.startswith("X-Amz-Algorithm=" + ALGORITHM))
            self.assertIn("X-Amz-Signature=", query)


if __name__ == "__main__":
    unittest.main()
