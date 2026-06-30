# claw-tap gateway 模式

Author: kejiqing

设置 `CLAW_CLUSTER_ID` 与 `CLAW_GATEWAY_DATABASE_URL`（与 http-gateway-rs 相同）后，claude-tap 进入 **claw gateway 模式**。

## 概览

- Tap **直连与 http-gateway-rs 相同的 PostgreSQL**（不经 gateway HTTP）。
- 定时从 PG 重载当前 LLM（`gateway_llm_cluster_state` + `gateway_llm_cluster_revision`，按 `CLAW_CLUSTER_ID`）。
- 将 OpenAI 兼容流量代理到配置的下游 LLM base URL。
- **不使用** `--tap-target`、`OPENAI_BASE_URL`、`UPSTREAM_OPENAI_BASE_URL`，也不使用 `--tap-upstream-config` / `claw-tap-upstream.json`。
- PG 无 active model 时 tap 拒绝启动，`/healthz` 返回 `ok: false`。
- 轮询间隔：`CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS`（默认 30）。Gateway Admin 在 PG 中 apply model 后，tap 下次轮询即生效，无需重启。

## API key 管理

自 **v0.0.11** 起，上游 LLM 的 API key 由 **PostgreSQL 统一管理**，不由 worker 客户端携带。

| 来源 | 表 / 字段 |
|------|-----------|
| Cluster 表（推荐） | `gateway_llm_cluster_model.api_key_ciphertext`（AES-GCM，以 `cluster_id` 为密钥） |
| Legacy 单例表 | `gateway_global_settings.llm_model_api_keys_json` |

转发 HTTP / WebSocket 到上游 LLM 时：

1. Tap 从 PG 加载并解密当前模型的 API key。
2. 若 DB key 非空，tap **替换**客户端的 `Authorization` 与 `x-api-key`，使用 DB key。
3. Header 格式取决于 `--tap-client`：
   - `codex` 等 OpenAI 兼容客户端 → `Authorization: Bearer <db-key>`
   - `claude` → `x-api-key: <db-key>`
4. 若 DB key 为空，仍透传客户端鉴权 header（向后兼容）。

**对 worker 的含义**

- Worker 只需将 `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` 指向 tap（如 `http://tap-host:8080/v1`）。
- Worker **不需要**有效的上游 LLM API key；key 由 Gateway Admin 写入 PG。
- 当 DB 有 key 时，客户端自带的 key（如 worker 的 `OPENAI_API_KEY`）**不会**发往真实 LLM。
- Trace 记录中仍会对鉴权 header **脱敏**。

## 配置

复制 [`.env.example`](../.env.example) 为 `.env`，填写 **Mode B** 块（compose 部署时再加端口）。不要把整个 claw-code `.env` 拷进本仓库。

| 变量 | 必填 | 说明 |
|------|------|------|
| `CLAW_CLUSTER_ID` | 是 | 集群标识，须与 http-gateway-rs 一致 |
| `CLAW_GATEWAY_DATABASE_URL` | 是 | PostgreSQL 连接串（与 gateway 相同） |
| `CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS` | 否 | 上游 URL 与 API key 刷新间隔（默认 `30`） |
| `CLAUDE_TAP_PORT` | 否 | 代理监听端口（默认 `8080`） |

## 示例（仅代理，端口 8080）

```bash
export CLAW_CLUSTER_ID=local-dev
export CLAW_GATEWAY_DATABASE_URL=postgres://claw_gateway:secret@postgres:5432/claw_gateway

claude-tap --tap-no-launch --tap-host 0.0.0.0 --tap-port 8080 --tap-client codex
```

Gateway Admin 注册 `host` + `proxyPort`（8080），探测 `GET http://{host}:8080/healthz`，并向 worker 注入指向 tap 的 `OPENAI_BASE_URL`。

Worker 示例（worker 侧无需配置上游 LLM key）：

```bash
export OPENAI_BASE_URL=http://tap-host:8080/v1
codex -c 'openai_base_url="http://tap-host:8080/v1"'
```

## 健康检查

`GET /healthz` 返回：

- `ok` — tap 已从 PG 加载 active LLM
- `clusterId` — 与 `CLAW_CLUSTER_ID` 相同
- `clusterHash` — 与 claw-code 中 `cluster_identity.rs` 算法一致

响应中**不包含** `dbHost`。

## Docker Compose

容器部署见 [deploy-compose.md](deploy-compose.md)。

## 相关文档

- claw-code：`docs/claw-tap-integration-requirements.md`
- Docker：[deploy-compose.md](deploy-compose.md)
