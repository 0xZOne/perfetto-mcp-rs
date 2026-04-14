# ROADMAP

Last updated: 2026-04-13

面向 `perfetto-mcp-rs` 的下一阶段执行清单。目标不是继续堆功能，而是先补齐正确性边界、测试防回归能力，以及高价值分析工具。

> 这是一个意图快照，不是进度看板。逐条任务的跟踪建议使用 GitHub issues，避免本文件里的 `- [ ]` 状态和外部 tracker 各自漂移。

## Principles

- 先修正确性和运行时稳定性，再扩功能
- 先补测试缝隙，再做重构和表面优化
- 优先投资“未来会反复复用”的基础能力
- 区分 correctness bug、runtime hardening 和 feature expansion

## Priority Order

当前建议的优先级顺序：

1. 修 `wait_ready` 的实例身份校验问题
2. 持续消费并记录子进程 `stderr`
3. 为 `tp_manager` 补关键回归测试
4. 降低 server 层对字符串匹配的依赖
5. 强化下载链路
6. 再扩高价值领域工具和 fixture

## Milestone 1: Correctness And Runtime Hardening

目标：先消除最可能导致“结果不可信”或“长时间运行不稳”的问题。

- [x] 修复 `wait_ready` 的实例身份校验
  - 现状：`/status` 可能误判到外部进程
  - 建议实现：基于 `stderr` 启动标记做 readiness gate，再进入 `/status` 轮询
  - 验收：端口预占用场景下不会把外部进程识别为自己的实例

- [x] 持续消费并记录子进程 `stderr`
  - 现状：`stderr` 已 `piped()`，但没有后台 drain
  - 目标：避免 pipe 堵塞，同时提升启动失败可诊断性
  - 验收：启动失败日志可见，长时间运行无 pipe 写满风险

- [x] 为启动超时和查询超时增加可配置项
  - 现状：超时策略写死
  - 目标：支持 CLI flag 或 env
  - 验收：能显式配置启动超时和 HTTP 查询超时

- [x] 评估是否对同一路径并发 spawn 做串行化
  - 现状：同一 trace 并发 `get_client` 会重复 spawn，再丢掉一个实例
  - 目标：避免无意义重复启动
  - 验收：并发同一路径请求时，最多只启动一个实例

## Milestone 2: Test Hardening

目标：把“现在能跑”提升为“后续不容易回归”。

- [x] 为自定义 `starting_port` 回绕补纯单元测试
  - 目标：验证回绕后回到 `starting_port`
  - 建议：提取纯端口分配逻辑，便于边界测试
  - 验收：覆盖 `u16::MAX -> starting_port`

- [x] 为 LRU 淘汰与实例复用补测试
  - 验收：超过容量后只淘汰最旧未使用实例

- [x] 为子进程异常退出后的自动恢复补测试
  - 验收：旧实例退出后，下一次查询可恢复

- [x] 增加同一 trace / 不同 trace 的并发访问测试
  - 验收：不死锁、不误复用、不重复 spawn

- [ ] 增加失败路径测试
  - 场景：trace 不存在、binary 不可执行、下载失败、端口冲突
  - 状态：trace 不存在已覆盖；binary / 下载 / 端口冲突场景仍待补
  - 验收：错误消息清晰且可定位

- [x] 为 server 层提示逻辑补回归测试
  - 现状：依赖 `msg.contains(...)`
  - 目标：锁定当前“missing table / missing module”提示行为
  - 验收：错误文案或分类变化时测试会报警

- [ ] 扩充 e2e fixture
  - 现状：单个 smoke fixture 只能证明主链路成立
  - 目标：增加至少一个小型 Chrome fixture 和一个 Android fixture
  - 验收：领域工具有代表性 e2e 覆盖

## Milestone 3: Error Model Tightening

目标：减少脆弱的字符串匹配，提高提示逻辑稳定性和可测试性。

- [ ] 设计 `QueryErrorKind`
  - 候选：`MissingTable`、`MissingModule`、`SyntaxError`、`Other`
  - 验收：形成清晰枚举，不破坏原始错误显示

- [ ] 在更靠近 client/decode 的层面做错误分类
  - 目标：server 层不直接依赖字符串匹配
  - 验收：提示逻辑改为 match 枚举

- [ ] 清理或收敛未使用错误语义
  - 目标：处理 `PerfettoError::NoTraceLoaded` 这类当前未落地的变体
  - 验收：错误枚举和实际语义一致

## Milestone 4: Download And Distribution Hardening

目标：让安装、升级和缓存恢复更稳。

- [ ] 将下载逻辑改为“临时文件 + 原子重命名”
  - 验收：下载中断不会污染缓存

- [ ] 增加 checksum 或等效校验
  - 验收：损坏 binary 可被识别并重新下载

- [ ] 增加下载源 / 镜像配置能力
  - 验收：网络受限环境可切换源

- [ ] 增强跨平台 CI 覆盖
  - 至少覆盖：Linux、macOS、Windows
  - 验收：构建、测试、release 资产一致可用

## Milestone 5: Productized Analysis Tools

目标：从“通用 SQL 执行器”升级为“常见 Perfetto 场景分析工具”。

- [ ] 新增 `cpu_hot_threads`
  - 输出高 CPU 占用线程及关联进程
  - 验收：适用于 Android/Linux 常见 trace

- [ ] 新增 `process_cpu_breakdown`
  - 输出进程级 CPU 时间分布
  - 验收：能快速定位最重进程

- [ ] 新增 `memory_growth_summary`
  - 输出内存增长显著的进程或计数器
  - 验收：可用于粗筛内存异常对象

- [ ] 新增 `android_startup_summary`
  - 聚焦应用启动关键阶段
  - 验收：能给出启动耗时和主要阶段拆解

- [ ] 新增 `chrome_frame_timeline_summary`
  - 聚焦 frame timeline / jank 场景
  - 验收：与现有 `chrome_scroll_jank_summary` 形成互补

- [ ] 新增 `anr_suspects`
  - 聚焦主线程卡顿、binder、锁等待等常见线索
  - 验收：可给出初步怀疑对象，而不是只返回原始表

- [ ] 新增 `list_stdlib_modules`
  - 目标：降低 agent 对预先知道模块名的依赖
  - 验收：可列出可发现的 stdlib 模块或相关元数据

## Milestone 6: Performance And Context Efficiency

目标：提升大 trace 和复杂 agent 工作流下的使用体验。

- [ ] 为 `execute_sql` 增加 limit / summary 模式
  - 目标：减少上下文消耗
  - 验收：可返回列信息、前 N 行、总行数或摘要

- [ ] 评估分页或流式结果输出
  - 现状：结果先完整解码为 `Vec<Value>`
  - 验收：形成明确方案，决定保留硬上限还是升级到分页/流式

- [ ] 为高频 schema 查询加缓存
  - 场景：`list_tables`、`table_structure`
  - 验收：重复查询减少不必要 RPC

- [ ] 支持 query cancellation
  - 场景：LLM 发起低质量长查询
  - 验收：长查询可中断，不必等待超时

- [ ] 增加 tracing spans
  - 目标：让慢查询和高频调用更易诊断
  - 验收：可观测 `sql_len`、耗时、row_count 等关键信息

## Suggested Release Plan

## v0.2

聚焦稳定性和正确性。

- [x] 完成 `Milestone 1`
- [x] 完成 `Milestone 2` 中最关键的回归测试
- [ ] 至少完成 `Milestone 3` 的测试补强或初步分类方案
- [ ] 完成下载原子化

发布门槛：

- [ ] 启动、查询、回收主链路无已知高优先级 correctness bug
- [x] 单元测试稳定
- [x] e2e 在 CI 环境稳定
- [x] 关键错误提示有测试覆盖

## v0.3

聚焦高价值分析能力和工具自发现性。

- [ ] 完成 `Milestone 5` 中至少 3 个领域工具
- [ ] 增加 `list_stdlib_modules`
- [ ] 为新增工具补 fixture 和 e2e
- [ ] 对工具描述和返回结构做一轮统一

发布门槛：

- [ ] 至少覆盖 CPU、内存、Chrome/Android 中的 2 到 3 个高频场景
- [ ] README 补齐新增工具使用示例

## v1.0

聚焦“稳定可发布”。

- [ ] 完成关键运行时硬化
- [ ] 完成关键回归测试矩阵
- [ ] 完成跨平台分发与诊断文档
- [ ] 明确版本升级与兼容性策略

发布门槛：

- [ ] 核心行为有防回归测试
- [ ] 安装与升级路径清晰
- [ ] 常见故障可诊断
- [ ] 面向典型 Perfetto 使用场景具备足够覆盖

## Reference

- 英文版本：`docs/roadmap.md`
