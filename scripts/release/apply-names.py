#!/usr/bin/env python3
"""把 scripts/release/release.env 中的名称与占位符同步到各清单和文档。

默认只预览（dry-run），加 --apply 才真正写入。
用法：
    python scripts/release/apply-names.py [--apply]
"""

import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONFIG = ROOT / "scripts" / "release" / "release.env"

TOKEN_FILES = [
    ROOT / "README.md",
    ROOT / "CONTRIBUTING.md",
    ROOT / "SECURITY.md",
    ROOT / "Dockerfile",
    ROOT / "docker-compose.yml",
    ROOT / "Cargo.toml",
    ROOT / "clients" / "README.md",
    ROOT / "clients" / "rust" / "Cargo.toml",
    ROOT / "clients" / "java" / "pom.xml",
    ROOT / "clients" / "js" / "package.json",
    ROOT / "clients" / "typescript" / "package.json",
    ROOT / "clients" / "python" / "pyproject.toml",
    ROOT / "dist" / "Dockerfile",
    ROOT / "dist" / "docker-compose.yml",
    ROOT / "scripts" / "release" / "release.env",
]

TOKENS = ["__GITHUB_ORG__", "__DOCKER_HUB_NAMESPACE__", "__SECURITY_CONTACT__",
          "__MAVEN_CENTRAL_STAGING_URL__"]


def load_config():
    values = {}
    for line in CONFIG.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = os.environ.get(key.strip(), value.strip())
    return values


def read_first(path, pattern):
    text = path.read_text(encoding="utf-8")
    match = re.search(pattern, text, re.MULTILINE)
    return match.group(1) if match else ""


def main():
    apply_changes = "--apply" in sys.argv[1:]
    cfg = load_config()

    token_map = {
        "__GITHUB_ORG__": cfg.get("GITHUB_ORG", ""),
        "__DOCKER_HUB_NAMESPACE__": cfg.get("DOCKERHUB_NAMESPACE", ""),
        "__SECURITY_CONTACT__": cfg.get("SECURITY_CONTACT", ""),
        "__MAVEN_CENTRAL_STAGING_URL__": cfg.get("MAVEN_CENTRAL_STAGING_URL", ""),
    }

    current = {
        "crate": read_first(ROOT / "clients" / "rust" / "Cargo.toml", r'^name = "([^"]+)"'),
        "maven_group": read_first(ROOT / "clients" / "java" / "pom.xml",
                                  r"<groupId>([^<]+)</groupId>"),
        "maven_artifact": read_first(ROOT / "clients" / "java" / "pom.xml",
                                     r"<artifactId>([^<]+)</artifactId>"),
        "npm_js": json.loads((ROOT / "clients" / "js" / "package.json").read_text(encoding="utf-8"))["name"],
        "npm_ts": json.loads((ROOT / "clients" / "typescript" / "package.json").read_text(encoding="utf-8"))["name"],
        "pypi": read_first(ROOT / "clients" / "python" / "pyproject.toml", r'^name = "([^"]+)"'),
    }
    wanted = {
        "crate": cfg.get("CRATE_NAME", current["crate"]),
        "maven_group": cfg.get("MAVEN_GROUP_ID", current["maven_group"]),
        "maven_artifact": cfg.get("MAVEN_ARTIFACT_ID", current["maven_artifact"]),
        "npm_js": cfg.get("NPM_JS_NAME", current["npm_js"]),
        "npm_ts": cfg.get("NPM_TS_NAME", current["npm_ts"]),
        "pypi": cfg.get("PYPI_NAME", current["pypi"]),
    }

    plans = {}

    def plan(path, old, new):
        if old != new:
            plans.setdefault(path, [])
            plans[path].append((old, new))

    plan(ROOT / "clients" / "rust" / "Cargo.toml", current["crate"], wanted["crate"])
    plan(ROOT / "clients" / "java" / "pom.xml", current["maven_group"], wanted["maven_group"])
    plan(ROOT / "clients" / "java" / "pom.xml", current["maven_artifact"], wanted["maven_artifact"])
    plan(ROOT / "clients" / "js" / "package.json", current["npm_js"], wanted["npm_js"])
    plan(ROOT / "clients" / "typescript" / "package.json", current["npm_ts"], wanted["npm_ts"])
    plan(ROOT / "clients" / "python" / "pyproject.toml", current["pypi"], wanted["pypi"])

    plan(ROOT / "clients" / "rust" / "README.md", current["crate"], wanted["crate"])
    plan(ROOT / "clients" / "js" / "README.md", current["npm_js"], wanted["npm_js"])
    plan(ROOT / "clients" / "typescript" / "README.md", current["npm_ts"], wanted["npm_ts"])
    plan(ROOT / "clients" / "python" / "README.md", current["pypi"], wanted["pypi"])
    plan(ROOT / "clients" / "java" / "README.md", current["maven_artifact"], wanted["maven_artifact"])

    # 根 README 与 clients/README 里出现的是“同一个旧名字对应多个包”。
    # 只有当这些包都改成同一个新名字时才批量替换，否则跳过并提示手工核对。
    shared = {}
    conflicts = set()
    for key in ("crate", "npm_js", "npm_ts", "pypi", "maven_artifact"):
        old, new = current[key], wanted[key]
        if old == new:
            continue
        if old in shared and shared[old] != new:
            conflicts.add(old)
        shared[old] = new
    for old, new in shared.items():
        if old in conflicts:
            print(f"[警告] 多个包共用旧名 {old} 但目标名不同，跳过根 README/clients README 的批量替换")
            continue
        plan(ROOT / "README.md", old, new)
        plan(ROOT / "clients" / "README.md", old, new)

    # Rust 包名变化时，crate 的模块名（oss_client）也要同步到源码与测试。
    crate_ident_old = current["crate"].replace("-", "_")
    crate_ident_new = wanted["crate"].replace("-", "_")
    if crate_ident_old != crate_ident_new:
        for rs in list((ROOT / "clients" / "rust" / "src").rglob("*.rs")) + \
                list((ROOT / "clients" / "rust" / "tests").rglob("*.rs")):
            plans.setdefault(rs, []).append((crate_ident_old, crate_ident_new))

    changed_files = 0
    for path in TOKEN_FILES + [p for p in plans if p not in TOKEN_FILES]:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        original = text

        for token in TOKENS:
            if token in text:
                value = token_map.get(token, "")
                if not value or "__" in value:
                    print(f"[跳过] {path.relative_to(ROOT)} 中的 {token} 尚未配置（值={value!r}）")
                    continue
                text = text.replace(token, value)

        # 仓库改名后同步 URL：<org>/oss-server -> <org>/<GITHUB_REPO>。
        # oss-server 是最初的仓库名，改名后这里做一次性对齐，避免残留旧地址。
        repo_old = f"{token_map['__GITHUB_ORG__']}/oss-server"
        repo_new = f"{token_map['__GITHUB_ORG__']}/{cfg.get('GITHUB_REPO', 'oss-server')}"
        if repo_old != repo_new and repo_old in text:
            text = text.replace(repo_old, repo_new)

        for old, new in plans.get(path, []):
            if old in text:
                text = text.replace(old, new)

        if text != original:
            changed_files += 1
            print(f"[{'已写入' if apply_changes else '预览'}] {path.relative_to(ROOT)}")
            if apply_changes:
                with path.open("w", encoding="utf-8", newline="\n") as handle:
                    handle.write(text)

    print(f"\n共 {changed_files} 个文件{'已更新' if apply_changes else '待更新（加 --apply 写入）'}。")
    if not apply_changes:
        print("这是预览模式，未修改任何文件。")


if __name__ == "__main__":
    main()
