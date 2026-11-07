# 贡献指南

感谢你愿意为 OSS Server 提交代码、文档或问题反馈。提交前请先阅读本文件。

## 开发环境

- Rust 1.87+（默认构建使用 redb，不依赖 C 编译器）
- 可选：Docker 20.10+、MySQL 8（仅在验证 `mysql` feature 时需要）
- 客户端：JDK 8+ / Node.js 14.17+ / Python 3.8+

常用命令：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

客户端各自的测试命令见 `clients/<language>/README.md`。

## 提交前检查

1. 代码能编译，新增逻辑有对应测试；不要降低现有测试覆盖率。
2. 不提交 `target/`、`node_modules/`、`__pycache__`、构建产物、数据库文件和任何真实密钥。
3. 新增或修改配置项时，同步更新 README 的配置表与 `dist/.env.example`。
4. 新增或修改接口行为时，同步更新 `docs/s3-compatible-api.md`。
5. 改动客户端时，确认五门语言的统一契约（方法名、参数顺序、返回语义）没有被破坏。

## 提交信息与 PR

- 提交信息用一句话说明“为什么改”，例如 `fix: 修复 Range 请求末尾字节越界`。
- 一个 PR 只做一件事；较大的改动请先开 issue 说明动机和方案。
- PR 描述里写清楚：改了什么、如何验证、是否有破坏性变更。

## 行为准则

- 尊重不同经验水平的贡献者，讨论聚焦技术与事实。
- 不接受包含攻击性内容、真实凭据或他人隐私数据的提交。
