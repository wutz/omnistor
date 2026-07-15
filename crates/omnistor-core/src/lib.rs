//! omnistor-core: 基础类型——ID、介质类别、元数据键（租户前缀）、错误。
//!
//! 对应文档：docs/architecture/overview.md

use std::fmt;

/// 租户 ID。租户是命名空间/QoS/Quota/密钥的第一级边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(pub u32);

/// 元数据 Bucket 编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BucketId(pub u32);

/// 存储池 ID（硬件分池，docs/architecture/pools.md）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PoolId(pub u32);

/// SNode ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SNodeId(pub u32);

/// CNode ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CNodeId(pub u32);

/// 分配单元（extent）ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtentId(pub u64);

/// 介质类别。分层在类别之间移动数据；池间均衡只在同类别内进行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaClass {
    /// 主层：元数据 + 热数据，元数据永不下沉出此层。
    TlcNvme,
    /// 温层。
    QlcNvme,
    /// 冷层。
    Hdd,
    /// 归档层：集群外对象存储。
    ExternalS3,
}

impl MediaClass {
    /// 温度下沉顺序中的下一层（可跳层由放置引擎决定）。
    pub fn colder(self) -> Option<MediaClass> {
        match self {
            MediaClass::TlcNvme => Some(MediaClass::QlcNvme),
            MediaClass::QlcNvme => Some(MediaClass::Hdd),
            MediaClass::Hdd => Some(MediaClass::ExternalS3),
            MediaClass::ExternalS3 => None,
        }
    }
}

/// 元数据键：租户 ID 为最高位前缀，其余为命名空间内的对象/inode/目录键。
///
/// 排序与哈希都先看租户前缀，因此同一 Bucket 内多租户的键空间严格隔离
/// （docs/features/multitenancy.md）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetaKey {
    pub tenant: TenantId,
    pub key: Vec<u8>,
}

impl MetaKey {
    pub fn new(tenant: TenantId, key: impl Into<Vec<u8>>) -> Self {
        Self {
            tenant,
            key: key.into(),
        }
    }

    /// FNV-1a 哈希（含租户前缀），用于一致性哈希路由到 Bucket。
    pub fn route_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.tenant.0.to_be_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        for &b in &self.key {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }
}

/// 统一错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// QoS 限流：请求应被快速拒绝/反压，而非排队。
    Throttled { dimension: &'static str },
    /// 配额超限（硬约束）。
    QuotaExceeded { scope: String },
    /// 容量不足（如纯 TLC 配置触发保护水位）。
    NoSpace { media: MediaClass },
    /// 租约围栏拒绝（旧任期写入）。
    Fenced { bucket: BucketId },
    /// Bucket 未运行/未找到。
    BucketUnavailable(BucketId),
    /// 租户不存在或已删除。
    UnknownTenant(TenantId),
    /// 通用非法参数。
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Throttled { dimension } => write!(f, "throttled on {dimension}"),
            Error::QuotaExceeded { scope } => write!(f, "quota exceeded: {scope}"),
            Error::NoSpace { media } => write!(f, "no space on {media:?}"),
            Error::Fenced { bucket } => write!(f, "fenced write to bucket {}", bucket.0),
            Error::BucketUnavailable(b) => write!(f, "bucket {} unavailable", b.0),
            Error::UnknownTenant(t) => write!(f, "unknown tenant {}", t.0),
            Error::Invalid(msg) => write!(f, "invalid: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_prefix_separates_key_space() {
        let a = MetaKey::new(TenantId(1), "same/path");
        let b = MetaKey::new(TenantId(2), "same/path");
        assert_ne!(a, b);
        assert_ne!(a.route_hash(), b.route_hash());
    }

    #[test]
    fn media_class_sink_order() {
        assert_eq!(MediaClass::TlcNvme.colder(), Some(MediaClass::QlcNvme));
        assert_eq!(MediaClass::Hdd.colder(), Some(MediaClass::ExternalS3));
        assert_eq!(MediaClass::ExternalS3.colder(), None);
    }
}
