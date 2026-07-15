# cmd

各服务入口（main 包）。

## 计划服务

| 二进制 | 职责 |
| --- | --- |
| `omnistor-frontend` | 协议前端（NFS/S3/iSCSI 网关） |
| `omnistor-metadata` | 元数据 Bucket 运行时（分片调度、B-tree/日志） |
| `omnistor-data` | 数据服务（纠删/放置/分层） |
| `omnistor-target` | SNode 盘导出服务（NVMe-oF target + 健康监控） |
| `omnistor-admin` | 管控与编排（节点、监控、放置决策） |
| `omnistor-ctl` | 命令行管理工具 |

> 🚧 待实现：待接口定义完成后编写。
