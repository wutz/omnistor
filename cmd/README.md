# cmd

各服务入口（main 包）。

## 计划服务

| 二进制 | 职责 |
| --- | --- |
| `omnistor-frontend` | 协议前端（NFS/S3/iSCSI 网关） |
| `omnistor-metadata` | 元数据服务（分片 + RAFT） |
| `omnistor-data` | 数据服务（对象存储/副本/分层） |
| `omnistor-admin` | 管控与编排（ebox、监控、放置决策） |
| `omnistor-ctl` | 命令行管理工具 |

> 🚧 待实现：待接口定义完成后编写。
