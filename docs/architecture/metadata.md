# 元数据设计（WekaFS 风格 × DASE 持久化）

OmniStor 的元数据设计**完全参考 WekaFS**：没有独立的"元数据服务器"层，元数据被切分为大量 **Bucket**，全量分布到集群**所有 TLC NVMe SSD** 上；同时结合 VastData DASE，把 Bucket 的持久化状态放在共享 NVMe 池中，使 Bucket 的接管（failover）无需任何数据迁移。

## 设计目标

| 目标 | 说明 |
| --- | --- |
| 全盘参与 | 元数据分布到集群内**所有** TLC NVMe SSD，不存在专用元数据盘 |
| 无专用元数据节点 | 元数据处理进程（Bucket）运行在任意计算节点上，硬件不绑定角色 |
| 容量弹性 | 元数据容量**跟随使用量增长**，不做固定预留（区别于传统按比例预留的设计） |
| Metadata QoS | 元数据操作（lookup/create/stat/list…）按租户/文件系统/桶限速 |
| 线性扩展 | 对象数、元数据 IOPS 随节点/SSD 数量近线性增长，支撑万亿对象 |

## Bucket 模型（参考 WekaFS）

### 什么是 Bucket

- **Bucket 是元数据的分片单元与处理单元**：每个 Bucket 是一个独立的轻量进程（或协程组），拥有命名空间的一个哈希分片——包括其中的 inode、目录项、extent 映射、对象索引。
- 集群启动时即创建远多于节点数的 Bucket（如数千到数万个），Bucket 数量决定元数据并行度上限。
- 目录、inode、对象 key 通过**一致性哈希**映射到 Bucket；超大目录按目录内前缀二次切分，跨多个 Bucket 承载，避免热点目录成为单 Bucket 瓶颈。

```
                 hash(inode / dirent / object-key)
                              │
        ┌─────────────┬───────┴──────┬─────────────┐
        ▼             ▼              ▼              ▼
   Bucket 0007   Bucket 0142    Bucket 1893    Bucket 4096 …
   (B-tree)      (B-tree)       (B-tree)       (B-tree)
        │             │              │              │
        └──────┬──────┴──────┬───────┴───────┬──────┘
               ▼             ▼               ▼
        ── 共享 TLC NVMe 池（DASE，全部 SSD 参与）──
```

### Bucket 内部结构

- 每个 Bucket 维护自己的 **B-tree**（键：inode 号 / 目录项 / 对象 key；值：属性、extent 指针、版本），B-tree 节点以 4 KB 块持久化在共享 TLC NVMe 池上。
- 写入先追加到 Bucket 私有的**日志段（journal）**，再批量合并进 B-tree——日志与 B-tree 都在共享池上，随使用量按需分配块。
- Bucket 之间无共享状态，跨 Bucket 操作（如 rename 跨目录）用两阶段提交，参与方仅限相关的 2–3 个 Bucket。

## 与 DASE 的融合：这是与 WekaFS 的关键差异

WekaFS 的 Bucket 状态落在本节点 SSD 上（shared-nothing），failover 需依赖纠删重建；OmniStor 把 Bucket 状态放在 **DASE 共享 NVMe 池**：

- Bucket 进程运行在无状态计算节点（CNode）上，通过 NVMe-oF 读写共享池中属于自己的元数据块。
- **任一计算节点都能看到全部元数据块** → Bucket 崩溃或节点下线时，集群协调器把该 Bucket 直接调度到其他节点**原地接管**：重放日志段即可上线，**零数据迁移、秒级恢复**。
- 元数据块本身的冗余由数据层统一的纠删/副本机制保证（跨 ebox 故障域），Bucket 层不再需要 RAFT 复制——单写者（single-writer per bucket）+ 共享持久化 取代了多副本状态机。

## 元数据容量：跟随使用量增长

- 元数据与数据**共用同一个 TLC NVMe 池**，没有为元数据划出固定分区或固定百分比。
- 池按固定大小的**分配单元（extent，如 64 MB）**管理，Bucket 需要空间时向池申请，删除/合并后归还——元数据占用量精确等于实际使用量。
- 空文件系统的元数据开销近乎为零；万亿小对象场景下元数据可自然增长到池的可观比例，不需要提前容量规划。
- 动态分配的仲裁与水位控制见 [tiering.md](../features/tiering.md) 的"TLC 池动态分配"一节。

## Metadata QoS

元数据操作与数据读写分开限速（三维 QoS 之一，详见 [qos.md](../features/qos.md)）：

- **限速点**：每个 Bucket 入口挂租户/文件系统粒度的令牌桶，元数据 op（lookup、create、unlink、stat、readdir、setattr…）逐一计数。
- **分布式记账**：单租户的元数据请求天然散落在众多 Bucket 上，采用分片令牌 + 周期重平衡（与数据 QoS 同一套机制），避免集中记账。
- **优先级**：内部后台操作（分层迁移的指针更新、再平衡、回收）使用低优先级令牌，前台永远优先。
- **过载保护**：Bucket 队列深度超阈值时反压到 Access 层，快速拒绝而非排队恶化尾延迟。

## 扩展与再平衡

- **加节点**：新计算节点加入后，协调器把部分 Bucket 迁移过去——由于状态在共享池，"迁移"只是换个节点运行进程，无数据搬迁。
- **加 SSD/ebox**：新 SSD 纳入共享池后，新分配的元数据 extent 自动落在新盘上；后台可选做 extent 级重平衡以摊平磨损。
- **万亿对象推演**：单 Bucket 承载 ~1 亿对象（B-tree 深度 3–4），万亿对象约需 1 万个 Bucket；按每节点承载 64–128 个 Bucket 计，约 100–160 个计算节点即可承载全部元数据处理，见 [scaling.md](../scaling.md)。

## 一致性模型

- 单 Bucket 内：单写者串行化，日志先行（WAL），天然强一致。
- 跨 Bucket：两阶段提交（如 rename、跨目录硬链接），协调者为发起 Bucket。
- Bucket 接管：新节点必须先在共享池上对日志段加**租约围栏（lease fencing）**，防止旧进程"僵尸写"。
