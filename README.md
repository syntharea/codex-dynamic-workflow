# Codex Dynamic Workflow

基于 [OpenAI Codex CLI](https://github.com/openai/codex) 的社区分支，增加 Claude Code 风格的 Dynamic Workflow 体验，并保持核心执行链路为 Rust 原生实现。

> 本项目不是 OpenAI 官方发行版。上游 Codex 的版权、商标和官方文档归其各自权利人所有。

## 主要特性

- **Rust 原生 Dynamic Workflow 引擎**：DAG 调度、并发控制、暂停、恢复、取消、失败重试、快照和冷恢复，不执行 JavaScript 工作流。
- **Claude Code 风格命令体验**：支持 `ultracode`、`/effort ultracode`、`/deep-research` 和 `/workflows`。
- **跨入口一致**：Dynamic Workflow 同时覆盖 TUI、`codex exec` 和 app-server v2。
- **保存与复用**：支持用户级 `$CODEX_HOME/workflows/*.json` 和受信任项目级 `.codex/workflows/*.json`。

## 不安装直接运行

需要 Rust 工具链和 `just`。首次运行会编译项目：

```bash
git clone https://github.com/syntharea/codex-dynamic-workflow.git
cd codex-dynamic-workflow

# 直接启动交互式 TUI
just codex
```

也可以不使用 `just`：

```bash
cd codex-rs
cargo run --bin codex --
```

运行非交互命令：

```bash
just exec "ultracode review this repository and fix the highest-risk issue"
```

## 登录

### API Key

```bash
printenv OPENAI_API_KEY | just codex login --with-api-key
```

也可以直接使用环境变量：

```bash
export OPENAI_API_KEY="your-api-key"
just codex
```

## Dynamic Workflow 用法

Dynamic Workflow 默认启用。

### Ultracode

在 TUI 输入普通提示词时，以 `ultracode` 开头：

```text
ultracode implement the feature, run tests, and review the final diff
```

也可以先选择一次性高强度工作流，再输入目标：

```text
/effort ultracode
```

非交互模式：

```bash
just exec "ultracode migrate the API and verify backward compatibility"
```

### Deep Research

```text
/deep-research compare the available architecture options
```

非交互模式：

```bash
just exec "/deep-research compare the available architecture options"
```

### Workflow 管理器

在 TUI 中打开管理器：

```text
/workflows
```

管理器支持查看状态，以及暂停、恢复、取消、重试、保存、运行和删除工作流。

## 配置

配置文件沿用 Codex 的 `config.toml`。以下字段均可选：

```toml
[workflows]
max_concurrent_workflows = 4
max_tasks_per_workflow = 64
max_concurrent_tasks = 8
max_task_attempts = 3
pause_timeout_seconds = 3600
```

覆盖 API Base URL：

```toml
[model_providers.openai]
base_url = "https://your-api.example.com/v1"
```

## App Server v2

新增 workflow RPC：

- `workflow/start`
- `workflow/read`
- `workflow/list`
- `workflow/pause`
- `workflow/resume`
- `workflow/cancel`
- `workflow/retry`
- `workflow/save`
- `workflow/run`
- `workflow/delete`

生命周期通知包括 `workflow/started`、`workflow/progress`、`workflow/paused`、`workflow/completed`、`workflow/failed` 和 `workflow/cancelled`。

协议说明见 [`codex-rs/app-server/README.md`](codex-rs/app-server/README.md)。

## 构建与测试

```bash
# 格式化
cd codex-rs && just fmt

# 运行指定 crate 测试
just test -p codex-workflows

# 运行完整 Rust workspace 测试
just test
```

Linux 上编译内嵌 bubblewrap 需要 `pkg-config` 和 libcap 开发包，例如 Ubuntu/Debian 的 `libcap-dev`。

## 与上游同步

本仓库基于 OpenAI Codex 开发。同步上游时建议保留独立 remote：

```bash
git remote add upstream https://github.com/openai/codex.git
git fetch upstream
git rebase upstream/main
```

## 文档

- [上游 Codex 官方文档](https://developers.openai.com/codex)
- [安装与构建](docs/install.md)
- [贡献指南](docs/contributing.md)
- [App Server API](codex-rs/app-server/README.md)

## License

本项目沿用上游仓库的 [Apache-2.0 License](LICENSE)。
