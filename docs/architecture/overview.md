# 架构总览

OmniStor 的核心架构采用 **DASE（Disaggregated Shared-Everything）**——无状态计算节点通过 NVMe-oF 共享全部存储介质；元数据切分为大量 **Bucket**，分布到集群所有 TLC NVMe SSD 上并行处理。

## 逻辑分层

```
┌──────────────────────────────────────────────────────────┐
│  CNode（无状态计算节点，横向扩展）                          │
│  ┌─────────────────────────────────────────────────────┐ │
│  │ Access Layer (协议前端)                              │ │
│  │  NFS (文件) · S3 (对象) · iSCSI/NVMe-oF (块)         │ │
│  ├─────────────────────────────────────────────────────┤ │
│  │ Metadata Buckets（哈希分片，见 metadata.md）          │ │
│  │  inode / 目录树 / 对象索引 · B-tree · Metadata QoS   │ │
│  ├─────────────────────────────────────────────────────┤ │
│  │ Data Services（纠删 / 放置 / 分层迁移）               │ │
│  └─────────────────────────────────────────────────────┘ │
└─────────────────────────┬────────────────────────────────┘
                          │ NVMe-oF（任意 CNode ↔ 任意 SSD）
┌─────────────────────────▼────────────────────────────────┐
│  ebox 共享存储池（DASE）                                   │
│  主层: TLC NVMe（元数据+数据，动态分配，无固定边界）         │
│  分层: QLC NVMe / HDD（可选，ebox 内）                     │
└─────────────────────────┬────────────────────────────────┘
                          │ 可选归档外溢
                ┌─────────▼─────────┐
                │ 外部对象存储 (S3)  │
                └───────────────────┘
```

## 两大架构支柱

| 支柱 | 内容 |
| --- | --- |
| **DASE 共享存储** | 计算/存储解耦；全部 SSD 经 NVMe-oF 全局共享；无状态 CNode；ebox 硬件方案 |
| **Bucket 分片元数据** | 元数据按哈希切分为大量 Bucket；分布到所有 NVMe SSD；无专用元数据节点；容量随使用量增长 |

结合点：Bucket 的持久化状态放在 DASE 共享池而非本地盘，因此 Bucket failover 无需数据迁移或多副本状态机——详见 [metadata.md](metadata.md)。

## 关键特性落点

| 需求 | 架构支撑 |
| --- | --- |
| 块/文件/对象统一 | Access Layer 三前端共享 Metadata Bucket + Data 层 |
| 元数据用满全部 TLC | Bucket 分片 + extent 分配器跨所有 SSD 放置，见 [metadata.md](metadata.md) |
| 元数据/数据动态分容量 | 共享 TLC 池 extent 级分配 + 水位仲裁，见 [features/tiering.md](../features/tiering.md) |
| 分层到 QLC/HDD/外部 S3 | 温度驱动下沉，元数据永不下沉，见 [features/tiering.md](../features/tiering.md) |
| 10 万亿对象 / 10 EiB | Bucket 横向扩展 + ebox 堆叠，见 [scaling.md](../scaling.md) |
| QoS（含 Metadata QoS） | Bucket 入口令牌桶 + Access 层入口限速，见 [features/qos.md](../features/qos.md) |
| Quota | Bucket 内统计与配额校验，见 [features/quota.md](../features/quota.md) |

## 相关文档

- [DASE 详解](dase.md)
- [元数据设计（Bucket 分片）](metadata.md)
- [硬件方案与 ebox](hardware.md)
- [集群拓扑与容量规模](topology.md)
