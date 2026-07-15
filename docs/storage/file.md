# 文件存储（NFS）

OmniStor 的文件存储**复用 Vastdata 的 NFS 方案**：直接采用其 NFS 前端实现，对接 OmniStor 的 Metadata + Data 层。

## 设计

- **NFS 前端**：复用 Vastdata NFS 服务（NFSv3/NFSv4.x），作为 Access Layer 的文件协议入口。
- **POSIX 语义**：目录树、inode、权限由元数据 Bucket 提供（见 [metadata.md](../architecture/metadata.md)），NFS 前端做协议翻译。
- **数据路径**：文件数据切分为对象，走与对象存储相同的数据放置与分层。
- **共享一切**：多个 NFS 前端共享同一命名空间，任意前端可服务任意路径。

## 与 Vastdata NFS 的集成

| 关注点 | 说明 |
| --- | --- |
| 复用范围 | NFS 协议栈、导出管理、NFSv4 状态机 |
| 适配点 | 底层从 Vastdata 私有元数据接口改为 OmniStor Metadata API |
| 收益 | 避免重复造轮子，继承 Vastdata NFS 的成熟度与兼容性 |

## 特性

- NFSv3 / NFSv4.1 / NFSv4.2
- POSIX 语义（强一致元数据）
- QoS：Metadata IOPS、Data IOPS、Data BW
- Quota：目录级容量与文件数配额

## 待定

- [ ] Vastdata NFS 方案的获取与许可方式
- [ ] 元数据接口适配层规范
