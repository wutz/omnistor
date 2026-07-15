# DASE 架构（Disaggregated Shared-Everything）

DASE = Disaggregated Shared-Everything（解耦共享一切），源自 VastData 的核心架构思想。OmniStor 以 DASE 为骨架，并在其上运行 WekaFS 风格的元数据 Bucket（见 [metadata.md](metadata.md)）。

## 与传统架构对比

| 架构 | 计算与存储 | 介质可见性 | 扩展瓶颈 | 代表 |
| --- | --- | --- | --- | --- |
| 传统 Scale-up | 耦合 | 本机 | 单点 | 双控阵列 |
| Scale-out (shared-nothing) | 耦合 | 本机，跨节点靠副本/纠删 | 节点间重建放大 | WekaFS、Ceph |
| **DASE (OmniStor)** | **解耦** | **任意 CNode 可见任意 SSD** | **近乎线性** | VastData |

## 解耦（Disaggregated）

- **CNode（计算节点）完全无状态**：协议前端、元数据 Bucket、数据服务都运行在 CNode 上，持久化状态全部在 ebox 共享池中。CNode 宕机不丢任何状态，加减 CNode 只改变性能不改变容量。
- **ebox（存储机箱）不运行业务逻辑**：仅通过冗余 fabric 模块把 NVMe SSD 暴露到 NVMe-oF 网络。
- 性能与容量独立扩展：算力不足加 CNode，容量不足加 ebox。

## 共享一切（Shared-Everything）

- 任意 CNode 通过 NVMe-oF 直接读写任意 ebox 中的任意 SSD——数据和元数据都没有"归属节点"。
- **写入路径**：前端 CNode → 定位元数据 Bucket（哈希，可能在本机或另一 CNode）→ 数据在 CNode 上纠删条带化 → 直写多个 ebox 的 TLC SSD（跨故障域）。
- **读取路径**：前端 CNode → Bucket 查 extent 指针 → 直接从 ebox 读数据块，无需经过其他 CNode 转发。
- 好处：负载天然均衡；CNode 故障切换零数据迁移；重建只发生在 SSD/ebox 故障时，且由所有 CNode 并行分担。

## 一致性模型

- **元数据**：每个 Bucket 单写者串行化 + WAL 日志，强一致。Bucket 状态在共享池中，接管时新节点重放日志即可，无需 RAFT 多副本状态机（详见 [metadata.md](metadata.md)）。
- **数据**：CNode 直写纠删条带，条带落盘即持久；跨介质分层迁移时旧副本保留到指针切换后，读不中断。
- **围栏**：Bucket 与纠删条带的写入均通过租约围栏防止脑裂下的双写。

## 与硬件的协同

DASE 依赖两类构建块——无状态 CNode 与仅存储的 ebox，通过 NVMe-oF fabric 互联。详见 [hardware.md](hardware.md)。
