//! omnistor-metadata: Bucket 分片元数据。
//!
//! - 一致性哈希把元数据键路由到 Bucket；
//! - 每 Bucket 单写者：journal（WAL）先行 + B-tree，状态在 DASE 共享池；
//! - 接管（failover）= 新任期加租约围栏 + 重放日志，零数据迁移；
//! - extent 分配器：元数据与数据共用 TLC 池，水位仲裁。
//!
//! 对应文档：docs/architecture/metadata.md、docs/features/tiering.md

use std::collections::BTreeMap;

use omnistor_core::{BucketId, CNodeId, Error, MetaKey, Result};

pub mod extent;

pub use extent::{ExtentAllocator, PoolWatermarks, Purpose};

/// 一致性哈希路由：键（含租户前缀）→ Bucket。
#[derive(Debug, Clone)]
pub struct BucketRouter {
    bucket_count: u32,
}

impl BucketRouter {
    pub fn new(bucket_count: u32) -> Self {
        Self {
            bucket_count: bucket_count.max(1),
        }
    }

    pub fn route(&self, key: &MetaKey) -> BucketId {
        BucketId((key.route_hash() % u64::from(self.bucket_count)) as u32)
    }

    pub fn bucket_count(&self) -> u32 {
        self.bucket_count
    }
}

/// journal 记录：Bucket 的写操作先追加于此，再合并进 B-tree。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalRecord {
    Put { key: MetaKey, value: Vec<u8> },
    Delete { key: MetaKey },
}

/// Bucket 在共享池中的持久化状态（模拟 DASE：任意节点可见）。
///
/// 真实实现中 B-tree 节点与日志段都是共享 TLC 池上的块；
/// 这里用内存结构承载相同的语义：**状态与运行进程分离**。
#[derive(Debug, Default)]
pub struct SharedState {
    /// 已合并的 B-tree（checkpoint）。
    btree: BTreeMap<MetaKey, Vec<u8>>,
    /// checkpoint 之后的日志段。
    journal: Vec<JournalRecord>,
    /// 围栏任期：只有持有当前任期的写者能追加日志。
    fenced_epoch: u64,
}

/// Bucket 运行时：单写者进程，绑定共享状态 + 任期。
#[derive(Debug)]
pub struct BucketProcess {
    pub id: BucketId,
    pub owner: CNodeId,
    /// 本进程持有的租约任期。
    epoch: u64,
    /// 运行时 B-tree 视图（checkpoint + 已重放日志）。
    view: BTreeMap<MetaKey, Vec<u8>>,
}

impl BucketProcess {
    /// 接管（或首次启动）一个 Bucket：
    /// 1. 在共享状态上推进任期（租约围栏，旧写者从此被拒）；
    /// 2. 从 checkpoint + 日志重放构建运行时视图——**零数据迁移**。
    pub fn take_over(id: BucketId, owner: CNodeId, shared: &mut SharedState) -> Self {
        shared.fenced_epoch += 1;
        let mut view = shared.btree.clone();
        for rec in &shared.journal {
            match rec {
                JournalRecord::Put { key, value } => {
                    view.insert(key.clone(), value.clone());
                }
                JournalRecord::Delete { key } => {
                    view.remove(key);
                }
            }
        }
        Self {
            id,
            owner,
            epoch: shared.fenced_epoch,
            view,
        }
    }

    fn append(&mut self, shared: &mut SharedState, rec: JournalRecord) -> Result<()> {
        // 租约围栏：旧任期的"僵尸写"在此被拒。
        if self.epoch != shared.fenced_epoch {
            return Err(Error::Fenced { bucket: self.id });
        }
        shared.journal.push(rec.clone());
        match rec {
            JournalRecord::Put { key, value } => {
                self.view.insert(key, value);
            }
            JournalRecord::Delete { key } => {
                self.view.remove(&key);
            }
        }
        Ok(())
    }

    pub fn put(&mut self, shared: &mut SharedState, key: MetaKey, value: Vec<u8>) -> Result<()> {
        self.append(shared, JournalRecord::Put { key, value })
    }

    pub fn delete(&mut self, shared: &mut SharedState, key: MetaKey) -> Result<()> {
        self.append(shared, JournalRecord::Delete { key })
    }

    pub fn get(&self, key: &MetaKey) -> Option<&Vec<u8>> {
        self.view.get(key)
    }

    /// 前缀扫描（LIST）：利用 B-tree 有序性；租户前缀不同的键天然不会混入。
    pub fn list(&self, tenant_prefix: &MetaKey, limit: usize) -> Vec<&MetaKey> {
        self.view
            .range(tenant_prefix.clone()..)
            .take_while(|(k, _)| {
                k.tenant == tenant_prefix.tenant && k.key.starts_with(&tenant_prefix.key)
            })
            .take(limit)
            .map(|(k, _)| k)
            .collect()
    }

    /// checkpoint：日志合并进共享 B-tree 后截断（后台低优先级操作）。
    pub fn checkpoint(&mut self, shared: &mut SharedState) -> Result<()> {
        if self.epoch != shared.fenced_epoch {
            return Err(Error::Fenced { bucket: self.id });
        }
        shared.btree = self.view.clone();
        shared.journal.clear();
        Ok(())
    }

    pub fn entry_count(&self) -> usize {
        self.view.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnistor_core::TenantId;

    fn key(t: u32, k: &str) -> MetaKey {
        MetaKey::new(TenantId(t), k)
    }

    #[test]
    fn router_is_deterministic_and_spreads() {
        let r = BucketRouter::new(128);
        let k = key(1, "a/b/c");
        assert_eq!(r.route(&k), r.route(&k));
        // 大量键应覆盖大多数 Bucket（并行度）
        let mut seen = std::collections::HashSet::new();
        for i in 0..10_000 {
            seen.insert(r.route(&key(1, &format!("obj-{i}"))));
        }
        assert!(seen.len() > 100, "only {} buckets hit", seen.len());
    }

    #[test]
    fn put_get_delete_roundtrip() {
        let mut shared = SharedState::default();
        let mut b = BucketProcess::take_over(BucketId(1), CNodeId(1), &mut shared);
        b.put(&mut shared, key(1, "f1"), b"v1".to_vec()).unwrap();
        assert_eq!(b.get(&key(1, "f1")), Some(&b"v1".to_vec()));
        b.delete(&mut shared, key(1, "f1")).unwrap();
        assert_eq!(b.get(&key(1, "f1")), None);
    }

    #[test]
    fn takeover_replays_journal_zero_migration() {
        let mut shared = SharedState::default();
        let mut b1 = BucketProcess::take_over(BucketId(1), CNodeId(1), &mut shared);
        b1.put(&mut shared, key(1, "a"), b"1".to_vec()).unwrap();
        b1.checkpoint(&mut shared).unwrap();
        b1.put(&mut shared, key(1, "b"), b"2".to_vec()).unwrap(); // 仅在日志中
                                                                  // CNode-1 宕机，CNode-2 原地接管共享状态
        let b2 = BucketProcess::take_over(BucketId(1), CNodeId(2), &mut shared);
        assert_eq!(b2.get(&key(1, "a")), Some(&b"1".to_vec())); // 来自 checkpoint
        assert_eq!(b2.get(&key(1, "b")), Some(&b"2".to_vec())); // 来自日志重放
    }

    #[test]
    fn lease_fencing_blocks_zombie_writer() {
        let mut shared = SharedState::default();
        let mut old = BucketProcess::take_over(BucketId(1), CNodeId(1), &mut shared);
        // 新节点接管（推进任期）
        let mut new = BucketProcess::take_over(BucketId(1), CNodeId(2), &mut shared);
        // 旧进程复活尝试写 → 被围栏拒绝
        let err = old
            .put(&mut shared, key(1, "x"), b"stale".to_vec())
            .unwrap_err();
        assert_eq!(
            err,
            Error::Fenced {
                bucket: BucketId(1)
            }
        );
        // 新写者正常
        new.put(&mut shared, key(1, "x"), b"fresh".to_vec())
            .unwrap();
        assert_eq!(new.get(&key(1, "x")), Some(&b"fresh".to_vec()));
    }

    #[test]
    fn list_respects_tenant_boundary() {
        let mut shared = SharedState::default();
        let mut b = BucketProcess::take_over(BucketId(1), CNodeId(1), &mut shared);
        b.put(&mut shared, key(1, "dir/f1"), vec![]).unwrap();
        b.put(&mut shared, key(1, "dir/f2"), vec![]).unwrap();
        b.put(&mut shared, key(2, "dir/f3"), vec![]).unwrap(); // 另一租户同名路径
        let listed = b.list(&key(1, "dir/"), 100);
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|k| k.tenant == TenantId(1)));
    }
}
