# 首版验证记录

## 2026-09-20：依赖升级

- 通过 crates.io 官方 API 逐项核对全部 22 个直接依赖（含开发依赖）的最新稳定版，将版本要求更新到对应版本，并刷新 `Cargo.lock`。实际跨版本升级为 `infer` 0.19.0 → 0.22.0、`sha1` 0.10.7 → 0.11.0、`similar` 2.7.0 → 3.2.0、`toml` 0.9.12 → 1.1.6、`jsonschema` 0.33.0 → 0.56.0。
- 间接依赖更新到上游约束允许的版本；例如 `axum` 仍依赖 `matchit` 0.8.4，`infer` 仍依赖 `cfb` 0.14.0。锁文件中的包数量由 214 降至 197。
- 验证环境为 macOS Apple Silicon、Rust/Cargo 1.98.1；保留已有的最低 Rust 1.98 要求，并同步 README。适配 `jsonschema::ValidationError::instance_path()`，按新 Clippy 要求将 UTF-16 解码的固定长度分块改用 `as_chunks::<2>()`。
- `cargo fmt --check`、`cargo clippy --all-targets --locked --offline -- -D warnings`、`cargo test --locked --offline`（26 项集成测试）和 `cargo build --release --locked --offline` 全部通过。
- `env -u SSLKEYLOGFILE python3 tests/smoke_http.py --binary target/release/files-reader-mcp` 通过：真实回环 HTTP 初始化、18 个工具枚举、结构化结果、原始 CRLF 和路径越界拒绝均正常。测试使用独立临时端口并在结束后停止，未重启已有常驻服务。

## 2026-09-18：输出 Schema

- 全部 18 个工具的 `tools/list` 均包含非空 `outputSchema`，通过 JSON Schema 校验器编译；逐一调用工具，验证 `structuredContent` 符合各自 Schema，并与 `content[0].text` 解码后的 JSON 相同。
- 边界测试验证 2 KiB / 2 条目预算下的分页、文本分段、搜索上下文省略、发现/搜索/状态缺口、二进制补丁省略，以及首次提交和后续提交的历史搜索结构。失败调用保持 `isError=true` 文本错误，不伪装成成功结构。
- `cargo fmt --check`、`cargo clippy --all-targets --locked --offline -- -D warnings`、`cargo test --locked --offline`（26 项集成测试）及 `cargo build --release --locked --offline` 全部通过。
- `python3 tests/smoke_http.py --binary target/release/files-reader-mcp` 通过，真实临时回环 HTTP 服务已验证 Schema 枚举、结构化/文本结果一致、CRLF 保留及路径越界拒绝。
- 本次未重启已有服务，未检查 ChatGPT 连接界面；上述 HTTP 测试使用独立临时端口并在结束后停止。

## 2026-09-17：首版

验证日期：2026-09-17。环境：macOS Apple Silicon，Rust/Cargo 1.93.1，测试夹具使用 Git 2.54.0。Linux 使用相同 Unix 文件句柄实现，但本次没有在 Linux 主机运行测试。

## 结果

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo clippy --all-targets --locked -- -D warnings` | 通过，无警告 |
| `cargo test --locked -- --test-threads=4` | 25 项集成测试全部通过 |
| `cargo build --release --locked` | 通过 |
| `python3 tests/smoke_http.py --binary target/release/files-reader-mcp` | 真实回环 HTTP 测试通过 |
| `git check-ignore config.toml target/example` | 本地配置及构建产物均被忽略 |

HTTP 测试实际启动服务并绑定操作系统分配的回环端口，完成 `initialize`、18 个工具枚举、文本读取（缩进/CRLF 保留）和越界路径拒绝，随后停止服务。未修改客户端配置，未部署为常驻后台服务。

## 覆盖内容

- 普通授权目录下多个独立子仓库；多个授权根之间独立定位。
- 隐藏文件和被 `.gitignore` 忽略的文本仍可阅读；目录深度/覆盖缺口。
- `..`、绝对路径、目录符号链接、设备链接、FIFO 拒绝；根路径被替换后固定文件描述符仍只读原目录。
- UTF-8/BOM、UTF-16 LE/BE、CRLF、Unicode、无末尾换行、超长文本行及补丁行跨页完整还原。
- 编码错误、二进制、PDF、SVG（含误导扩展名/DOCTYPE/命名空间）排除；包含 SVG 字符串的源代码正常可读。
- 搜索分页、二进制未覆盖项、文件改变后旧读取游标失效。
- 本地 Git refs、annotated tag、短对象 ID、祖先解析、历史、提交差异、状态、暂存/工作区差异、模式变化、first-parent blame 和共同祖先。
- loose/pack 对象及 delta、packed refs、index v4；缺失 blob 直接错误而非无命中。
- linked worktree 及其主仓库管理目录拒绝；alternates 和 Git 对象目录符号链接拒绝。
- 二进制/SVG/历史符号链接不经 Git 补丁泄漏；恶意 diff/textconv/fsmonitor/include 配置不执行，index/config 不变且无 index.lock。
- HTTP Host/Origin/bearer 检查、64 KiB 请求体限制、MCP 初始化与全部工具调用、未知参数拒绝。

首版的 SHA-1/索引/历史遍历边界及 first-parent 语义见 README；这些测试不等同于完整 Git 实现兼容性认证或独立安全审计。
