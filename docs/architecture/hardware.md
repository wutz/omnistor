# 硬件方案与 ebox

OmniStor 部署在通用 X86 服务器上，采用 **ebox** 标准化高密度机箱方案，并遵循 DASE 的计算/存储解耦。

## 两类构建块

| 构建块 | 内容 | 状态 | 扩展维度 |
| --- | --- | --- | --- |
| **CNode（计算节点）** | CPU + 内存 + 高速网卡，运行协议前端、元数据 Bucket、数据服务逻辑 | 无状态 | 性能（IOPS/带宽/元数据 op） |
| **ebox（存储机箱）** | TLC NVMe SSD（主层，必配）+ 可选 QLC NVMe / HDD（容量层），双冗余 NVMe-oF fabric 模块 | 持久化状态全部在此 | 容量 |

- CNode 通过 NVMe-oF（RoCE / TCP）访问所有 ebox 中的所有盘——**任意 CNode 可见任意 SSD**，这是 DASE 的物理基础。
- ebox 内不运行业务逻辑（仅 fabric 转发），CNode 与 ebox 可独立按需扩容。
- 逻辑角色（前端网关 / 元数据 Bucket / 数据服务）由软件在 CNode 上调度，硬件不绑定角色。

## 介质角色

### 主存储层：TLC NVMe SSD（必配）

- **元数据与数据统一落在 TLC NVMe 池**上，二者不做固定分区，按使用量动态分配（见 [tiering.md](../features/tiering.md)）。
- 元数据分布到**所有** TLC SSD（Bucket 分片，见 [metadata.md](metadata.md)），没有专用元数据盘。
- 新写入的数据一律先落 TLC 层，纠删组跨 ebox 构建。

### 容量分层（可选，三选零到多）

| 分层目标 | 位置 | 典型用途 |
| --- | --- | --- |
| QLC NVMe SSD | ebox 内 | 温数据，读多写少，保持全闪延迟 |
| HDD | ebox 内 | 冷数据，成本最优的本地容量底座 |
| 外部对象存储（S3） | 集群外 | 归档/近线，容量近乎无限，可跨机房 |

三种分层目标可任意组合（也可全不配，纯 TLC 全闪运行）。分层策略见 [tiering.md](../features/tiering.md)。

## 典型配置

### 配置 A：纯 TLC 全闪

ebox 仅配 TLC NVMe。元数据 + 全部数据同池，最低延迟，运维最简。适合性能敏感、容量中等的场景；可选挂外部 S3 做归档外溢。

### 配置 B：TLC + QLC 全闪

TLC 承载元数据与热数据，温数据下沉 QLC。全闪延迟下的容量优化，适合读密集大容量场景。

### 配置 C：TLC + HDD 混闪

TLC 承载元数据与热数据，冷数据下沉 HDD。成本最优的本地方案，适合归档与大规模对象存储。

### 配置 D：任意本地配置 + 外部 S3

在 A/B/C 基础上外接对象存储作为最冷层，本地容量可显著缩小。

## 网络

- CNode ↔ ebox：NVMe-oF，RoCE v2 优先（低延迟），TCP 兜底。
- CNode ↔ 客户端：以太网（NFS/S3/iSCSI）或 InfiniBand/RoCE（高性能文件客户端）。
- 东西向与南北向建议物理或逻辑隔离，避免重建/分层流量挤占前台。

## 节点角色（逻辑，非物理绑定）

| 逻辑角色 | 职责 | 资源倾向 |
| --- | --- | --- |
| Frontend / Gateway | 协议接入 (NFS/S3/iSCSI) | CPU 强，网卡多 |
| Metadata Bucket | 命名空间分片、索引、配额（见 [metadata.md](metadata.md)） | CPU + 内存，低延迟网络 |
| Data Service | 纠删、放置、分层迁移 | 带宽大 |

三种角色均运行在 CNode 上，可同机混部或按负载独立扩缩。
