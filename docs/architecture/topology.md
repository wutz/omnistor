# 集群拓扑与容量规模

## 规模目标

- 对象数量：**10 万亿级（10¹³）**
- 单集群容量：**10 EiB**（本地介质 + 外部对象存储层合计）

## 容量推演（示例）

以 TLC + HDD 混闪配置估算，假设单 HDD = 24 TB：

| 项目 | 数值 |
| --- | --- |
| 单 SNode HDD 数量 | 90 |
| 单 SNode 原始容量 | 90 × 24 TB = 2,160 TB ≈ 2.1 PiB |
| 纠删开销 (k+m, 如 8+3) | 有效系数 ≈ 0.73 |
| 单 SNode 有效容量 | ≈ 1.5 PiB |
| 10 EiB 全本地所需 SNode | 10,240 PiB / 1.5 ≈ **~6,800 台 SNode** |
| 本地 20% + 外部 S3 80% | 本地 2 EiB ≈ **~1,400 台 SNode** |

10 EiB 全部落本地介质并不现实也非必要——设计上以**外部 S3 归档层承载冷数据主体**，本地 SNode 只保有热/温数据；上表第二行是更典型的部署形态。

> 以上为量级估算，实际取决于纠删策略、副本数、介质选型与冷热占比。

## 元数据规模

10 万亿对象的元数据全部常驻 TLC NVMe 池（元数据永不下沉）：

- 单对象元数据 ~1 KB，10 万亿对象 ≈ 10 PB 量级元数据——**随使用量增长而非预留**（见 [metadata.md](metadata.md)）；即使数据主体在外部 S3，这部分也必须由本地 TLC 池承载，是本地 TLC 容量规划的下限之一。
- 元数据按 Bucket 哈希分片，单 Bucket 承载 ~1 亿对象，10 万亿对象约 **10 万个 Bucket**，分布到所有 CNode 并行处理；冷 Bucket 可休眠换出，活跃 Bucket 数远小于总数。
- 按每 CNode 常驻 64–128 个活跃 Bucket 计，千级 CNode 即可承载全量活跃元数据处理，随活跃度弹性伸缩。
- Bucket 状态在 DASE 共享池中，扩缩容与 failover 均无元数据搬迁。

## 拓扑示意

```
Cluster (10 EiB, 10¹³ objects)
├── CNode-0001 .. CNode-1000+     ← 无状态计算层
│   ├── Frontend gateways (NFS/S3/iSCSI)
│   ├── Metadata Buckets (共 ~10 万，活跃常驻/冷可休眠)
│   └── Data services (纠删/放置/分层)
│            │ NVMe-oF fabric（zone 内全互联）
├── zone-01 .. zone-NN            ← 故障域分组
│   └── SNode-xxx（x86 存储服务器: TLC NVMe 主层 + 可选 QLC/HDD）
├── 外部对象存储 (S3, 冷数据主体)
└── Global coordinator（Bucket 调度、放置决策、监控）
```

万级 SNode 规模下，纠删组与重建流量限制在 zone 内（见 [scaling.md](../scaling.md)），zone 间仅有放置决策与再平衡的控制流。

## 扩展性设计

- **性能扩展**：加 CNode → 协调器把部分 Bucket 与前端负载调度过去，因状态在共享池，迁移即"换机运行"，秒级完成。
- **容量扩展**：加 SNode → 新 SSD 纳入共享池，新 extent 自动落新盘；HDD/QLC 层同理。
- **归档扩展**：外部 S3 层容量近乎无限，本地只保留热/温数据与全量元数据。

详见 [scaling.md](../scaling.md)。
