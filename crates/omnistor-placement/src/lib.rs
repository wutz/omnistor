//! omnistor-placement: 硬件分池、池间均衡与温度分层。
//!
//! - 池 = 一组同构 SNode，media class 标签；纠删与故障域以池为边界；
//! - 写入选池：同 media class 内按容量水位 + 负载加权评分；
//! - 后台再平衡：池间水位差超阈值时迁移存量 extent；
//! - 均衡不跨 media class；跨类移动只由分层（温度）驱动；
//! - 租户可绑定专属池（物理隔离）。
//!
//! 对应文档：docs/architecture/pools.md、docs/features/tiering.md

use std::collections::HashMap;

use omnistor_core::{Error, MediaClass, PoolId, Result, TenantId};

/// 池的实时状态（由监控周期刷新）。
#[derive(Debug, Clone)]
pub struct PoolState {
    pub id: PoolId,
    pub media: MediaClass,
    pub capacity: u64,
    pub used: u64,
    /// 负载余量 0..=1000（千分数，越大越闲）。
    pub load_headroom_permille: u32,
    /// 专属租户（None = 共享池）。
    pub dedicated_to: Option<TenantId>,
}

impl PoolState {
    fn used_permille(&self) -> u32 {
        if self.capacity == 0 {
            return 1000;
        }
        ((self.used * 1000) / self.capacity) as u32
    }

    /// 放置评分：容量与负载双低者胜（分高者优先）。
    fn score(&self) -> u64 {
        let capacity_headroom = 1000 - u64::from(self.used_permille());
        let load_headroom = u64::from(self.load_headroom_permille);
        // 容量与负载等权；实际系统可用实测画像校准。
        capacity_headroom + load_headroom
    }
}

/// 再平衡建议：把若干 extent 从高水位池迁往低水位池。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebalanceMove {
    pub from: PoolId,
    pub to: PoolId,
    pub extents: u64,
}

/// 放置引擎。
#[derive(Debug, Default)]
pub struct PlacementEngine {
    pools: HashMap<PoolId, PoolState>,
    /// 池间水位差触发阈值（千分数，默认 150 = 15%）。
    watermark_diff_permille: u32,
}

impl PlacementEngine {
    pub fn new(watermark_diff_permille: u32) -> Self {
        Self {
            pools: HashMap::new(),
            watermark_diff_permille,
        }
    }

    pub fn upsert_pool(&mut self, state: PoolState) {
        self.pools.insert(state.id, state);
    }

    pub fn pool(&self, id: PoolId) -> Option<&PoolState> {
        self.pools.get(&id)
    }

    /// 枚举全部池（监控/控台视图）。
    pub fn pools(&self) -> Vec<&PoolState> {
        let mut v: Vec<_> = self.pools.values().collect();
        v.sort_by_key(|p| p.id);
        v
    }

    /// 写入选池：在指定 media class 的候选池中选评分最高者。
    ///
    /// - 共享租户只用共享池；
    /// - 绑定专属池的租户只用自己的池（物理隔离）。
    pub fn select_pool(
        &self,
        media: MediaClass,
        tenant: TenantId,
        dedicated: Option<PoolId>,
    ) -> Result<PoolId> {
        let candidates: Vec<&PoolState> = self
            .pools
            .values()
            .filter(|p| p.media == media)
            .filter(|p| match dedicated {
                Some(pool) => p.id == pool,
                None => p.dedicated_to.is_none() || p.dedicated_to == Some(tenant),
            })
            .filter(|p| p.used < p.capacity)
            .collect();
        candidates
            .into_iter()
            .max_by_key(|p| (p.score(), std::cmp::Reverse(p.id)))
            .map(|p| p.id)
            .ok_or(Error::NoSpace { media })
    }

    /// 记账：extent 落到某池。
    pub fn commit(&mut self, pool: PoolId, extents: u64) -> Result<()> {
        let p = self
            .pools
            .get_mut(&pool)
            .ok_or_else(|| Error::Invalid(format!("no pool {}", pool.0)))?;
        p.used = (p.used + extents).min(p.capacity);
        Ok(())
    }

    /// 后台再平衡：对每个 media class，若最高/最低水位差超阈值，
    /// 生成从高到低的迁移建议。**绝不跨 media class**，专属池不参与。
    pub fn plan_rebalance(&self) -> Vec<RebalanceMove> {
        let mut moves = Vec::new();
        let classes = [MediaClass::TlcNvme, MediaClass::QlcNvme, MediaClass::Hdd];
        for media in classes {
            let mut shared: Vec<&PoolState> = self
                .pools
                .values()
                .filter(|p| p.media == media && p.dedicated_to.is_none() && p.capacity > 0)
                .collect();
            if shared.len() < 2 {
                continue;
            }
            shared.sort_by_key(|p| p.used_permille());
            let low = shared[0];
            let high = shared[shared.len() - 1];
            let diff = high.used_permille().saturating_sub(low.used_permille());
            if diff > self.watermark_diff_permille {
                // 迁移量：拉平到两池平均水位所需（按低池容量折算），保守起见取一半。
                let target_permille =
                    (u64::from(high.used_permille()) + u64::from(low.used_permille())) / 2;
                let extents =
                    (u64::from(high.used_permille()) - target_permille) * high.capacity / 1000 / 2;
                if extents > 0 {
                    moves.push(RebalanceMove {
                        from: high.id,
                        to: low.id,
                        extents,
                    });
                }
            }
        }
        moves
    }

    /// 温度下沉：为某 media class 上的冷数据选择下一层的目标池。
    /// 跳过未配置的层（如无 QLC 直接 TLC → HDD），到 ExternalS3 为止。
    pub fn select_sink_target(
        &self,
        from: MediaClass,
        tenant: TenantId,
    ) -> Option<(MediaClass, Option<PoolId>)> {
        let mut next = from.colder();
        while let Some(media) = next {
            if media == MediaClass::ExternalS3 {
                // 外部对象存储不是本地池，恒可用（容量近乎无限）。
                return Some((MediaClass::ExternalS3, None));
            }
            if let Ok(pool) = self.select_pool(media, tenant, None) {
                return Some((media, Some(pool)));
            }
            next = media.colder();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: TenantId = TenantId(1);

    fn pool(id: u32, media: MediaClass, capacity: u64, used: u64) -> PoolState {
        PoolState {
            id: PoolId(id),
            media,
            capacity,
            used,
            load_headroom_permille: 500,
            dedicated_to: None,
        }
    }

    fn engine() -> PlacementEngine {
        let mut e = PlacementEngine::new(150);
        e.upsert_pool(pool(1, MediaClass::TlcNvme, 1000, 100)); // tlc-gen1, 10% used
        e.upsert_pool(pool(2, MediaClass::TlcNvme, 1000, 800)); // tlc-gen2, 80% used
        e.upsert_pool(pool(3, MediaClass::Hdd, 10_000, 1000));
        e
    }

    #[test]
    fn select_prefers_low_watermark_pool() {
        let e = engine();
        assert_eq!(
            e.select_pool(MediaClass::TlcNvme, T, None).unwrap(),
            PoolId(1)
        );
    }

    #[test]
    fn full_media_class_reports_no_space() {
        let mut e = PlacementEngine::new(150);
        e.upsert_pool(pool(1, MediaClass::TlcNvme, 100, 100)); // 满
        assert_eq!(
            e.select_pool(MediaClass::TlcNvme, T, None).unwrap_err(),
            Error::NoSpace {
                media: MediaClass::TlcNvme
            }
        );
    }

    #[test]
    fn rebalance_triggers_on_watermark_diff_within_class_only() {
        let e = engine();
        let moves = e.plan_rebalance();
        // TLC 池差 70% > 15% → 触发；且 from/to 都是 TLC 池，绝不涉及 HDD
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].from, PoolId(2));
        assert_eq!(moves[0].to, PoolId(1));
        assert!(moves[0].extents > 0);
    }

    #[test]
    fn no_rebalance_when_balanced() {
        let mut e = PlacementEngine::new(150);
        e.upsert_pool(pool(1, MediaClass::TlcNvme, 1000, 500));
        e.upsert_pool(pool(2, MediaClass::TlcNvme, 1000, 550)); // 差 5%
        assert!(e.plan_rebalance().is_empty());
    }

    #[test]
    fn dedicated_pool_isolation() {
        let mut e = engine();
        let bank = TenantId(9);
        let mut p = pool(4, MediaClass::TlcNvme, 1000, 0); // 空池，评分最高
        p.dedicated_to = Some(bank);
        e.upsert_pool(p);
        // 共享租户选不到专属池（哪怕它最空）
        assert_eq!(
            e.select_pool(MediaClass::TlcNvme, T, None).unwrap(),
            PoolId(1)
        );
        // 专属租户只落自己的池
        assert_eq!(
            e.select_pool(MediaClass::TlcNvme, bank, Some(PoolId(4)))
                .unwrap(),
            PoolId(4)
        );
        // 专属池不参与共享再平衡
        let moves = e.plan_rebalance();
        assert!(moves
            .iter()
            .all(|m| m.from != PoolId(4) && m.to != PoolId(4)));
    }

    #[test]
    fn sink_skips_missing_tier_and_ends_at_s3() {
        let e = engine(); // 有 TLC 和 HDD，无 QLC
        let (media, pool) = e.select_sink_target(MediaClass::TlcNvme, T).unwrap();
        assert_eq!(media, MediaClass::Hdd); // 跳过 QLC
        assert_eq!(pool, Some(PoolId(3)));
        // HDD 再往下 → 外部 S3
        let (media, pool) = e.select_sink_target(MediaClass::Hdd, T).unwrap();
        assert_eq!(media, MediaClass::ExternalS3);
        assert_eq!(pool, None);
    }

    #[test]
    fn commit_moves_watermark() {
        let mut e = engine();
        e.commit(PoolId(1), 850).unwrap(); // pool1: 95% used
                                           // 现在 pool2 (80%) 更空
        assert_eq!(
            e.select_pool(MediaClass::TlcNvme, T, None).unwrap(),
            PoolId(2)
        );
    }
}
