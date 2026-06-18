# 块存储

OmniStor 通过块存储前端提供卷（Volume）抽象，协议支持 iSCSI 与 NVMe-oF。

## 设计

- **卷 = 一组对象序列**：块存储将卷切分为固定大小的对象（如 4 MB），映射到底层 Data 层的对象寻址。
- **元数据**：卷→对象映射表由 Metadata 层管理；卷的元数据（大小、快照、QoS）常驻 SCM/TLC。
- **协议前端**：iSCSI/NVMe-oF target 运行在 Frontend 节点，将 SCSI/NVMe 命令翻译为对象读写。
- **共享一切**：任一数据节点都可服务卷的任意对象，支持多路径与故障切换。

## 特性

- 精简配置（thin provisioning）
- 快照与克隆（基于对象 COW）
- QoS（见 [features/qos.md](../features/qos.md)）：Data IOPS、Data BW
- 介质分层：热卷落 SCM/TLC，冷卷下沉 QLC/HDD

## 待定

- [ ] 对象粒度（4 MB？可配置？）
- [ ] NVMe-oF 传输（TCP/RDMA/FC）
- [ ] 多路径与故障切换语义
