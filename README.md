# OmniStor

> Unified storage for the exabyte era — block, file, and object on one DASE platform.

OmniStor 是一个面向 EB 级规模的统一存储系统，灵感来自 Vastdata 的统一存储与 vastbox DASE（Disaggregated Shared-Everything）架构。它在一套通用 X86 集群上同时提供**块存储、文件存储、对象存储**三种协议，并通过多级介质分层（SCM SSD + QLC SSD、纯 TLC SSD，或 TLC SSD + HDD）在成本与性能之间取得平衡。

## 核心定位

| 维度 | 目标 |
| --- | --- |
| 协议 | 块（iSCSI/NVMe-oF）、文件（NFS，复用 Vastdata NFS 方案）、对象（S3） |
| 硬件 | 通用 X86 服务器，ebox 方案（标准高密度机箱） |
| 介质分层 | SCM SSD + QLC SSD、纯 TLC SSD，或 TLC SSD + HDD |
| 软件架构 | DASE（Disaggregated Shared-Everything，解耦共享一切） |
| 规模 | 万亿（10¹²）级对象数量，单集群 1 EiB 容量 |
| QoS | Metadata IOPS、Data IOPS、Data BW 三维限速 |
| Quota | 租户/桶/卷级别容量与对象数配额 |

## 设计原则

- ** disaggregated shared-everything**：计算与存储解耦，但所有存储节点共享全局命名空间与数据副本，元数据与数据分离。
- **介质感知分层**：热数据落 SCM/TLC，温/冷数据下沉到 QLC/HDD，由放置引擎按访问频度自动迁移。
- **水平无状态扩展**：元数据服务与数据服务均可线性扩展，无单点瓶颈。
- **协议统一**：三种协议共享同一套底层对象寻址与副本管理，NFS/S3/iSCSI 仅作为访问前端。

## 仓库结构

```
omnistor/
├── docs/
│   ├── architecture/   # DASE 架构、硬件方案、集群拓扑
│   ├── storage/        # 块/文件/对象三种协议设计
│   └── features/       # QoS、Quota、介质分层
├── api/                # 接口定义 (gRPC / REST / proto)
├── cmd/                # 各服务入口
├── pkg/                # 核心库
└── deploy/             # 部署编排 (ebox / k8s / compose)
```

详见 [docs/architecture/overview.md](docs/architecture/overview.md)。

## 状态

🚧 设计阶段（design phase）— 当前仓库包含架构设计与接口骨架，尚无可运行代码。
