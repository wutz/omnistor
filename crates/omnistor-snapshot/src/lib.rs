//! omnistor-snapshot: 元数据级 COW 快照与可写克隆。
//!
//! - 键值带世代号（generation），快照 = 冻结当前世代——瞬时、与数据量无关；
//! - 快照读 = 按 (key, gen ≤ snap_gen) 取最新版本；
//! - 克隆 = 从快照分叉新的可写头，共享分叉点前的全部版本；
//! - extent 引用计数：不再被任何世代引用才可回收；
//! - snap-to-object：增量导出快照到外部对象存储（自包含）。
//!
//! 对应文档：docs/features/snapshots.md

use std::collections::{BTreeMap, HashMap};

use omnistor_core::{Error, ExtentId, Result};

/// 世代号：命名空间范围内单调递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Generation(pub u64);

/// 快照 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotId(pub u32);

/// 可写头（live 或克隆）的 ID。0 = 原生 live 头。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HeadId(pub u32);

/// 一个键的一个版本：世代 + 指向的数据 extent（None = 删除墓碑）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    gen: Generation,
    /// 写入该版本的头（克隆分叉后各写各的世代链）。
    head: HeadId,
    extent: Option<ExtentId>,
}

/// 可写头：live 或克隆，各自持有当前世代与祖先链。
#[derive(Debug, Clone)]
struct Head {
    /// 当前可写世代。
    gen: Generation,
    /// 分叉祖先：本头在 fork_gen 之前的版本继承自 parent。
    parent: Option<(HeadId, Generation)>,
}

/// 快照：某头在某世代的只读视图。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub head: HeadId,
    pub gen: Generation,
    pub name: String,
    /// 已导出到对象存储的世代（增量导出的基线）。
    pub exported: bool,
}

/// 一个命名空间范围（目录树/桶/卷）的版本化键空间。
///
/// 真实实现中版本链内嵌在 Bucket 的 B-tree 值里；
/// 原型用独立结构表达同一语义。
#[derive(Debug)]
pub struct VersionedNamespace {
    /// key → 版本链（新在后）。
    versions: BTreeMap<Vec<u8>, Vec<Version>>,
    heads: HashMap<HeadId, Head>,
    snapshots: BTreeMap<SnapshotId, Snapshot>,
    /// extent → 引用计数（版本引用）。
    refcounts: HashMap<ExtentId, u32>,
    next_snapshot: u32,
    next_head: u32,
    next_gen: u64,
}

impl Default for VersionedNamespace {
    fn default() -> Self {
        Self::new()
    }
}

impl VersionedNamespace {
    pub fn new() -> Self {
        let mut heads = HashMap::new();
        heads.insert(
            HeadId(0),
            Head {
                gen: Generation(1),
                parent: None,
            },
        );
        Self {
            versions: BTreeMap::new(),
            heads,
            snapshots: BTreeMap::new(),
            refcounts: HashMap::new(),
            next_snapshot: 0,
            next_head: 1,
            next_gen: 2,
        }
    }

    pub fn live_head(&self) -> HeadId {
        HeadId(0)
    }

    fn head(&self, id: HeadId) -> Result<&Head> {
        self.heads
            .get(&id)
            .ok_or_else(|| Error::Invalid(format!("no head {}", id.0)))
    }

    /// 头的可见性判定：版本 (head=h, gen=g) 对头 `view` 在世代 `at` 是否可见。
    /// 沿分叉祖先链回溯：本头写的且 gen ≤ at 可见；祖先写的且 gen ≤ fork点 可见。
    fn visible(&self, v: &Version, view: HeadId, at: Generation) -> bool {
        let mut cur = view;
        let mut ceiling = at;
        loop {
            if v.head == cur && v.gen <= ceiling {
                return true;
            }
            match self.heads.get(&cur).and_then(|h| h.parent) {
                Some((parent, fork_gen)) => {
                    ceiling = std::cmp::min(ceiling, fork_gen);
                    cur = parent;
                }
                None => return false,
            }
        }
    }

    /// 在某头的某世代视图下解析键 → extent。
    fn resolve(&self, key: &[u8], view: HeadId, at: Generation) -> Option<ExtentId> {
        let chain = self.versions.get(key)?;
        chain
            .iter()
            .rev()
            .find(|v| self.visible(v, view, at))
            .and_then(|v| v.extent)
    }

    /// 写入（put）：在头的当前世代追加新版本。COW——绝不改旧版本。
    pub fn put(&mut self, head: HeadId, key: impl Into<Vec<u8>>, extent: ExtentId) -> Result<()> {
        let gen = self.head(head)?.gen;
        self.versions.entry(key.into()).or_default().push(Version {
            gen,
            head,
            extent: Some(extent),
        });
        *self.refcounts.entry(extent).or_insert(0) += 1;
        Ok(())
    }

    /// 删除：追加墓碑版本（旧世代的快照仍能看到旧值）。
    pub fn delete(&mut self, head: HeadId, key: impl Into<Vec<u8>>) -> Result<()> {
        let gen = self.head(head)?.gen;
        self.versions.entry(key.into()).or_default().push(Version {
            gen,
            head,
            extent: None,
        });
        Ok(())
    }

    /// live/克隆头的当前读。
    pub fn get(&self, head: HeadId, key: &[u8]) -> Result<Option<ExtentId>> {
        let h = self.head(head)?;
        Ok(self.resolve(key, head, h.gen))
    }

    /// 创建快照：冻结头的当前世代并推进可写头——一次元数据操作，瞬时。
    pub fn create_snapshot(&mut self, head: HeadId, name: &str) -> Result<SnapshotId> {
        let frozen = self.head(head)?.gen;
        let id = SnapshotId(self.next_snapshot);
        self.next_snapshot += 1;
        self.snapshots.insert(
            id,
            Snapshot {
                id,
                head,
                gen: frozen,
                name: name.into(),
                exported: false,
            },
        );
        // 推进可写头世代：之后的写不再进入被冻结的世代。
        let next = Generation(self.next_gen);
        self.next_gen += 1;
        self.heads.get_mut(&head).expect("head exists").gen = next;
        Ok(id)
    }

    fn snapshot(&self, id: SnapshotId) -> Result<&Snapshot> {
        self.snapshots
            .get(&id)
            .ok_or_else(|| Error::Invalid(format!("no snapshot {}", id.0)))
    }

    /// 快照视图读。
    pub fn get_at(&self, snap: SnapshotId, key: &[u8]) -> Result<Option<ExtentId>> {
        let s = self.snapshot(snap)?;
        Ok(self.resolve(key, s.head, s.gen))
    }

    /// 从快照分叉可写克隆：共享分叉点前全部版本，分叉后各写各的。
    pub fn clone_from(&mut self, snap: SnapshotId) -> Result<HeadId> {
        let s = self.snapshot(snap)?;
        let (src_head, fork_gen) = (s.head, s.gen);
        let id = HeadId(self.next_head);
        self.next_head += 1;
        let gen = Generation(self.next_gen);
        self.next_gen += 1;
        self.heads.insert(
            id,
            Head {
                gen,
                parent: Some((src_head, fork_gen)),
            },
        );
        Ok(id)
    }

    /// 删除快照。返回**因此不再被任何视图引用**、可交给分配器回收的 extent。
    pub fn delete_snapshot(&mut self, snap: SnapshotId) -> Result<Vec<ExtentId>> {
        self.snapshots
            .remove(&snap)
            .ok_or_else(|| Error::Invalid(format!("no snapshot {}", snap.0)))?;
        Ok(self.collect_garbage())
    }

    /// 回收：清除不被 live 头/克隆头/任何快照可见的版本，归还 extent 引用。
    fn collect_garbage(&mut self) -> Vec<ExtentId> {
        // 全部视图 = 各头的当前世代 + 各快照的冻结世代。
        let views: Vec<(HeadId, Generation)> = self
            .heads
            .iter()
            .map(|(id, h)| (*id, h.gen))
            .chain(self.snapshots.values().map(|s| (s.head, s.gen)))
            .collect();
        let mut freed = Vec::new();
        let keys: Vec<Vec<u8>> = self.versions.keys().cloned().collect();
        for key in keys {
            let chain = self.versions.get(&key).expect("key exists").clone();
            let keep: Vec<bool> = chain
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    // 版本 v 被保留，当且仅当存在某视图下 v 是该键的解析结果。
                    views.iter().any(|(view, at)| {
                        self.visible(v, *view, *at)
                            && chain
                                .iter()
                                .skip(i + 1)
                                .all(|later| !self.visible(later, *view, *at))
                    })
                })
                .collect();
            let entry = self.versions.get_mut(&key).expect("key exists");
            let mut idx = 0;
            entry.retain(|v| {
                let k = keep[idx];
                idx += 1;
                if !k {
                    if let Some(e) = v.extent {
                        if let Some(rc) = self.refcounts.get_mut(&e) {
                            *rc -= 1;
                            if *rc == 0 {
                                self.refcounts.remove(&e);
                                freed.push(e);
                            }
                        }
                    }
                }
                k
            });
            if entry.is_empty() {
                self.versions.remove(&key);
            }
        }
        freed
    }

    /// snap-to-object 导出：产出该快照视图引用的全部 extent；
    /// 若同头存在已导出的更早快照，做**增量**（只导出基线之后新写的 extent）。
    pub fn export_manifest(&mut self, snap: SnapshotId) -> Result<ExportManifest> {
        let s = self.snapshot(snap)?.clone();
        // 基线：同头、世代更早、已导出的最新快照。
        let baseline = self
            .snapshots
            .values()
            .filter(|b| b.head == s.head && b.gen < s.gen && b.exported)
            .max_by_key(|b| b.gen)
            .map(|b| b.gen);
        let mut extents = Vec::new();
        for (key, _) in self.versions.iter() {
            if let Some(e) = self.resolve(key, s.head, s.gen) {
                // 增量：基线视图下解析结果相同的键跳过。
                if let Some(base_gen) = baseline {
                    if self.resolve(key, s.head, base_gen) == Some(e) {
                        continue;
                    }
                }
                extents.push((key.clone(), e));
            }
        }
        self.snapshots
            .get_mut(&snap)
            .expect("snapshot exists")
            .exported = true;
        Ok(ExportManifest {
            snapshot: snap,
            incremental_from: baseline,
            entries: extents,
        })
    }

    /// extent 当前引用数（测试/监控）。
    pub fn refcount(&self, e: ExtentId) -> u32 {
        self.refcounts.get(&e).copied().unwrap_or(0)
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }
}

/// snap-to-object 导出清单：写到外部对象存储的自包含内容。
#[derive(Debug)]
pub struct ExportManifest {
    pub snapshot: SnapshotId,
    /// Some(gen) = 相对该基线世代的增量导出。
    pub incremental_from: Option<Generation>,
    /// (键, 数据 extent)。
    pub entries: Vec<(Vec<u8>, ExtentId)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const E1: ExtentId = ExtentId(101);
    const E2: ExtentId = ExtentId(102);
    const E3: ExtentId = ExtentId(103);

    #[test]
    fn snapshot_is_instant_frozen_view() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "a", E1).unwrap();
        let snap = ns.create_snapshot(live, "s1").unwrap();
        // 快照后覆盖写 + 删除
        ns.put(live, "a", E2).unwrap();
        ns.put(live, "b", E3).unwrap();
        // 快照视图冻结在旧世代
        assert_eq!(ns.get_at(snap, b"a").unwrap(), Some(E1));
        assert_eq!(ns.get_at(snap, b"b").unwrap(), None);
        // live 看到新值
        assert_eq!(ns.get(live, b"a").unwrap(), Some(E2));
        ns.delete(live, "a").unwrap();
        assert_eq!(ns.get(live, b"a").unwrap(), None);
        assert_eq!(ns.get_at(snap, b"a").unwrap(), Some(E1)); // 快照不受删除影响
    }

    #[test]
    fn writable_clone_shares_then_diverges() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "shared", E1).unwrap();
        let snap = ns.create_snapshot(live, "fork-point").unwrap();
        let clone = ns.clone_from(snap).unwrap();
        // 克隆共享分叉点前的数据（零拷贝）
        assert_eq!(ns.get(clone, b"shared").unwrap(), Some(E1));
        // 分叉后各写各的
        ns.put(clone, "shared", E2).unwrap();
        ns.put(live, "shared", E3).unwrap();
        assert_eq!(ns.get(clone, b"shared").unwrap(), Some(E2));
        assert_eq!(ns.get(live, b"shared").unwrap(), Some(E3));
        // 克隆分叉后 live 的新写对克隆不可见
        ns.put(live, "live-only", E3).unwrap();
        assert_eq!(ns.get(clone, b"live-only").unwrap(), None);
    }

    #[test]
    fn delete_snapshot_frees_unreferenced_extents_only() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "a", E1).unwrap();
        ns.put(live, "keep", E3).unwrap();
        let snap = ns.create_snapshot(live, "s").unwrap();
        ns.put(live, "a", E2).unwrap(); // E1 现在只被快照引用
        let freed = ns.delete_snapshot(snap).unwrap();
        assert_eq!(freed, vec![E1]); // E1 回收
        assert_eq!(ns.refcount(E3), 1); // live 仍引用的不回收
        assert_eq!(ns.get(live, b"a").unwrap(), Some(E2));
        assert_eq!(ns.get(live, b"keep").unwrap(), Some(E3));
    }

    #[test]
    fn clone_keeps_shared_extents_alive() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "a", E1).unwrap();
        let snap = ns.create_snapshot(live, "s").unwrap();
        let clone = ns.clone_from(snap).unwrap();
        ns.put(live, "a", E2).unwrap();
        // 删除快照：E1 仍被克隆视图依赖（经由分叉祖先链），不能回收
        let freed = ns.delete_snapshot(snap).unwrap();
        assert!(freed.is_empty());
        assert_eq!(ns.get(clone, b"a").unwrap(), Some(E1));
    }

    #[test]
    fn export_full_then_incremental() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "a", E1).unwrap();
        ns.put(live, "b", E2).unwrap();
        let s1 = ns.create_snapshot(live, "daily-1").unwrap();
        let m1 = ns.export_manifest(s1).unwrap();
        assert_eq!(m1.incremental_from, None); // 首次 = 全量
        assert_eq!(m1.entries.len(), 2);
        // 只改一个键，再快照
        ns.put(live, "b", E3).unwrap();
        let s2 = ns.create_snapshot(live, "daily-2").unwrap();
        let m2 = ns.export_manifest(s2).unwrap();
        assert!(m2.incremental_from.is_some()); // 相对 s1 的增量
        assert_eq!(m2.entries.len(), 1); // 只有 b 变了
        assert_eq!(m2.entries[0].0, b"b".to_vec());
        assert_eq!(m2.entries[0].1, E3);
    }

    #[test]
    fn many_snapshots_cost_is_diff_only() {
        let mut ns = VersionedNamespace::new();
        let live = ns.live_head();
        ns.put(live, "hot", E1).unwrap();
        for i in 0..100 {
            ns.create_snapshot(live, &format!("s{i}")).unwrap();
        }
        // 100 个快照没写入 → 版本链不增长（空间开销 = 变更量）
        assert_eq!(ns.versions.get(b"hot".as_slice()).unwrap().len(), 1);
        assert_eq!(ns.snapshot_count(), 100);
    }
}
