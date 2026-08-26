---
layout: default
title: 文档维护
nav_order: 99
---

# 文档维护

## 事实来源

- 当前能力：`docs/status.md`；
- 未完成工作：`docs/roadmap.md`；
- CLI：`cargo run --offline -- --help`；
- Desktop API：`packages/sdk/desktop-api.schema.json` 及生成 Reference；
- Manifest：Rust serde 类型与 `MANIFEST_REFERENCE.md`；
- 目标体验：标记为设计文档，不得写成当前教程。

不要手写源码文件数或测试通过数量。它们会随提交变化；文档应给出验证命令。

## CI 检查

```powershell
node scripts/check-docs.mjs
node packages/sdk/generate-schema.mjs --check
```

文档检查验证本地 Markdown 链接、已知过期术语和生成文件漂移。代码块中的 Manifest 示例应尽量被
Rust 测试或真实 example 覆盖。

## 更新规则

功能变更同时更新：事实状态、相关专题文档、示例 README 和 Roadmap。设计目标落地后，将其从
“计划”移动到状态页，不要简单复制一份“已完成”描述。文档中的日期只用于说明历史快照，不能替代
当前验证命令或 commit。
