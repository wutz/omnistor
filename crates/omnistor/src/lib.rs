//! omnistor: 顶层组装——把租户/QoS/Quota/元数据 Bucket/放置引擎
//! 串成一条可验证的写路径（设计原型，无 I/O）。
//!
//! 写路径（docs/architecture/dase.md）：
//! 认证租户 → QoS 令牌 → Quota 校验 → 路由到元数据 Bucket →
//! 放置引擎选池 → TLC extent 分配 → 提交元数据。

use std::collections::HashMap;

use omnistor_core::{BucketId, CNodeId, Error, MediaClass, MetaKey, Result, TenantId};
use omnistor_metadata::{
    BucketProcess, BucketRouter, ExtentAllocator, PoolWatermarks, Purpose, SharedState,
};
use omnistor_placement::{PlacementEngine, PoolState};
use omnistor_qos::{Dimension, Priority, QosEntity, QosSpec};
use omnistor_quota::{QuotaLimit, QuotaManager};
use omnistor_tenant::{Placement, TenantRegistry, TenantSpec};

pub use omnistor_core as core;
pub use omnistor_metadata as metadata;
pub use omnistor_placement as placement;
pub use omnistor_qos as qos;
pub use omnistor_quota as quota;
pub use omnistor_tenant as tenant;

/// 单进程集群原型：真实系统中这些组件分布在 CNode 上，
/// SharedState 在 DASE 共享池中。
pub struct Cluster {
    pub tenants: TenantRegistry,
    pub quotas: QuotaManager,
    pub placement: PlacementEngine,
    pub tlc_extents: ExtentAllocator,
    router: BucketRouter,
    /// Bucket 共享状态（模拟共享池：与运行进程分离）。
    shared: HashMap<BucketId, SharedState>,
    /// 运行中的 Bucket 进程。
    buckets: HashMap<BucketId, BucketProcess>,
    /// 每租户 QoS（简化：单实体桶；生产为 ShardedQos 分布到各执行点）。
    tenant_qos: HashMap<TenantId, QosEntity>,
}

impl Cluster {
    pub fn new(bucket_count: u32, tlc_extents: u64) -> Self {
        Self {
            tenants: TenantRegistry::new(),
            quotas: QuotaManager::new(),
            placement: PlacementEngine::new(150),
            tlc_extents: ExtentAllocator::new(tlc_extents, PoolWatermarks::default()),
            router: BucketRouter::new(bucket_count),
            shared: HashMap::new(),
            buckets: HashMap::new(),
            tenant_qos: HashMap::new(),
        }
    }

    pub fn add_pool(&mut self, state: PoolState) {
        self.placement.upsert_pool(state);
    }

    /// 建租户：注册 + 配额 + QoS 一步到位。
    pub fn create_tenant(
        &mut self,
        name: &str,
        quota: QuotaLimit,
        qos: QosSpec,
        placement: Placement,
    ) -> Result<TenantId> {
        let id = self.tenants.create(TenantSpec {
            name: name.into(),
            placement,
        })?;
        self.quotas.set_tenant_limit(id, quota);
        self.tenant_qos.insert(id, QosEntity::new(qos));
        Ok(id)
    }

    fn bucket_for(&mut self, key: &MetaKey) -> BucketId {
        let id = self.router.route(key);
        if let std::collections::hash_map::Entry::Vacant(e) = self.buckets.entry(id) {
            let shared = self.shared.entry(id).or_default();
            // 简化：都调度到 CNode(0)；接管语义见 metadata crate 测试。
            e.insert(BucketProcess::take_over(id, CNodeId(0), shared));
        }
        id
    }

    /// 写入一个对象：完整前台路径。
    pub fn put_object(
        &mut self,
        tenant: TenantId,
        scope: &str,
        key: &str,
        size_bytes: u64,
    ) -> Result<()> {
        // 1. 租户存在且密钥可用（已删除租户 = 密码学擦除，直接拒绝）
        self.tenants.key(tenant)?;
        // 2. QoS：元数据 op + 数据 IOPS + 带宽三维取令牌
        let q = self
            .tenant_qos
            .get_mut(&tenant)
            .ok_or(Error::UnknownTenant(tenant))?;
        q.acquire(Dimension::MetadataIops, 1, Priority::Foreground)?;
        q.acquire(Dimension::DataIops, 1, Priority::Foreground)?;
        q.acquire(Dimension::DataBw, size_bytes, Priority::Foreground)?;
        // 3. Quota 硬校验
        self.quotas.charge(tenant, scope, size_bytes, 1)?;
        // 4. 放置：新写入一律先落 TLC（租户可能绑定专属池）
        let dedicated = match self.tenants.placement(tenant)? {
            Placement::DedicatedPool(p) => Some(p),
            Placement::Shared => None,
        };
        let pool = match self
            .placement
            .select_pool(MediaClass::TlcNvme, tenant, dedicated)
        {
            Ok(p) => p,
            Err(e) => {
                self.quotas.release(tenant, scope, size_bytes, 1); // 回滚配额
                return Err(e);
            }
        };
        // 5. TLC extent 分配（数据用途，受水位仲裁）
        if let Err(e) = self.tlc_extents.allocate(Purpose::Data) {
            self.quotas.release(tenant, scope, size_bytes, 1);
            return Err(e);
        }
        self.placement.commit(pool, 1)?;
        // 6. 元数据落 Bucket（journal 先行）
        let meta_key = MetaKey::new(tenant, key);
        let bucket = self.bucket_for(&meta_key);
        // 元数据自身占用（extent 按需分配，跟随使用量增长）
        self.tlc_extents.allocate(Purpose::Metadata)?;
        let shared = self.shared.get_mut(&bucket).expect("shared state exists");
        self.buckets
            .get_mut(&bucket)
            .ok_or(Error::BucketUnavailable(bucket))?
            .put(shared, meta_key, size_bytes.to_be_bytes().to_vec())
    }

    /// 读元数据（lookup）。
    pub fn stat(&mut self, tenant: TenantId, key: &str) -> Result<u64> {
        self.tenants.key(tenant)?;
        let q = self
            .tenant_qos
            .get_mut(&tenant)
            .ok_or(Error::UnknownTenant(tenant))?;
        q.acquire(Dimension::MetadataIops, 1, Priority::Foreground)?;
        let meta_key = MetaKey::new(tenant, key);
        let bucket = self.bucket_for(&meta_key);
        let v = self.buckets[&bucket]
            .get(&meta_key)
            .ok_or_else(|| Error::Invalid(format!("not found: {key}")))?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&v[..8]);
        Ok(u64::from_be_bytes(buf))
    }

    /// 删除租户：密码学擦除 + 配额与 QoS 记账清理。
    pub fn delete_tenant(&mut self, tenant: TenantId) -> Result<()> {
        self.tenants.delete(tenant)?;
        self.quotas.remove_tenant(tenant);
        self.tenant_qos.remove(&tenant);
        Ok(())
    }

    /// QoS tick（时间推进，补充令牌）。
    pub fn tick(&mut self) {
        for q in self.tenant_qos.values_mut() {
            q.tick();
        }
    }

    /// 活跃 Bucket 数（触达过的分片）。
    pub fn active_buckets(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnistor_core::PoolId;

    fn cluster() -> Cluster {
        let mut c = Cluster::new(256, 100_000);
        c.add_pool(PoolState {
            id: PoolId(1),
            media: MediaClass::TlcNvme,
            capacity: 50_000,
            used: 0,
            load_headroom_permille: 500,
            dedicated_to: None,
        });
        c.add_pool(PoolState {
            id: PoolId(2),
            media: MediaClass::TlcNvme,
            capacity: 50_000,
            used: 0,
            load_headroom_permille: 500,
            dedicated_to: None,
        });
        c
    }

    fn default_qos() -> QosSpec {
        QosSpec {
            metadata_iops: 10_000,
            data_iops: 10_000,
            data_bw_bytes: 1 << 30,
            burst_multiple: 2,
        }
    }

    #[test]
    fn end_to_end_write_and_stat() {
        let mut c = cluster();
        let t = c
            .create_tenant(
                "acme",
                QuotaLimit {
                    capacity_bytes: Some(1 << 40),
                    object_count: Some(1000),
                },
                default_qos(),
                Placement::Shared,
            )
            .unwrap();
        c.put_object(t, "bucket-a", "photos/1.jpg", 4096).unwrap();
        assert_eq!(c.stat(t, "photos/1.jpg").unwrap(), 4096);
        // 元数据与数据都精确记账
        assert_eq!(c.tlc_extents.metadata_used(), 1);
        assert_eq!(c.tlc_extents.data_used(), 1);
    }

    #[test]
    fn tenants_are_isolated_end_to_end() {
        let mut c = cluster();
        let quota = QuotaLimit {
            capacity_bytes: Some(1 << 30),
            object_count: None,
        };
        let a = c
            .create_tenant("acme", quota, default_qos(), Placement::Shared)
            .unwrap();
        let b = c
            .create_tenant("globex", quota, default_qos(), Placement::Shared)
            .unwrap();
        c.put_object(a, "s", "same/path", 100).unwrap();
        // 租户 b 看不到租户 a 的同名对象
        assert!(c.stat(b, "same/path").is_err());
        c.put_object(b, "s", "same/path", 200).unwrap();
        assert_eq!(c.stat(a, "same/path").unwrap(), 100);
        assert_eq!(c.stat(b, "same/path").unwrap(), 200);
    }

    #[test]
    fn objects_spread_across_buckets() {
        let mut c = cluster();
        let t = c
            .create_tenant(
                "acme",
                QuotaLimit::default(),
                default_qos(),
                Placement::Shared,
            )
            .unwrap();
        for i in 0..2000 {
            c.put_object(t, "s", &format!("obj/{i}"), 1).unwrap();
            if i % 100 == 0 {
                c.tick();
            }
        }
        // 元数据并行度：对象散布到大多数 Bucket
        assert!(
            c.active_buckets() > 200,
            "only {} buckets active",
            c.active_buckets()
        );
    }

    #[test]
    fn qos_throttles_then_recovers() {
        let mut c = cluster();
        let t = c
            .create_tenant(
                "acme",
                QuotaLimit::default(),
                QosSpec {
                    metadata_iops: 5,
                    data_iops: 100,
                    data_bw_bytes: 1 << 20,
                    burst_multiple: 1,
                },
                Placement::Shared,
            )
            .unwrap();
        let mut throttled = false;
        for i in 0..100 {
            match c.put_object(t, "s", &format!("k{i}"), 1) {
                Err(Error::Throttled { .. }) => {
                    throttled = true;
                    break;
                }
                other => other.unwrap(),
            }
        }
        assert!(throttled, "expected throttling under tiny QoS");
        c.tick(); // 补充令牌
        c.put_object(t, "s", "after-tick", 1).unwrap();
    }

    #[test]
    fn quota_exceeded_rolls_back_nothing_leaks() {
        let mut c = cluster();
        let t = c
            .create_tenant(
                "acme",
                QuotaLimit {
                    capacity_bytes: Some(100),
                    object_count: None,
                },
                default_qos(),
                Placement::Shared,
            )
            .unwrap();
        c.put_object(t, "s", "a", 100).unwrap();
        let data_used_before = c.tlc_extents.data_used();
        assert!(matches!(
            c.put_object(t, "s", "b", 1),
            Err(Error::QuotaExceeded { .. })
        ));
        // 失败的写不泄漏 extent
        assert_eq!(c.tlc_extents.data_used(), data_used_before);
        assert!(c.stat(t, "b").is_err());
    }

    #[test]
    fn deleted_tenant_is_rejected_at_entry() {
        let mut c = cluster();
        let t = c
            .create_tenant(
                "acme",
                QuotaLimit::default(),
                default_qos(),
                Placement::Shared,
            )
            .unwrap();
        c.put_object(t, "s", "x", 1).unwrap();
        c.tenants.delete(t).unwrap(); // 密码学擦除
        assert_eq!(
            c.put_object(t, "s", "y", 1).unwrap_err(),
            Error::UnknownTenant(t)
        );
        assert_eq!(c.stat(t, "x").unwrap_err(), Error::UnknownTenant(t));
    }

    #[test]
    fn dedicated_tenant_lands_on_own_pool() {
        let mut c = cluster();
        // 专属池
        c.add_pool(PoolState {
            id: PoolId(9),
            media: MediaClass::TlcNvme,
            capacity: 10_000,
            used: 0,
            load_headroom_permille: 1000,
            dedicated_to: Some(TenantId(1)),
        });
        let bank = c
            .create_tenant(
                "bank",
                QuotaLimit::default(),
                default_qos(),
                Placement::DedicatedPool(PoolId(9)),
            )
            .unwrap();
        c.put_object(bank, "vault", "doc", 1).unwrap();
        assert_eq!(c.placement.pool(PoolId(9)).unwrap().used, 1);
        // 共享池未被触碰
        assert_eq!(
            c.placement.pool(PoolId(1)).unwrap().used + c.placement.pool(PoolId(2)).unwrap().used,
            0
        );
    }
}
