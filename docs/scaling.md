# 扩展性：万亿对象 / 1 EiB

OmniStor 设计目标：单集群支撑**万亿（10¹²）对象**与 **1 EiB** 容量。

## 扩展瓶颈分析

| 层 | 瓶颈风险 | 应对 |
| --- | --- | --- |
| Access (前端) | 协议连接数、CPU | 无状态 CNode 横向扩展，按 QPS 弹性 |
| Metadata | 对象数膨胀、元数据 IOPS | Bucket 分片分布到所有 CNode/SSD，加节点即加并行度 |
| Data | 容量、带宽 | ebox 横向堆叠 + 外部 S3 归档层 |
| 网络 | 东西向 NVMe-oF 流量 | 高带宽 fabric，CNode 直读直写免转发 |

## 元数据扩展（Bucket 模型）

- 万亿对象按**哈希切分为 ~1 万个 Bucket**，每 Bucket 承载 ~1 亿对象（B-tree 深度 3–4）。
- Bucket 是处理单元而非存储位置：状态持久化在 DASE 共享 TLC 池，**加 CNode 时把 Bucket 调度过去即可，零数据迁移**——这与 shared-nothing 系统的"分片再平衡搬数据"有本质区别。
- 元数据容量随对象数自然增长（extent 按需分配），无需预估元数据盘比例。
- 超大目录按前缀二次切分跨多个 Bucket，支撑大规模 LIST。
- 详见 [architecture/metadata.md](architecture/metadata.md)。

## 数据层扩展

- 新 ebox 加入即纳入共享池，新写入的纠删条带自动分散到新 SSD。
- 容量逼近上限时优先在利用率低的 ebox 上放置；配置外部 S3 层后冷数据外溢，本地容量压力有上界。
- 纠删组跨 ebox 构建，单 ebox 故障不丢数据；重建由所有 CNode 并行分担。

## 容量推演

见 [architecture/topology.md](architecture/topology.md) 的容量估算示例。

## 待定

- [ ] Bucket 数量的初始规划与在线分裂
- [ ] 跨数据中心扩展（单集群 vs 联邦）
- [ ] 外部 S3 层的多目标与生命周期联动
