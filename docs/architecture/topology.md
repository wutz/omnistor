# 集群拓扑与容量规模

## 规模目标

- 对象数量：**万亿级（10¹²）**
- 单集群容量：**1 EiB（2⁶⁰ 字节）**（本地介质 + 外部对象存储层合计）

## 容量推演（示例）

以 TLC + HDD 混闪配置估算，假设单 HDD = 24 TB：

| 项目 | 数值 |
| --- | --- |
| 单 ebox HDD 数量 | 90 |
| 单 ebox 原始容量 | 90 × 24 TB = 2,160 TB ≈ 2.1 PiB |
| 纠删开销 (k+m, 如 8+3) | 有效系数 ≈ 0.73 |
| 单 ebox 有效容量 | ≈ 1.5 PiB |
| 达到 1 EiB 所需 ebox | 1024 PiB / 1.5 ≈ **683 个 ebox** |

配置外部 S3 归档层后，本地 ebox 数量可按热/温数据占比显著缩减。

> 以上为量级估算，实际取决于纠删策略、副本数与介质选型。

## 元数据规模

万亿对象的元数据全部常驻 TLC NVMe 池（元数据永不下沉）：

- 单对象元数据 ~1 KB，万亿对象 ≈ 1 PB 量级元数据——占 TLC 池的一部分，**随使用量增长而非预留**（见 [metadata.md](metadata.md)）。
- 元数据按 Bucket 哈希分片，单 Bucket 承载 ~1 亿对象，万亿对象约 1 万个 Bucket，分布到所有 CNode 并行处理。
- Bucket 状态在 DASE 共享池中，扩缩容与 failover 均无元数据搬迁。

## 拓扑示意

```
Cluster (1 EiB)
├── CNode-001 .. CNode-160        ← 无状态计算层
│   ├── Frontend gateways (NFS/S3/iSCSI)
│   ├── Metadata Buckets (每节点 64–128 个)
│   └── Data services (纠删/放置/分层)
│            │ NVMe-oF fabric（全互联）
├── ebox-001 .. ebox-N            ← 共享存储层
│   ├── TLC NVMe（主层：元数据+数据）
│   └── QLC NVMe / HDD（可选容量层）
├── 外部对象存储 (S3, 可选归档层)
└── Global coordinator（Bucket 调度、放置决策、监控）
```

## 扩展性设计

- **性能扩展**：加 CNode → 协调器把部分 Bucket 与前端负载调度过去，因状态在共享池，迁移即"换机运行"，秒级完成。
- **容量扩展**：加 ebox → 新 SSD 纳入共享池，新 extent 自动落新盘；HDD/QLC 层同理。
- **归档扩展**：外部 S3 层容量近乎无限，本地只保留热/温数据。

详见 [scaling.md](../scaling.md)。
