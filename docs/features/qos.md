# QoS（服务质量限制）

OmniStor 提供三维 QoS 限速，可按租户 / 桶 / 卷粒度配置：

| 维度 | 含义 | 适用协议 |
| --- | --- | --- |
| **Metadata IOPS** | 元数据操作速率（open/lookup/stat/list 等） | 文件、对象、块 |
| **Data IOPS** | 数据读写操作速率（按对象/块计） | 文件、对象、块 |
| **Data BW** | 数据读写带宽（字节/秒） | 文件、对象、块 |

## 实现思路

- **令牌桶（Token Bucket）**：每维度独立令牌桶，支持突发（burst）与持续速率（rate）。
- **分层执行**：
  - Access Layer 入口：首道限速，快速拒绝超额请求（保护后端）。
  - Metadata Bucket / Data 层：二次校验，防止跨前端的聚合超额。
- **分布式令牌**：单租户的令牌桶跨多个前端节点时，采用分片令牌 + 周期性重平衡，避免集中记账瓶颈。

### Metadata QoS（Bucket 级执行）

元数据操作的最终限速点在**元数据 Bucket 入口**（见 [metadata.md](../architecture/metadata.md)）：

- 单租户的元数据请求按哈希天然散落在众多 Bucket 上，每个 Bucket 持有该租户令牌桶的一个分片，与数据 QoS 共用分片令牌机制。
- 后台操作（分层迁移的指针更新、Bucket 再平衡、垃圾回收）使用**低优先级令牌**，前台元数据操作永远优先。
- Bucket 队列深度超阈值时反压到 Access 层，快速拒绝而非排队，保护尾延迟。

## 配置示例（概念）

```yaml
qos:
  tenant: "acme"
  metadata_iops: 50000     # 元数据 IOPS 上限
  data_iops: 100000        # 数据 IOPS 上限
  data_bw: "10GiB/s"       # 数据带宽上限
  burst: 2x                # 允许 2 倍突发
```

## 与 Quota 的区别

- **QoS**：限**速率**（吞吐/IOPS），软约束，超额限流不拒绝写入。
- **Quota**：限**总量**（容量/对象数），硬约束，超配额拒绝写入。

见 [quota.md](quota.md)。

## 待定

- [ ] 分布式令牌桶的一致性与精度
- [ ] QoS 优先级（高优租户抢占）
- [ ] 观测指标与告警
