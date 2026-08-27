---
layout: default
title: RAG 与数据库设计
parent: 架构与设计
nav_order: 11
---

# Alex Runtime RAG 与数据库设计

> 目标设计文档，修订日期：2026-08-27。本文定义建议的产品边界、接口和实施顺序，不代表其中
> 的 API 已经实现。当前事实以 [`status.md`](./status.md) 为准，AI Runtime 的总体实施方案见
> [`ai-runtime-implementation.md`](./ai-runtime-implementation.md)。

## 1. 摘要

Alex Runtime 当前已经提供构建 RAG 所需的部分基础能力：`model.embed`、模型生成、Agent、MCP、
应用级数据目录、`storage.*` 和长期运行的 Node/Python service。它尚未提供文档解析、切分、向量
索引、检索、重排、引用或知识库管理，也没有平台级关系数据库 API 或内建向量数据库。

本设计建议新增一个可选的 **Alex Knowledge Service**，作为受 Runtime 管理的官方应用层服务：

- 为应用和 Agent 提供稳定的 `knowledge.*` API；
- 本地默认使用 SQLite 保存元数据、文本块、任务与访问控制信息；
- 向量索引通过可替换的 `VectorIndex` 适配器提供，本地实现优先采用 SQLite 向量扩展；
- Embedding 统一调用现有 `model.embed`，不重复建设模型生命周期；
- 外部 PostgreSQL/pgvector、Qdrant、Milvus 等通过受权限控制的适配器或 MCP 接入；
- Runtime 核心只负责身份、权限、Secret、进程、资源、审计和生命周期，不承担检索算法实现。

第一版目标是可靠的单机、单用户、本地优先 RAG，不以分布式向量数据库、多租户 SaaS 或数据仓库为目标。

## 2. 当前能力与缺口

### 2.1 已有能力

| 能力 | 当前状态 | 可复用位置 |
| --- | --- | --- |
| 文本向量化 | 已接线 | `model.embed`，支持批量输入和本地/远程 Provider |
| 文本生成 | 已接线 | Model Provider 流式生成、取消、限流和审计 |
| Agent 工作流 | 已接线 | checkpoint、恢复、预算、审批、并行工具与调度 |
| MCP | 已接线 | tools/resources/prompts、OAuth、权限与审计 |
| 应用持久目录 | 已接线 | 每应用 `data/cache/logs/runtime` 目录 |
| 小型状态存储 | 已接线 | `storage.*` 原子 JSON 持久化 |
| SQLite 应用示例 | 已验证 | `examples/notes` 使用 `better-sqlite3`/`node:sqlite` |
| Secret | 已接线 | 数据库凭据可只保存 opaque account 引用 |
| Service 生命周期 | 已接线 | health、restart、日志、资源限制和依赖编排 |

### 2.2 尚未实现

- 文档导入、格式解析、清洗和标准化；
- Chunk 切分、稳定标识和增量更新；
- 向量索引、关键词索引与 Metadata Filter；
- Hybrid Search、重排、上下文组装和引用溯源；
- 知识库 CRUD、权限、配额、备份和管理 UI；
- 平台级数据库连接、迁移、查询或事务 API；
- 索引任务调度、失败重试、断点恢复和进度事件；
- RAG 质量评估、召回率、延迟和答案忠实度测试。

`storage.*` 不应被扩展成通用数据库：它适合设置和小型状态，不适合大规模文档、向量、复杂查询或
长事务。

## 3. 产品边界

### 3.1 Runtime 核心负责什么

Runtime 核心继续只提供通用治理能力：

- 应用、Agent 和调用者身份；
- Manifest 权限上限与用户授权；
- Model、MCP、Secret 和受管 service 生命周期；
- CPU、内存、进程、磁盘和网络边界；
- 任务、流、取消、日志、指标和审计；
- 安装、升级、回滚和数据目录迁移。

### 3.2 Knowledge Service 负责什么

Knowledge Service 是官方维护但可替换的服务，负责：

- 数据源连接与内容读取；
- 文档解析、规范化、切分和去重；
- 调用 `model.embed` 并写入索引；
- dense、sparse、hybrid 检索和可选重排；
- 上下文预算、引用和结果解释；
- 知识库权限、版本、任务和索引状态；
- 向 Agent 暴露声明式只读检索工具。

### 3.3 明确不做

- 不自行实现 SQL 引擎、LLM、Embedding 模型或 GPU Runtime；
- 不要求所有 Alex 应用使用统一数据库；
- 不允许应用绕过权限读取其他应用的知识库；
- 不把原始文档或数据库密码写入 Manifest、日志或审计参数；
- 不宣称向量相似度结果等同于正确答案；
- 第一版不提供分布式一致性、跨区域复制和 SaaS 级租户计费。

## 4. 总体架构

```text
WebView / App Backend / Agent
              │
              │ knowledge.* / MCP tool
              ▼
       Alex API Router + PermissionManager
              │
              ▼
       Knowledge Service Supervisor
              │
     ┌────────┼───────────┬──────────────┐
     ▼        ▼           ▼              ▼
 Ingestion  Retrieval   Task Engine    Audit/Metrics
 Pipeline   Pipeline
     │        │
     │        ├──────────────► optional Reranker
     ▼        │
 model.embed  │
     │        │
     └────┬───┘
          ▼
   Storage Abstraction
     ├─ MetadataStore: SQLite
     ├─ BlobStore: app data directory
     ├─ VectorIndex: local SQLite vector extension
     └─ External adapter: pgvector/Qdrant/Milvus/MCP
```

Knowledge Service 必须作为独立进程运行，不应链接进入 Shell 或 daemon 主进程。解析器、向量扩展或
第三方数据库驱动崩溃时，不得拖垮 Runtime 控制面。

## 5. 核心领域模型

### 5.1 Knowledge Base

```rust
struct KnowledgeBase {
    id: String,
    app_id: String,
    name: String,
    description: Option<String>,
    embedding_profile: EmbeddingProfile,
    retrieval_profile: RetrievalProfile,
    storage_profile: StorageProfile,
    state: KnowledgeBaseState,
    schema_version: u32,
    created_at_ms: u64,
    updated_at_ms: u64,
}
```

`id` 在应用作用域内唯一。默认禁止跨应用访问；未来共享知识库需要显式 owner、ACL 和用户授权，不能
通过猜测 ID 访问。

### 5.2 Source、Document 与 Chunk

```rust
struct Source {
    id: String,
    knowledge_base_id: String,
    kind: SourceKind,
    locator: String,
    sync_policy: SyncPolicy,
    content_hash: Option<String>,
    last_synced_at_ms: Option<u64>,
}

struct Document {
    id: String,
    source_id: String,
    canonical_uri: String,
    title: Option<String>,
    mime_type: String,
    content_hash: String,
    metadata: JsonValue,
    version: u64,
}

struct Chunk {
    id: String,
    document_id: String,
    ordinal: u32,
    text: String,
    token_count: u32,
    start_offset: Option<u64>,
    end_offset: Option<u64>,
    heading_path: Vec<String>,
    content_hash: String,
    embedding_version: String,
}
```

Chunk ID 应由文档稳定 ID、切分配置版本和内容哈希派生。相同内容重新导入时不重复计算向量；切分器或
Embedding 模型变化时创建新索引 generation，并在完成后原子切换。

### 5.3 Citation 与检索结果

```rust
struct RetrievalHit {
    chunk_id: String,
    document_id: String,
    score: f32,
    dense_score: Option<f32>,
    lexical_score: Option<f32>,
    rerank_score: Option<f32>,
    text: String,
    citation: Citation,
    metadata: JsonValue,
}

struct Citation {
    source_id: String,
    canonical_uri: String,
    title: Option<String>,
    start_offset: Option<u64>,
    end_offset: Option<u64>,
    page: Option<u32>,
    content_hash: String,
}
```

引用必须指向实际参与生成的文本块，并携带内容哈希，避免文档更新后旧引用静默指向不同内容。

## 6. 数据库设计

### 6.1 本地默认方案

第一版建议使用 SQLite，原因是单文件部署、事务成熟、备份简单、适合桌面端并且已经在项目示例中验证。
数据库放在：

```text
%LOCALAPPDATA%/AlexOS/apps/<app-id>/data/knowledge/<kb-id>/
  metadata.sqlite3
  blobs/
  indexes/
  staging/
  backups/
```

建议启用 WAL、foreign keys、busy timeout 和一致的同步级别。数据库连接只由 Knowledge Service 持有；
WebView 不直接获得文件路径或 SQL 执行能力。

### 6.2 建议表结构

```sql
CREATE TABLE knowledge_bases (
  id TEXT PRIMARY KEY,
  app_id TEXT NOT NULL,
  name TEXT NOT NULL,
  config_json TEXT NOT NULL,
  active_generation INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE sources (
  id TEXT PRIMARY KEY,
  knowledge_base_id TEXT NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  locator TEXT NOT NULL,
  sync_policy_json TEXT NOT NULL,
  content_hash TEXT,
  last_synced_at_ms INTEGER
);

CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  canonical_uri TEXT NOT NULL,
  title TEXT,
  mime_type TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  metadata_json TEXT NOT NULL,
  version INTEGER NOT NULL
);

CREATE TABLE chunks (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  text TEXT NOT NULL,
  token_count INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  embedding_version TEXT NOT NULL,
  citation_json TEXT NOT NULL,
  UNIQUE(document_id, generation, ordinal)
);

CREATE TABLE ingestion_tasks (
  id TEXT PRIMARY KEY,
  knowledge_base_id TEXT NOT NULL,
  state TEXT NOT NULL,
  phase TEXT NOT NULL,
  progress_current INTEGER NOT NULL,
  progress_total INTEGER,
  checkpoint_json TEXT,
  error_json TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
```

全文检索可使用 SQLite FTS；向量表的具体 DDL 由 `VectorIndex` 实现拥有，不进入稳定公共 schema。

### 6.3 Schema Migration

- 每次 schema 变更必须有单向迁移和兼容测试；
- 迁移前创建可恢复备份，并校验可用磁盘空间；
- 大型索引变更使用新 generation 构建，完成后原子切换；
- Runtime/应用降级不得直接用旧二进制打开不兼容 schema；
- 失败时保留旧数据库与结构化错误，不做破坏性“自动重建”；
- 文本、元数据和向量索引分别版本化，索引可重建但原始数据不可静默丢失。

### 6.4 外部数据库

外部数据库采用适配器，不把供应商细节放入 `knowledge.*` API：

```rust
trait MetadataStore { /* transaction and document metadata */ }
trait BlobStore { /* original and normalized content */ }
trait VectorIndex { /* upsert, delete, search, compact */ }
trait LexicalIndex { /* upsert, delete, search */ }
```

连接配置只保存非敏感字段，密码通过 `secretAccount` 引用 Secret Store。网络访问必须满足 Manifest origin
白名单；证书校验、超时、连接池、重试和最大响应尺寸均由适配器强制。

第一阶段不提供任意 SQL API。若未来增加 `database.*`，也应使用预声明查询或 migration，而不是允许
WebView 传入任意 SQL。

## 7. 文档摄取管线

### 7.1 支持的数据源

建议分阶段支持：

1. 用户通过文件选择器授权的本地文件和目录；
2. 应用包内或应用数据目录中的文件；
3. HTTPS URL；
4. MCP Resources；
5. 外部数据库的受控查询适配器；
6. 云盘、Wiki、Git 仓库等插件数据源。

第一版格式建议限定为 UTF-8 文本、Markdown、HTML、JSON 和 PDF。Office 文档、图片 OCR、音视频转写
作为可选解析 Worker，避免扩大核心信任面。

### 7.2 处理步骤

```text
authorize → discover → read → malware/type check → parse → normalize
→ deduplicate → chunk → embed → index → verify → activate generation
```

每一步必须可观测、可取消和有大小上限。任务状态至少包括：

```text
queued / discovering / parsing / chunking / embedding / indexing /
verifying / completed / failed / cancelled
```

### 7.3 切分策略

内建策略至少包括：

- `fixedTokens`：固定 token 数并允许重叠；
- `markdown`：按标题、段落和代码块切分；
- `html`：移除脚本样式后按语义元素切分；
- `json`：按对象路径和大小限制切分；
- `pageAware`：保留 PDF 页码与块位置。

配置必须记录在索引 generation 中，包括 tokenizer、最大 token、重叠、语言和规范化版本。修改配置触发
新 generation，而不是原地混用不同语义的向量。

### 7.4 增量更新

- 使用 canonical URI、文件标识、mtime、大小和 SHA-256 判断变化；
- 内容未变时跳过解析和 embedding；
- 只替换变化文档对应的 chunk；
- 删除源文件时默认标记 tombstone，经过同步确认后再删除索引；
- 文件监听事件只作为提示，最终状态必须重新扫描确认；
- 失败任务从持久 checkpoint 恢复，不重复提交已经确认成功的 embedding 批次。

## 8. 检索与上下文组装

### 8.1 检索流程

```text
query validation
  → query rewrite（可选）
  → model.embed
  → dense search + lexical search
  → metadata/ACL filter
  → score fusion
  → rerank（可选）
  → diversity/dedup
  → context budget packing
  → hits + citations
```

默认应先过滤 ACL 再返回文本。若底层数据库不能在检索阶段执行权限过滤，服务必须过取候选并在返回前
过滤，而且不得在日志、缓存或模型请求中泄露无权内容。

### 8.2 Hybrid Search

第一版可采用可解释的加权归一化或 Reciprocal Rank Fusion。融合配置与结果中应暴露 dense、lexical、
rerank 分数，便于调试；公共 API 不承诺某一向量距离算法的原始分值在不同 Provider 间可比较。

### 8.3 Reranker

Reranker 是可选 Model Provider 能力。必须有候选数量、输入 token、延迟和费用预算；失败时允许按配置
回退至融合排序，同时在响应中标记 `rerankApplied: false`，不能静默伪装为已重排。

### 8.4 上下文组装

- 按模型 context window 和 Agent 剩余预算计算最大上下文；
- 相邻 chunk 可合并，但必须保持引用边界；
- 不让单一文档占满全部预算；
- 文档内容始终按不可信数据处理，不与 system/developer 指令混合；
- 使用结构化边界传入模型，保留 source/chunk ID；
- 最终回答引用只能来自实际发送给模型的 chunk。

## 9. 建议 API

以下 API 是目标设计，并未实现。

### 9.1 权限

| 权限 | 含义 |
| --- | --- |
| `knowledge.read` | 查询知识库和读取检索结果 |
| `knowledge.write` | 创建知识库、导入、同步、删除和重建索引 |
| `knowledge.admin` | 修改共享、外部存储、迁移和备份策略 |
| `model.use` | 调用配置的 embedding/rerank/generation 模型 |
| `mcp.use` | 使用声明的数据源或外部知识库 MCP binding |

### 9.2 Knowledge Base API

```text
knowledge.create
knowledge.get
knowledge.list
knowledge.update
knowledge.delete
knowledge.stats
```

创建示例：

```json
{
  "name": "project-docs",
  "embedding": {
    "model": "remote/text-embedding-3-small",
    "dimensions": 1536,
    "batchSize": 64
  },
  "retrieval": {
    "mode": "hybrid",
    "topK": 12,
    "candidateK": 60
  },
  "storage": { "kind": "local" }
}
```

### 9.3 Source 与摄取 API

```text
knowledge.source.add
knowledge.source.list
knowledge.source.remove
knowledge.ingest
knowledge.sync
knowledge.reindex
knowledge.task.status
knowledge.task.cancel
knowledge.task.events
```

导入调用返回 task ID，不阻塞 IPC：

```json
{
  "knowledgeBaseId": "kb_project_docs",
  "sources": [
    { "kind": "fileToken", "token": "opaque-file-token" }
  ],
  "chunking": {
    "strategy": "markdown",
    "maxTokens": 700,
    "overlapTokens": 80
  }
}
```

### 9.4 检索 API

```text
knowledge.search
knowledge.retrieveContext
knowledge.explain
```

```json
{
  "knowledgeBaseId": "kb_project_docs",
  "query": "权限撤销如何传播到运行中的 Agent？",
  "topK": 8,
  "filter": {
    "and": [
      { "field": "language", "eq": "zh-CN" },
      { "field": "version", "gte": 2 }
    ]
  },
  "hybrid": { "enabled": true },
  "rerank": { "enabled": true, "model": "remote/reranker" },
  "include": ["text", "citation", "scores"]
}
```

过滤器必须是受限 AST，限制字段、节点数、深度和操作符；禁止透传 SQL 或供应商查询语法。

### 9.5 Agent 工具

Agent 默认只获得只读工具：

```text
alex.knowledge.search
alex.knowledge.getDocument
```

摄取、删除、重建索引属于有副作用工具，必须在 Agent spec 中显式声明并默认要求审批。每次工具调用受
Agent token、费用、工具次数和墙钟预算约束。

## 10. Manifest 建议

```yaml
schemaVersion: 2
id: com.example.research

permissions:
  - knowledge.read
  - knowledge.write
  - model.use

knowledgeBases:
  - name: project-docs
    storage:
      kind: local
      quotaMb: 2048
    embedding:
      model: remote/text-embedding-3-small
      dimensions: 1536
    retrieval:
      mode: hybrid
      topK: 8
    sources:
      - kind: appData
        path: documents/
```

Manifest 只声明能力上限和默认配置。用户导入的文件、Secret、绝对路径、OAuth token 和动态外部连接不
写入发布包。

## 11. 安全与隐私

### 11.1 内容不可信

所有文档和检索结果都可能包含 prompt injection。Knowledge Service 必须：

- 将内容标记为 data，而不是指令；
- 不执行文档中的命令、URL 或工具调用；
- 在 Agent 工具返回中保留来源和信任标签；
- 对 HTML 移除脚本、事件属性、远程资源和危险 URL；
- 对解析器设置进程隔离、超时、内存和展开大小限制；
- 对模型可见内容执行敏感字段策略，但保留原文与脱敏版本的明确边界。

### 11.2 数据访问

- 每次操作绑定 `app_id`、用户和知识库 ID；
- 文件导入使用短期 File Token，不因一次选择永久扩大任意目录权限；
- 外部数据源使用 Secret Store 引用；
- 知识库共享默认关闭；
- 删除必须同时覆盖 metadata、全文和向量索引，并产生可验证审计事件；
- 日志不记录原始文档、完整 query、向量或数据库密码；
- 遥测默认只包含聚合大小、耗时、错误类型和计数。

### 11.3 数据库安全

- SQLite 文件和 Blob 目录使用应用作用域 ACL；
- 外部连接强制 TLS，证书错误不可静默降级；
- 禁止 URL 内嵌账号密码；
- 参数化查询，禁止字符串拼接 SQL；
- 备份默认本地加密或由用户明确选择目的地；
- 数据库驱动和向量扩展作为受限 Worker，不进入 daemon/Shell 主进程；
- 磁盘配额必须覆盖数据库、WAL、临时索引、原始文件和备份，而不只统计主数据库文件。

### 11.4 审计

建议记录：调用者、知识库、操作、参数摘要哈希、数据量、模型、token/费用、耗时、结果数量、任务终态
和错误域。审计不得记录原文；管理员可选择单独启用受保护的调试采样。

## 12. 可靠性与生命周期

### 12.1 原子 generation

摄取和重建在 staging generation 中执行：

1. 保存任务与目标 generation；
2. 解析并写入 staging；
3. 校验文档数、chunk 数、向量维度和索引可读性；
4. 在短事务中切换 `active_generation`；
5. 延迟清理旧 generation。

查询始终读取一个确定 generation，不能在一次请求中混合新旧索引。

### 12.2 崩溃恢复

- 每个阶段保存 checkpoint 和幂等键；
- 已完成的 embedding 批次不重复计费；
- daemon/Knowledge Service 重启后恢复 queued/running 任务；
- 远程数据库写入必须明确幂等语义；
- 取消只保证 Alex 不再继续工作，不承诺撤销外部系统已经完成的副作用；
- 连续崩溃达到阈值后熔断，并保留旧 active generation 可查询。

### 12.3 备份与恢复

- SQLite 使用在线备份 API 或一致性快照，不直接复制正在写入的数据库文件；
- 备份清单包含 schema、索引 generation、文件哈希和模型配置；
- 恢复先进入 staging，完整校验后替换；
- 外部数据库只保存连接与逻辑配置，备份责任必须在 UI 和文档中明确；
- 删除/恢复操作需要权限、确认和审计。

## 13. 配额与性能

每知识库至少配置：

- 最大原始字节数；
- 最大文档数和 chunk 数；
- 最大数据库/索引/临时空间；
- 单文档大小、解析时间和展开比例；
- embedding 批次、并发、token 和费用；
- 查询并发、候选数、topK 和超时；
- rerank 候选数、token 和费用；
- 缓存大小和保留时间。

建议首版默认值保守并可由宿主策略进一步收紧。达到配额时返回稳定错误，不允许因磁盘满破坏现有索引。

性能目标应按设备分层，而不是承诺单一数字。基准至少覆盖：1 万、10 万、100 万 chunk；冷启动和热查询；
仅关键词、仅向量和 hybrid；CPU 与 GPU embedding；增量导入和全量重建。

## 14. 可观测性与错误域

### 14.1 指标

- 文档、chunk、原始字节和索引字节数；
- ingestion 队列深度、各阶段耗时和失败率；
- embedding QPS、token、费用、重试和 cache hit；
- 查询 p50/p95/p99、候选数和 topK；
- rerank 延迟和回退率；
- SQLite busy、WAL 大小、外部连接池和错误；
- generation 切换、恢复和清理结果。

### 14.2 稳定错误

建议新增错误域：

```text
KNOWLEDGE_NOT_FOUND
KNOWLEDGE_PERMISSION_DENIED
KNOWLEDGE_QUOTA_EXCEEDED
KNOWLEDGE_SOURCE_UNAVAILABLE
KNOWLEDGE_PARSE_FAILED
KNOWLEDGE_EMBED_FAILED
KNOWLEDGE_INDEX_FAILED
KNOWLEDGE_DIMENSION_MISMATCH
KNOWLEDGE_MIGRATION_REQUIRED
KNOWLEDGE_QUERY_INVALID
KNOWLEDGE_TASK_CANCELLED
DATABASE_CONNECTION_FAILED
DATABASE_MIGRATION_FAILED
DATABASE_CORRUPT
```

程序不得依赖 Rust/Node 的原始错误文本判断错误类型。

## 15. 测试与评估

### 15.1 正确性测试

- parser、normalizer、chunker 的 golden tests；
- Unicode、多语言、代码块、表格、超长行和损坏文件；
- 内容哈希去重与增量更新；
- embedding 维度不匹配和模型切换；
- generation 原子切换及失败回滚；
- ACL/metadata filter 在检索前后均不可泄漏；
- 删除、备份、恢复和 schema migration；
- daemon、Worker、数据库和网络中断恢复。

### 15.2 安全测试

- 路径穿越、符号链接、压缩炸弹和解析器漏洞 fixture；
- 恶意 HTML、PDF、JSON 和 MCP Resource；
- prompt injection 与工具注入；
- SQL/过滤器注入；
- 跨应用知识库 ID 猜测；
- Secret、原文和向量的日志泄漏；
- 磁盘满、WAL 膨胀、超大索引与拒绝服务。

### 15.3 RAG 质量评估

建立版本化 eval dataset，至少记录：

- Recall@K、MRR、nDCG；
- 引用命中率和引用完整性；
- 无答案问题的拒答率；
- 答案忠实度和上下文相关性；
- 不同 chunk、embedding、hybrid 权重和 reranker 的对比；
- 每次查询的延迟、token 与费用。

质量 eval 不能只使用 LLM-as-judge；关键数据集应有人工标注，并固定 judge 模型和 prompt 版本。

## 16. 管理 UI

App Manager 建议新增“知识库”页面：

- 知识库列表、状态、大小、文档/chunk 数和 active generation；
- 数据源、最近同步、失败原因和下次计划；
- 摄取任务进度、暂停、取消和重试；
- Embedding、索引、检索和 reranker 配置；
- 搜索 Playground，展示文本、引用及各阶段分数；
- 权限、共享、外部连接和 Secret 引用；
- 重建索引、备份、恢复和删除；
- 费用、token、延迟、错误和审计摘要。

危险操作必须显示影响范围，不应只提供一个无上下文的确认框。

## 17. 实施阶段

### K0：契约与边界

- 固化 `knowledge.*` IDL、权限和错误域；
- 建立 `KnowledgeStore`/`VectorIndex` traits；
- 确定数据目录、schema、配额和审计格式；
- 为未实现能力在 `system.capabilities` 中保持诚实报告。

验收：API schema 可生成 Rust/TypeScript 类型，所有未授权调用 fail closed。

### K1：托管 SQLite 与文本检索

- SQLite metadata store、migration、WAL、备份；
- 文本/Markdown/HTML 摄取；
- 稳定 chunk 和 FTS 检索；
- task、取消、进度、恢复和 App Manager 基础 UI。

验收：重启和摄取失败不破坏旧索引，关键词检索返回稳定引用。

### K2：本地向量检索

- 接入 `model.embed`；
- 本地 `VectorIndex`；
- embedding cache、批处理和维度校验；
- generation 构建与原子切换；
- dense search 和搜索 Playground。

验收：10 万 chunk 数据集可增量更新、取消、恢复并通过 Recall@K 基线。

### K3：生产 RAG

- Hybrid Search、Metadata Filter、reranker；
- PDF 与页码引用；
- 上下文预算与 Agent 只读工具；
- prompt injection 防护和 RAG eval；
- 磁盘配额硬限制和长期稳定性测试。

验收：恶意文档不能获得工具权限，回答引用可追溯到实际发送给模型的 chunk。

### K4：外部数据库与连接器

- PostgreSQL/pgvector 或 Qdrant 首个官方适配器；
- MCP Resources、云盘、Wiki 和 Git 数据源；
- Secret、TLS、连接池和企业策略；
- 外部系统故障恢复和兼容矩阵。

验收：切换存储后公共 API 和 Agent 工具语义不变，断网不损坏本地任务状态。

### K5：共享与生态

- 用户/团队知识库 ACL；
- 导入导出与可签名 Knowledge Package；
- 连接器/解析器插件 SDK；
- Registry 分发、撤销和企业 allowlist。

验收：共享、更新、撤销和卸载均不可绕过 PermissionManager，也不会遗留 Secret。

## 18. 推荐首个垂直切片

首个可交付切片建议严格限制为：

1. 单应用、单用户、单本地知识库；
2. Markdown/TXT 文件选择导入；
3. SQLite 元数据与 FTS；
4. 调用现有 `model.embed` 写入本地向量索引；
5. `knowledge.create/ingest/task.status/search/delete`；
6. 文件 token、`knowledge.read/write` 权限与审计；
7. 一个 App Manager 页面和一个端到端示例；
8. 10 万 chunk、重启恢复、磁盘满和 prompt injection 测试。

这一切片先证明生命周期、权限、索引原子性和引用正确性，再扩展 PDF、reranker、外部数据库和共享。

## 19. 关键决策记录

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| RAG 所属层 | 官方应用层 Service | 保持 Runtime 核心通用且可替换 |
| 本地 metadata | SQLite | 单文件、事务成熟、桌面部署简单 |
| 本地向量索引 | 可替换 SQLite 扩展优先 | 开箱即用，同时避免锁死具体扩展 |
| Embedding | 复用 `model.embed` | 统一模型、Secret、限流、费用和审计 |
| 外部数据库 | Adapter/MCP | 隔离供应商差异和驱动风险 |
| 公共查询 | 结构化 API/Filter AST | 防止 SQL 注入和供应商耦合 |
| 重建策略 | generation + 原子切换 | 查询不中断且失败可回滚 |
| 文档信任 | 始终不可信 | RAG 内容可能携带 prompt/tool injection |
| 默认共享 | 禁止 | 避免跨应用和跨用户数据泄漏 |

## 20. 完成定义

RAG/数据库能力只有同时满足以下条件才可以标记为生产可用：

- 本地默认方案无需用户安装独立数据库即可工作；
- 所有访问都有应用身份、权限决定、配额和审计；
- 摄取、重建、迁移、备份和恢复均可中断并安全恢复；
- 模型或切分配置变化不会混用不兼容向量；
- 检索结果有稳定引用，生成回答可追溯到实际上下文；
- 外部数据库故障不会拖垮 daemon，也不会泄露 Secret；
- prompt injection、跨应用读取、SQL 注入和磁盘耗尽有自动化安全测试；
- 真实数据规模下有质量、延迟、资源和费用基线；
- SDK、API Reference、示例、迁移说明和 App Manager UI 同步完成。

