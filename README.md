# OmniStor

> Unified storage for the exabyte era — block, file, and object on one DASE platform.

OmniStor 是一个面向 EB 级规模的统一存储系统：核心架构采用 DASE（Disaggregated Shared-Everything）——无状态计算节点经 NVMe-oF 共享全部存储介质；元数据切分为大量 Bucket，分布到所有 TLC NVMe SSD 并行处理。它在一套通用 X86 集群上同时提供**块存储、文件存储、对象存储**三种协议。

## 核心定位

| 维度 | 目标 |
| --- | --- |
| 协议 | 块（iSCSI/NVMe-oF）、文件（NFS）、对象（S3） |
| 软件架构 | DASE 骨架 × Bucket 分片元数据 |
| 硬件 | 通用 x86 服务器（无状态 CNode + SNode 存储节点），NVMe-oF 全互联 |
| 异构硬件 | 按规格分池（Storage Pool），池间负载/容量均衡，池即故障域 |
| 主存储层 | TLC NVMe SSD：元数据与数据同池，容量动态分配 |
| 分层存储 | 可选下沉到 QLC NVMe / HDD / 外部对象存储（S3） |
| 规模 | 10 万亿（10¹³）级对象数量，单集群 10 EiB 容量 |
| QoS | Metadata IOPS、Data IOPS、Data BW 三维限速 |
| Quota | 租户/桶/卷级别容量与对象数配额 |
| 多租户 | 命名空间/VIP/认证/加密密钥按租户隔离，租户内两级自助管理 |

## 设计原则

- **DASE（解耦共享一切）**：CNode 完全无状态，任意 CNode 可见任意 SSD；性能与容量独立扩展；failover 零数据迁移。
- **Bucket 分片元数据**：无专用元数据节点，元数据 Bucket 分布到**所有** TLC NVMe SSD；容量随使用量增长而非固定预留；支持 Metadata QoS。
- **统一 TLC 主层**：元数据与数据共用 TLC NVMe 池，extent 级动态分配，水位仲裁。
- **温度驱动分层**：冷数据下沉 QLC / HDD / 外部 S3（可任意组合），元数据永不下沉，保证命名空间操作延迟稳定。
- **硬件分池**：同集群混用不同规格/代际硬件，池内同构纠删、池间加权均衡，故障与重建限制在池内。
- **原生多租户**：租户是命名空间、认证、QoS/Quota、加密密钥的第一级边界，一套集群切分为多个逻辑独立的存储服务。
- **协议统一**：三种协议共享同一套元数据 Bucket 与数据服务，NFS/S3/iSCSI 仅作为访问前端。

## 仓库结构

```
omnistor/
├── docs/
│   ├── architecture/   # DASE、元数据(Bucket)、硬件、拓扑
│   ├── storage/        # 块/文件/对象三种协议设计
│   └── features/       # QoS、Quota、分层存储
├── api/                # 接口定义 (gRPC / REST / proto)
├── cmd/                # 各服务入口
├── pkg/                # 核心库
└── deploy/             # 部署编排 (裸金属 / k8s / compose)
```

详见 [docs/architecture/overview.md](docs/architecture/overview.md)。

## 状态

🚧 设计阶段（design phase）— 当前仓库包含架构设计与接口骨架，尚无可运行代码。
