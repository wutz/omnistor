# Quota（配额）

OmniStor 支持多级配额，硬约束资源使用上限，超限拒绝写入。

## 配额维度

| 粒度 | 容量配额 | 对象/文件数配额 | 适用 |
| --- | --- | --- | --- |
| 租户（Tenant） | ✅ | ✅ | 账户级总量上限 |
| 桶（Bucket） | ✅ | ✅ | 对象存储 |
| 卷（Volume） | ✅ | — | 块存储 |
| 目录（Directory） | ✅ | ✅ | 文件存储（NFS） |

## 实现思路

- **统计**：Metadata 层维护各级配额计数器（已用容量、对象数），写入时原子递增。
- **校验**：写入路径在分配对象前校验配额，超限返回 `QUOTA_EXCEEDED`（对应 S3 的 507 SlowDown/QuotaExceeded、NFS 的 EDQUOT）。
- **一致性**：计数器与元数据同事务提交，避免计数漂移；后台周期性全量校准。
- **软/硬配额**：硬配额拒绝写入；可选软配额仅告警。

## 配置示例（概念）

```yaml
quota:
  tenant: "acme"
  capacity: "1PiB"
  object_count: 1000000000000   # 1 万亿
  buckets:
    - name: "archive"
      capacity: "500TiB"
      object_count: 500000000000
```

## 与 QoS 的关系

Quota 管**总量**，QoS 管**速率**，二者正交。见 [qos.md](qos.md)。

## 待定

- [ ] 配额计数的分布式事务方案
- [ ] 配额超限的错误码与协议映射
- [ ] 配额变更的热更新
