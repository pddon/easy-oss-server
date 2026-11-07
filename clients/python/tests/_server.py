"""集成测试用的服务端进程管理（Windows / PowerShell 环境）。

- 优先使用 ``target/release/oss-server.exe``；
- 不存在时用 cargo 构建，并把 ``CARGO_TARGET_DIR`` 指向临时目录，避免与主构建抢锁；
- 固定监听 127.0.0.1:18103，数据目录为 ``%TEMP%\\oss-server-python-data``；
- 启动后通过管理 API 创建 App，返回 access_key/secret_key。
"""

import atexit
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
HOST = "127.0.0.1"
PORT = 18103
ENDPOINT = "http://%s:%d" % (HOST, PORT)
ADMIN_TOKEN = "python-client-admin-token"
DATA_DIR = Path(tempfile.gettempdir()) / "oss-server-python-data"
BUILD_DIR = Path(tempfile.gettempdir()) / "oss-server-build-py"


def _read_tail(path: Path, lines: int = 30) -> str:
    try:
        content = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return "<无日志>"
    return "\n".join(content[-lines:])


class TestServer:
    def __init__(self) -> None:
        self.proc = None
        self.log_file = None
        self.app_id = None
        self.app_secret = None
        self.binary = None

    # ------------------------------------------------------------------
    def start(self):
        if os.environ.get("OSS_SKIP_INTEGRATION"):
            raise RuntimeError("OSS_SKIP_INTEGRATION 已设置，跳过集成测试")
        self._reset_data_dir()
        self._ensure_port_free()
        self.binary = self._resolve_binary()
        self._launch()
        try:
            self._wait_healthy()
            self._create_app()
        except Exception:
            self.stop()
            raise
        atexit.register(self.stop)
        return {"app_id": self.app_id, "app_secret": self.app_secret}

    def stop(self) -> None:
        proc = self.proc
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
        self.proc = None
        if self.log_file is not None:
            try:
                self.log_file.close()
            except OSError:
                pass
            self.log_file = None

    # ------------------------------------------------------------------
    def _reset_data_dir(self) -> None:
        shutil.rmtree(str(DATA_DIR), ignore_errors=True)
        DATA_DIR.mkdir(parents=True, exist_ok=True)

    def _ensure_port_free(self) -> None:
        try:
            with socket.create_connection((HOST, PORT), timeout=0.5):
                pass
        except OSError:
            return
        raise RuntimeError(
            "端口 %d 已被占用，请先停止残留的 oss-server 进程后重跑集成测试" % PORT
        )

    def _resolve_binary(self) -> Path:
        env_bin = os.environ.get("OSS_SERVER_BIN")
        if env_bin:
            candidate = Path(env_bin)
            if candidate.exists():
                return candidate
            raise RuntimeError("OSS_SERVER_BIN 指向的文件不存在: %s" % candidate)

        built = None
        for name in ("oss-server.exe", "oss-server"):
            candidate = BUILD_DIR / "release" / name
            if candidate.exists():
                built = candidate
                break

        default = REPO_ROOT / "target" / "release" / "oss-server.exe"
        if default.exists() and self._is_fresh(default):
            return default
        if built is not None and self._is_fresh(built):
            return built

        cargo = shutil.which("cargo")
        if not cargo:
            raise RuntimeError(
                "未找到可用的 oss-server 二进制，且 cargo 不在 PATH 中；"
                "请先构建服务端或设置 OSS_SERVER_BIN"
            )
        BUILD_DIR.mkdir(parents=True, exist_ok=True)
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(BUILD_DIR)
        toolchain = Path(os.environ.get("USERPROFILE", "")) / (
            ".rustup/toolchains/stable-x86_64-pc-windows-gnu"
        )
        if toolchain.exists():
            env["PATH"] = "%s;%s;%s" % (
                toolchain / "bin",
                toolchain / "lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained",
                env.get("PATH", ""),
            )
        print(
            "[integration] 正在构建最新服务端，CARGO_TARGET_DIR=%s" % BUILD_DIR
        )
        subprocess.run(
            [cargo, "build", "--release"],
            cwd=str(REPO_ROOT),
            env=env,
            check=True,
        )
        for name in ("oss-server.exe", "oss-server"):
            candidate = BUILD_DIR / "release" / name
            if candidate.exists():
                return candidate
        raise RuntimeError("cargo 构建完成但未找到 oss-server 可执行文件")

    @staticmethod
    def _is_fresh(binary: Path) -> bool:
        """二进制时间戳不早于 ``src`` 下最新的 Rust 源码时视为可用。"""
        try:
            newest = max(
                path.stat().st_mtime for path in (REPO_ROOT / "src").rglob("*.rs")
            )
            return binary.stat().st_mtime >= newest
        except (OSError, ValueError):
            return False

    def _launch(self) -> None:
        self.log_file = open(str(DATA_DIR / "server.log"), "wb")
        env = os.environ.copy()
        env.update(
            {
                "OSS_HOST": "%s:%d" % (HOST, PORT),
                "OSS_DATA_DIR": str(DATA_DIR),
                "OSS_DB_URL": str(DATA_DIR / "oss.db"),
                "OSS_STORAGE_BACKEND": "redb",
                "OSS_ADMIN_TOKEN": ADMIN_TOKEN,
                "OSS_STARTUP_CHECK_MODE": "report",
                "OSS_CLEANUP_INTERVAL_MINUTES": "0",
                "OSS_TRASH_RETENTION_DAYS": "0",
                "RUST_LOG": "warn",
            }
        )
        kwargs = {}
        if os.name == "nt":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        self.proc = subprocess.Popen(
            [str(self.binary)],
            cwd=str(REPO_ROOT),
            env=env,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            **kwargs
        )

    def _wait_healthy(self) -> None:
        deadline = time.time() + 30
        url = ENDPOINT + "/healthz"
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(
                    "服务端启动失败（退出码 %s），日志尾部:\n%s"
                    % (self.proc.returncode, _read_tail(DATA_DIR / "server.log"))
                )
            try:
                with urllib.request.urlopen(url, timeout=1) as resp:
                    if resp.status == 200:
                        return
            except (urllib.error.URLError, OSError):
                pass
            time.sleep(0.25)
        raise RuntimeError(
            "等待服务端健康检查超时，日志尾部:\n%s"
            % _read_tail(DATA_DIR / "server.log")
        )

    def _create_app(self) -> None:
        name = "python-client-%s" % uuid.uuid4().hex[:10]
        payload = json.dumps({"name": name}).encode("utf-8")
        request = urllib.request.Request(
            ENDPOINT + "/api/admin/apps",
            data=payload,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "X-Admin-Token": ADMIN_TOKEN,
            },
        )
        with urllib.request.urlopen(request, timeout=10) as resp:
            if resp.status not in (200, 201):
                raise RuntimeError("创建 App 失败: HTTP %s" % resp.status)
            data = json.loads(resp.read().decode("utf-8"))
        self.app_id = data["app_id"]
        self.app_secret = data["app_secret"]
