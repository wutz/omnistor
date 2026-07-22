//! omnistor-protection: 分布式 D+P 纠删——条带放置、降级读、并行重建。
//!
//! - 条带 = D 数据 + P 校验分片，分别落在 D+P 个不同故障域（SNode）；
//! - 写永远写新位置（无读-改-写），本 crate 只管放置与条带索引；
//! - SNode 故障 → 从条带索引列出受影响条带（不扫盘），
//!   按风险（丢失分片数）排序，均匀分派给所有 CNode 并行重建；
//! - 条带严格在池内构建，重建流量不出池。
//!
//! 对应文档：docs/architecture/data-protection.md

use std::collections::{BTreeMap, BTreeSet, HashMap};

use omnistor_core::{CNodeId, Error, PoolId, Result, SNodeId};

/// 条带 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StripeId(pub u64);

/// 纠删方案：D 个数据分片 + P 个校验分片。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectionScheme {
    pub data: u8,
    pub parity: u8,
}

impl ProtectionScheme {
    /// D = 4..=16，P = 2 或 4（docs/architecture/data-protection.md）。
    pub fn new(data: u8, parity: u8) -> Result<Self> {
        if !(4..=16).contains(&data) {
            return Err(Error::Invalid(format!("data shards {data} not in 4..=16")));
        }
        if parity != 2 && parity != 4 {
            return Err(Error::Invalid(format!("parity shards {parity} not 2 or 4")));
        }
        Ok(Self { data, parity })
    }

    pub fn width(&self) -> usize {
        usize::from(self.data) + usize::from(self.parity)
    }

    /// 空间效率（千分数）：D/(D+P)。条带越宽开销越低。
    pub fn efficiency_permille(&self) -> u32 {
        u32::from(self.data) * 1000 / (u32::from(self.data) + u32::from(self.parity))
    }
}

/// 条带的健康状态（依据当前故障集合推导，非持久化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StripeHealth {
    /// 全部分片健在。
    Healthy,
    /// 丢失 1..=P 个分片：可降级读（重构），需要重建。
    Degraded { lost: u8 },
    /// 丢失 > P 个分片：数据丢失。
    Lost,
}

/// 池内的条带放置器 + 条带索引。
///
/// 故障域粒度 = SNode（池内可再细分机架，原型从简）。
#[derive(Debug)]
pub struct StripeManager {
    pool: PoolId,
    scheme: ProtectionScheme,
    /// 池内故障域及其已放置分片数（磨损/容量均衡的替身指标）。
    domains: BTreeMap<SNodeId, u64>,
    /// 当前故障中的域。
    failed: BTreeSet<SNodeId>,
    /// 条带 → 成员域。写新位置：每个条带一经写定不原地改。
    stripes: HashMap<StripeId, Vec<SNodeId>>,
    /// 反向索引：域 → 其上有分片的条带。重建时免扫全量。
    by_domain: HashMap<SNodeId, BTreeSet<StripeId>>,
    next_stripe: u64,
}

impl StripeManager {
    pub fn new(pool: PoolId, scheme: ProtectionScheme) -> Self {
        Self {
            pool,
            scheme,
            domains: BTreeMap::new(),
            failed: BTreeSet::new(),
            stripes: HashMap::new(),
            by_domain: HashMap::new(),
            next_stripe: 0,
        }
    }

    pub fn pool(&self) -> PoolId {
        self.pool
    }

    pub fn scheme(&self) -> ProtectionScheme {
        self.scheme
    }

    pub fn add_domain(&mut self, snode: SNodeId) {
        self.domains.entry(snode).or_insert(0);
    }

    fn healthy_domains(&self) -> impl Iterator<Item = (&SNodeId, &u64)> {
        self.domains
            .iter()
            .filter(|(id, _)| !self.failed.contains(id))
    }

    /// 放置一个新条带：选 D+P 个**互不相同**的健康故障域，
    /// 已放置分片最少者优先（容量/磨损均衡），同值按 ID 轮转确定性打散。
    pub fn place_stripe(&mut self) -> Result<(StripeId, Vec<SNodeId>)> {
        let width = self.scheme.width();
        let mut candidates: Vec<(SNodeId, u64)> =
            self.healthy_domains().map(|(id, n)| (*id, *n)).collect();
        if candidates.len() < width {
            return Err(Error::Invalid(format!(
                "pool {} has {} healthy fault domains, stripe needs {width}",
                self.pool.0,
                candidates.len()
            )));
        }
        // 均衡：分片数少者优先；同值时用 (id + 条带号) 轮转避免总选小 ID。
        let rotate = self.next_stripe;
        candidates.sort_by_key(|(id, n)| (*n, u64::from(id.0).wrapping_add(rotate) % 997, id.0));
        let members: Vec<SNodeId> = candidates
            .into_iter()
            .take(width)
            .map(|(id, _)| id)
            .collect();

        let id = StripeId(self.next_stripe);
        self.next_stripe += 1;
        for m in &members {
            *self.domains.get_mut(m).expect("member exists") += 1;
            self.by_domain.entry(*m).or_default().insert(id);
        }
        self.stripes.insert(id, members.clone());
        Ok((id, members))
    }

    /// 回收条带（覆盖写指针切换后 / 快照引用归零后）。
    pub fn release_stripe(&mut self, id: StripeId) -> Result<()> {
        let members = self
            .stripes
            .remove(&id)
            .ok_or_else(|| Error::Invalid(format!("no stripe {}", id.0)))?;
        for m in members {
            if let Some(n) = self.domains.get_mut(&m) {
                *n = n.saturating_sub(1);
            }
            if let Some(set) = self.by_domain.get_mut(&m) {
                set.remove(&id);
            }
        }
        Ok(())
    }

    /// 条带健康状态：丢失分片数 ≤ P 可降级读，> P 丢数据。
    pub fn health(&self, id: StripeId) -> Result<StripeHealth> {
        let members = self
            .stripes
            .get(&id)
            .ok_or_else(|| Error::Invalid(format!("no stripe {}", id.0)))?;
        let lost = members.iter().filter(|m| self.failed.contains(m)).count() as u8;
        Ok(if lost == 0 {
            StripeHealth::Healthy
        } else if lost <= self.scheme.parity {
            StripeHealth::Degraded { lost }
        } else {
            StripeHealth::Lost
        })
    }

    /// 降级读：给出条带内可用于重构的存活成员（≥ D 个即可读）。
    pub fn readable_members(&self, id: StripeId) -> Result<Vec<SNodeId>> {
        let members = self
            .stripes
            .get(&id)
            .ok_or_else(|| Error::Invalid(format!("no stripe {}", id.0)))?;
        let alive: Vec<SNodeId> = members
            .iter()
            .filter(|m| !self.failed.contains(m))
            .copied()
            .collect();
        if alive.len() < usize::from(self.scheme.data) {
            return Err(Error::Invalid(format!(
                "stripe {} lost beyond parity",
                id.0
            )));
        }
        Ok(alive)
    }

    /// 标记故障域。返回受影响条带数（直接来自反向索引——不扫盘）。
    pub fn fail_domain(&mut self, snode: SNodeId) -> usize {
        self.failed.insert(snode);
        self.by_domain.get(&snode).map_or(0, BTreeSet::len)
    }

    /// 故障域修复归来：其上旧分片应已被重建替代（rebuild 完成后调用）。
    pub fn recover_domain(&mut self, snode: SNodeId) {
        self.failed.remove(&snode);
    }

    /// 规划并行重建：
    /// - 只包含受影响条带（反向索引直查）；
    /// - 风险排序：丢分片多的条带先修（先脱离"再坏一个就丢"的状态）；
    /// - 均匀分派给参与的 CNode——参与者越多每个越轻，重建越快；
    /// - 目标域 = 池内健康且不与该条带现存分片同域。
    pub fn plan_rebuild(&self, cnodes: &[CNodeId]) -> Result<Vec<RebuildTask>> {
        if cnodes.is_empty() {
            return Err(Error::Invalid("no cnodes to rebuild".into()));
        }
        // 收集受影响条带（可能多个域同时故障，用集合去重）。
        let mut affected: BTreeSet<StripeId> = BTreeSet::new();
        for domain in &self.failed {
            if let Some(set) = self.by_domain.get(domain) {
                affected.extend(set.iter().copied());
            }
        }
        // 风险排序：丢失分片数降序，同级按条带 ID。
        let mut ranked: Vec<(u8, StripeId)> = Vec::new();
        for id in affected {
            match self.health(id)? {
                StripeHealth::Degraded { lost } => ranked.push((lost, id)),
                StripeHealth::Lost => {
                    return Err(Error::Invalid(format!(
                        "stripe {} lost beyond parity, rebuild impossible",
                        id.0
                    )))
                }
                StripeHealth::Healthy => {}
            }
        }
        ranked.sort_by_key(|(lost, id)| (std::cmp::Reverse(*lost), id.0));

        let mut tasks = Vec::with_capacity(ranked.len());
        for (i, (lost, id)) in ranked.into_iter().enumerate() {
            let members = &self.stripes[&id];
            let sources = self.readable_members(id)?;
            // 目标域：健康、且不与条带现存分片同域；仍按最少分片优先。
            let mut spares: Vec<(SNodeId, u64)> = self
                .healthy_domains()
                .filter(|(id, _)| !members.contains(id))
                .map(|(id, n)| (*id, *n))
                .collect();
            if spares.len() < usize::from(lost) {
                return Err(Error::Invalid(format!(
                    "pool {} lacks spare fault domains for stripe {}",
                    self.pool.0, id.0
                )));
            }
            spares.sort_by_key(|(id, n)| (*n, id.0));
            let targets: Vec<SNodeId> = spares
                .into_iter()
                .take(usize::from(lost))
                .map(|(id, _)| id)
                .collect();
            tasks.push(RebuildTask {
                stripe: id,
                lost,
                assigned_to: cnodes[i % cnodes.len()],
                read_from: sources,
                write_to: targets,
            });
        }
        Ok(tasks)
    }

    /// 执行一个重建任务：把重建出的分片记到目标域（条带成员替换）。
    pub fn apply_rebuild(&mut self, task: &RebuildTask) -> Result<()> {
        let members = self
            .stripes
            .get_mut(&task.stripe)
            .ok_or_else(|| Error::Invalid(format!("no stripe {}", task.stripe.0)))?;
        let mut targets = task.write_to.iter();
        for slot in members.iter_mut() {
            if self.failed.contains(slot) {
                let new = *targets
                    .next()
                    .ok_or_else(|| Error::Invalid("rebuild targets exhausted".into()))?;
                // 旧域索引移除、新域记账。
                if let Some(set) = self.by_domain.get_mut(slot) {
                    set.remove(&task.stripe);
                }
                if let Some(n) = self.domains.get_mut(slot) {
                    *n = n.saturating_sub(1);
                }
                *slot = new;
                *self.domains.get_mut(&new).expect("target exists") += 1;
                self.by_domain.entry(new).or_default().insert(task.stripe);
            }
        }
        Ok(())
    }

    pub fn stripe_count(&self) -> usize {
        self.stripes.len()
    }

    /// 某域上的分片数（监控）。
    pub fn shards_on(&self, snode: SNodeId) -> u64 {
        self.domains.get(&snode).copied().unwrap_or(0)
    }
}

/// 一个重建任务：一个 CNode 负责一个条带——读 D 个存活分片、重构、写目标域。
#[derive(Debug, Clone)]
pub struct RebuildTask {
    pub stripe: StripeId,
    /// 丢失分片数（风险等级）。
    pub lost: u8,
    pub assigned_to: CNodeId,
    pub read_from: Vec<SNodeId>,
    pub write_to: Vec<SNodeId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(domains: u32) -> StripeManager {
        let mut m = StripeManager::new(PoolId(1), ProtectionScheme::new(8, 2).unwrap());
        for i in 0..domains {
            m.add_domain(SNodeId(i));
        }
        m
    }

    #[test]
    fn scheme_validation_and_efficiency() {
        assert!(ProtectionScheme::new(3, 2).is_err()); // D 太窄
        assert!(ProtectionScheme::new(8, 3).is_err()); // P 只能 2/4
        let wide = ProtectionScheme::new(16, 2).unwrap();
        let narrow = ProtectionScheme::new(4, 2).unwrap();
        // 条带越宽空间效率越高
        assert!(wide.efficiency_permille() > narrow.efficiency_permille());
        assert_eq!(wide.efficiency_permille(), 888);
    }

    #[test]
    fn stripe_members_never_share_fault_domain() {
        let mut m = manager(12);
        for _ in 0..100 {
            let (_, members) = m.place_stripe().unwrap();
            let unique: BTreeSet<_> = members.iter().collect();
            assert_eq!(unique.len(), 10, "duplicate fault domain in stripe");
        }
    }

    #[test]
    fn placement_balances_across_domains() {
        let mut m = manager(20);
        for _ in 0..200 {
            m.place_stripe().unwrap();
        }
        // 200 条带 × 10 分片 / 20 域 = 平均 100，均衡放置应贴近平均
        let counts: Vec<u64> = (0..20).map(|i| m.shards_on(SNodeId(i))).collect();
        let (min, max) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
        assert!(max - min <= 1, "imbalance: min={min} max={max}");
    }

    #[test]
    fn too_few_domains_is_rejected() {
        let mut m = manager(9); // 需要 10
        assert!(m.place_stripe().is_err());
    }

    #[test]
    fn degraded_read_within_parity() {
        let mut m = manager(12);
        let (id, members) = m.place_stripe().unwrap();
        assert_eq!(m.health(id).unwrap(), StripeHealth::Healthy);
        // 坏 2 个（= P）：降级但可读
        m.fail_domain(members[0]);
        m.fail_domain(members[1]);
        assert_eq!(m.health(id).unwrap(), StripeHealth::Degraded { lost: 2 });
        let alive = m.readable_members(id).unwrap();
        assert_eq!(alive.len(), 8); // 恰好 D 个
                                    // 坏第 3 个：超出校验能力
        m.fail_domain(members[2]);
        assert_eq!(m.health(id).unwrap(), StripeHealth::Lost);
        assert!(m.readable_members(id).is_err());
    }

    #[test]
    fn rebuild_lists_only_affected_stripes() {
        let mut m = manager(14);
        let mut placed = Vec::new();
        for _ in 0..50 {
            placed.push(m.place_stripe().unwrap());
        }
        let victim = SNodeId(0);
        let affected = m.fail_domain(victim);
        let expected = placed
            .iter()
            .filter(|(_, mem)| mem.contains(&victim))
            .count();
        assert_eq!(affected, expected);
        let tasks = m.plan_rebuild(&[CNodeId(1)]).unwrap();
        assert_eq!(tasks.len(), expected); // 只修受影响条带，不扫全量
    }

    #[test]
    fn rebuild_spreads_over_cnodes_and_ranks_by_risk() {
        let mut m = manager(16);
        for _ in 0..60 {
            m.place_stripe().unwrap();
        }
        // 两个域同时故障：部分条带丢 2 片（高风险），部分丢 1 片
        m.fail_domain(SNodeId(0));
        m.fail_domain(SNodeId(1));
        let cnodes: Vec<CNodeId> = (0..4).map(CNodeId).collect();
        let tasks = m.plan_rebuild(&cnodes).unwrap();
        assert!(!tasks.is_empty());
        // 风险排序：lost 单调不增
        for w in tasks.windows(2) {
            assert!(w[0].lost >= w[1].lost, "high-risk stripes must come first");
        }
        // 并行分派：4 个 CNode 都有份（任务足够多时）
        let assignees: BTreeSet<_> = tasks.iter().map(|t| t.assigned_to).collect();
        assert_eq!(assignees.len(), 4);
        // 目标域不与条带现存成员重叠、不在故障集
        for t in &tasks {
            for target in &t.write_to {
                assert!(!t.read_from.contains(target));
                assert!(target != &SNodeId(0) && target != &SNodeId(1));
            }
        }
    }

    #[test]
    fn apply_rebuild_restores_health() {
        let mut m = manager(12);
        let (id, members) = m.place_stripe().unwrap();
        let victim = members[3];
        m.fail_domain(victim);
        assert_eq!(m.health(id).unwrap(), StripeHealth::Degraded { lost: 1 });
        let tasks = m.plan_rebuild(&[CNodeId(7)]).unwrap();
        assert_eq!(tasks.len(), 1);
        m.apply_rebuild(&tasks[0]).unwrap();
        // 条带恢复健康；故障域上不再有该条带的分片
        assert_eq!(m.health(id).unwrap(), StripeHealth::Healthy);
        assert_eq!(m.shards_on(victim), 0);
        // 修复归来的域作为空节点重新参与放置
        m.recover_domain(victim);
        assert_eq!(m.health(id).unwrap(), StripeHealth::Healthy);
    }

    #[test]
    fn release_stripe_returns_capacity() {
        let mut m = manager(12);
        let (id, members) = m.place_stripe().unwrap();
        m.release_stripe(id).unwrap();
        for mem in members {
            assert_eq!(m.shards_on(mem), 0);
        }
        assert_eq!(m.stripe_count(), 0);
        // 释放后故障不再牵动它
        assert_eq!(m.fail_domain(SNodeId(0)), 0);
    }

    #[test]
    fn bigger_cluster_rebuilds_with_less_work_per_cnode() {
        // 相同故障规模下，CNode 越多每个分到的任务越少——重建越快。
        let mut m = manager(20);
        for _ in 0..100 {
            m.place_stripe().unwrap();
        }
        m.fail_domain(SNodeId(0));
        let few: Vec<CNodeId> = (0..2).map(CNodeId).collect();
        let many: Vec<CNodeId> = (0..10).map(CNodeId).collect();
        let t_few = m.plan_rebuild(&few).unwrap();
        let t_many = m.plan_rebuild(&many).unwrap();
        let max_load = |tasks: &[RebuildTask], n: usize| {
            (0..n)
                .map(|c| {
                    tasks
                        .iter()
                        .filter(|t| t.assigned_to == CNodeId(c as u32))
                        .count()
                })
                .max()
                .unwrap()
        };
        assert!(max_load(&t_many, 10) < max_load(&t_few, 2));
    }
}
