# 文件存储（NFS）

OmniStor 的文件存储以 NFS 为协议入口，对接元数据 Bucket 与 Data 层。

## 设计

- **NFS 前端**：NFSv3/NFSv4.x 协议栈运行在 CNode 上，作为 Access Layer 的文件协议入口。
- **POSIX 语义**：目录树、inode、权限由元数据 Bucket 提供（见 [metadata.md](../architecture/metadata.md)），NFS 前端做协议翻译。
- **数据路径**：文件数据切分为对象，走与对象存储相同的数据放置与分层。
- **共享一切**：多个 NFS 前端共享同一命名空间，任意前端可服务任意路径。

## 前端实现

| 关注点 | 说明 |
| --- | --- |
| 协议栈 | NFS 协议解析、导出管理、NFSv4 状态机（锁、委托、会话） |
| 适配点 | 协议栈通过统一的 Metadata API 对接元数据 Bucket |
| 扩展性 | 前端无状态，NFSv4 状态持久化在共享池，前端故障客户端可无缝重连其他 CNode |

## 特性

- NFSv3 / NFSv4.1 / NFSv4.2
- POSIX 语义（强一致元数据）
- QoS：Metadata IOPS、Data IOPS、Data BW
- Quota：目录级容量与文件数配额

## 待定

- [ ] NFS 协议栈选型（自研 / 基于开源实现）
- [ ] 元数据接口适配层规范
