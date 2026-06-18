# 架构总览

OmniStor 采用 **DASE（Disaggregated Shared-Everything）** 架构，参考 Vastdata vastbox 的设计哲学：计算与存储解耦，但存储平面全局共享命名空间与数据副本。

## 逻辑分层

```
┌──────────────────────────────────────────────────────────┐
│  Access Layer (协议前端)                                  │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                │
│  │ NFS (文件)│  │ S3 (对象)│  │ iSCSI/NVMe-oF (块)       │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                │
└───────┼─────────────┼─────────────┼──────────────────────┘
        │             │             │
┌───────▼─────────────▼─────────────▼──────────────────────┐
│  Metadata Layer (元数据服务, 可扩展无状态)                 │
│  - 全局命名空间 / inode / 对象映射 / 目录树               │
│  - 基于 KV (RAFT 复制)                                   │
└───────┬─────────────────────────────────────────────────┘
        │
┌───────▼─────────────────────────────────────────────────┐
│  Data Layer (数据服务, 共享一切)                          │
│  - 对象寻址 / 副本 / 纠删 / 分层迁移                      │
│  - 介质: SCM SSD · QLC SSD · TLC SSD · HDD              │
└─────────────────────────────────────────────────────────┘
```

## 关键特性落点

| 需求 | 架构支撑 |
| --- | --- |
| 块/文件/对象统一 | Access Layer 三前端共享 Metadata + Data 层 |
| SCM+QLC / TLC+HDD | Data Layer 介质感知放置引擎 |
| 万亿对象 / 1 EiB | Metadata 层分片 + Data 层横向扩展，见 [scaling.md](../scaling.md) |
| QoS | Access 层入口限速 + Metadata/Data 层配额令牌桶，见 [features/qos.md](../features/qos.md) |
| Quota | Metadata 层统计与配额校验，见 [features/quota.md](../features/quota.md) |
| 复用 NFS | Access Layer 文件前端直接复用 Vastdata NFS 实现 |

## 相关文档

- [DASE 详解](dase.md)
- [硬件方案与 ebox](hardware.md)
- [集群拓扑与容量规模](topology.md)
