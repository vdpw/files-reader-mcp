# 首版验证记录

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
