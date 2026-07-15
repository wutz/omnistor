# pkg

OmniStor 核心库，供各服务复用。

## 计划模块

| 模块 | 职责 |
| --- | --- |
| `metadata` | 命名空间、Bucket 分片、B-tree/日志、租约围栏 |
| `data` | 对象寻址、副本/纠删、分层引擎 |
| `placement` | 介质感知放置与迁移决策 |
| `qos` | 令牌桶、分布式限速 |
| `quota` | 配额计数与校验 |
| `access/nfs` | NFS 协议前端 |
| `access/s3` | S3 兼容前端 |
| `access/block` | iSCSI/NVMe-oF 前端 |
| `cluster` | ebox 编排、拓扑、再平衡 |
| `pb` | proto 生成的接口类型 |

> 🚧 待实现：待接口与架构稳定后填充。
