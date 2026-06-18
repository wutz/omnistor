# api

OmniStor 内部接口定义层。

## 计划

- `proto/` — gRPC 接口定义（Metadata API、Data API、Admin API）
- `openapi/` — S3 兼容 REST 规范与扩展
- `go/` — 由 proto 生成的 Go 客户端/服务端桩

## 接口分组

| 接口 | 用途 |
| --- | --- |
| Metadata API | 命名空间、对象映射、配额、QoS 配置 |
| Data API | 对象读写、副本/纠删、分层迁移 |
| Admin API | 集群管理、ebox 编排、监控指标 |
| Access API | NFS/S3/iSCSI 前端对接底层 |

> 🚧 待定义：待架构设计稳定后填充 proto / openapi。
