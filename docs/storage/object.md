# 对象存储（S3）

OmniStor 提供兼容 S3 的对象存储接口，作为 Access Layer 的对象协议入口。

## 设计

- **桶（Bucket）/ 对象（Object）**：对象是 Data 层的原生单位，桶是 Metadata 层的命名空间容器。
- **S3 前端**：实现 S3 REST API，将请求翻译为元数据操作 + 对象读写。
- **原生寻址**：对象存储与块/文件共享同一套底层对象寻址与副本/纠删管理，无额外转换开销。

## 特性

- S3 兼容 API（PUT/GET/DELETE/LIST / multipart / 版本控制 / 生命周期）
- 多版本与生命周期策略（自动分层到 QLC/HDD/外部 S3）
- QoS：Data IOPS、Data BW（按桶/租户）
- Quota：桶级容量与对象数配额
- 大规模 LIST：基于元数据分片与游标索引，支撑万亿对象

## 万亿对象挑战

- LIST 性能：传统目录式 LIST 不可行，采用**前缀分片 + 并行扫描 + 游标**，大桶索引跨多个元数据 Bucket 承载（见 [metadata.md](../architecture/metadata.md)）。
- 对象 ID：使用内容无关的全局唯一 ID，避免哈希冲突与热点。
- 元数据压缩：对象元数据编码紧凑，全部常驻 TLC NVMe 池且永不下沉。

## 待定

- [ ] S3 兼容性范围（signature v4、ACL、STS 等）
- [ ] 生命周期规则引擎
