//! omnistor-qos: 三维 QoS 令牌桶（Metadata IOPS / Data IOPS / Data BW），
//! 支持分片令牌与周期重平衡、后台低优先级令牌。
//!
//! 对应文档：docs/features/qos.md

use omnistor_core::{Error, Result};

/// QoS 维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    MetadataIops,
    DataIops,
    DataBw,
}

impl Dimension {
    fn name(self) -> &'static str {
        match self {
            Dimension::MetadataIops => "metadata_iops",
            Dimension::DataIops => "data_iops",
            Dimension::DataBw => "data_bw",
        }
    }
}

/// 请求优先级：后台操作（分层迁移、再平衡、回收）永远让位于前台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Foreground,
    Background,
}

/// 单个令牌桶。时间由调用方注入（tick 驱动），便于测试且无系统时钟依赖。
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// 每 tick 补充的令牌数（持续速率）。
    rate_per_tick: u64,
    /// 桶容量 = rate × burst 倍数。
    capacity: u64,
    tokens: u64,
    /// 后台请求只允许动用低于此水位的余量：保证前台永远有优先余量。
    background_floor: u64,
}

impl TokenBucket {
    pub fn new(rate_per_tick: u64, burst_multiple: u64) -> Self {
        let capacity = rate_per_tick.saturating_mul(burst_multiple.max(1));
        Self {
            rate_per_tick,
            capacity,
            tokens: capacity,
            // 后台最多用到容量的一半，剩余一半留给前台突发。
            background_floor: capacity / 2,
        }
    }

    /// 推进一个 tick，补充令牌。
    pub fn tick(&mut self) {
        self.tokens = (self.tokens + self.rate_per_tick).min(self.capacity);
    }

    pub fn try_acquire(&mut self, amount: u64, prio: Priority) -> bool {
        let floor = match prio {
            Priority::Foreground => 0,
            Priority::Background => self.background_floor,
        };
        if self.tokens >= floor + amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }

    pub fn available(&self) -> u64 {
        self.tokens
    }
}

/// 三维 QoS 配置。
#[derive(Debug, Clone, Copy)]
pub struct QosSpec {
    pub metadata_iops: u64,
    pub data_iops: u64,
    pub data_bw_bytes: u64,
    pub burst_multiple: u64,
}

/// 一个记账实体（租户/桶/卷）的三维令牌桶组。
#[derive(Debug, Clone)]
pub struct QosEntity {
    meta: TokenBucket,
    data_iops: TokenBucket,
    data_bw: TokenBucket,
}

impl QosEntity {
    pub fn new(spec: QosSpec) -> Self {
        Self {
            meta: TokenBucket::new(spec.metadata_iops, spec.burst_multiple),
            data_iops: TokenBucket::new(spec.data_iops, spec.burst_multiple),
            data_bw: TokenBucket::new(spec.data_bw_bytes, spec.burst_multiple),
        }
    }

    pub fn tick(&mut self) {
        self.meta.tick();
        self.data_iops.tick();
        self.data_bw.tick();
    }

    fn bucket(&mut self, dim: Dimension) -> &mut TokenBucket {
        match dim {
            Dimension::MetadataIops => &mut self.meta,
            Dimension::DataIops => &mut self.data_iops,
            Dimension::DataBw => &mut self.data_bw,
        }
    }

    pub fn acquire(&mut self, dim: Dimension, amount: u64, prio: Priority) -> Result<()> {
        if self.bucket(dim).try_acquire(amount, prio) {
            Ok(())
        } else {
            Err(Error::Throttled {
                dimension: dim.name(),
            })
        }
    }
}

/// 分片令牌桶：单实体的额度切成 N 片分布到各执行点
/// （前端 CNode 或元数据 Bucket），周期性按用量重平衡。
#[derive(Debug)]
pub struct ShardedQos {
    shards: Vec<QosEntity>,
    spec: QosSpec,
    /// 各分片自上次重平衡以来的使用量（用于按需分配权重）。
    usage: Vec<u64>,
}

impl ShardedQos {
    pub fn new(spec: QosSpec, shard_count: usize) -> Self {
        let n = shard_count.max(1) as u64;
        let per_shard = QosSpec {
            metadata_iops: (spec.metadata_iops / n).max(1),
            data_iops: (spec.data_iops / n).max(1),
            data_bw_bytes: (spec.data_bw_bytes / n).max(1),
            burst_multiple: spec.burst_multiple,
        };
        Self {
            shards: (0..shard_count.max(1))
                .map(|_| QosEntity::new(per_shard))
                .collect(),
            spec,
            usage: vec![0; shard_count.max(1)],
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn acquire(
        &mut self,
        shard: usize,
        dim: Dimension,
        amount: u64,
        prio: Priority,
    ) -> Result<()> {
        let idx = shard % self.shards.len();
        self.shards[idx].acquire(dim, amount, prio)?;
        self.usage[idx] = self.usage[idx].saturating_add(amount);
        Ok(())
    }

    pub fn tick(&mut self) {
        for s in &mut self.shards {
            s.tick();
        }
    }

    /// 周期重平衡：按各分片近期用量占比重新切分总额度，热分片拿更大份额。
    pub fn rebalance(&mut self) {
        let total_usage: u64 = self.usage.iter().sum();
        let n = self.shards.len() as u64;
        for (i, shard) in self.shards.iter_mut().enumerate() {
            // 无历史用量时均分；有用量时按占比（保底 1/2n 防饿死）。
            let weight = if total_usage == 0 {
                1.0 / n as f64
            } else {
                (self.usage[i] as f64 / total_usage as f64).max(0.5 / n as f64)
            };
            let scaled = |v: u64| ((v as f64 * weight) as u64).max(1);
            *shard = QosEntity::new(QosSpec {
                metadata_iops: scaled(self.spec.metadata_iops),
                data_iops: scaled(self.spec.data_iops),
                data_bw_bytes: scaled(self.spec.data_bw_bytes),
                burst_multiple: self.spec.burst_multiple,
            });
        }
        self.usage.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> QosSpec {
        QosSpec {
            metadata_iops: 100,
            data_iops: 200,
            data_bw_bytes: 1000,
            burst_multiple: 2,
        }
    }

    #[test]
    fn foreground_throttles_at_capacity() {
        let mut e = QosEntity::new(spec());
        // 容量 = 100 × 2 = 200
        for _ in 0..200 {
            e.acquire(Dimension::MetadataIops, 1, Priority::Foreground)
                .unwrap();
        }
        let err = e
            .acquire(Dimension::MetadataIops, 1, Priority::Foreground)
            .unwrap_err();
        assert_eq!(
            err,
            Error::Throttled {
                dimension: "metadata_iops"
            }
        );
        // tick 补充后恢复
        e.tick();
        e.acquire(Dimension::MetadataIops, 100, Priority::Foreground)
            .unwrap();
    }

    #[test]
    fn background_leaves_headroom_for_foreground() {
        let mut e = QosEntity::new(spec());
        // 后台最多消耗到 floor（容量一半 = 100），之后被拒
        let mut granted = 0;
        while e
            .acquire(Dimension::DataIops, 1, Priority::Background)
            .is_ok()
        {
            granted += 1;
        }
        assert_eq!(granted, 200); // data_iops 容量 400，floor 200
                                  // 前台仍能拿到剩余的一半
        e.acquire(Dimension::DataIops, 200, Priority::Foreground)
            .unwrap();
    }

    #[test]
    fn dimensions_are_independent() {
        let mut e = QosEntity::new(spec());
        while e
            .acquire(Dimension::MetadataIops, 1, Priority::Foreground)
            .is_ok()
        {}
        // 元数据被限流，数据维度不受影响
        e.acquire(Dimension::DataBw, 500, Priority::Foreground)
            .unwrap();
    }

    #[test]
    fn sharded_rebalance_shifts_budget_to_hot_shard() {
        let mut sq = ShardedQos::new(spec(), 4);
        // 分片 0 打满，其余闲置
        while sq
            .acquire(0, Dimension::DataIops, 1, Priority::Foreground)
            .is_ok()
        {}
        let hot_before = sq.shards[0].data_iops.available();
        sq.rebalance();
        let hot_after = sq.shards[0].data_iops.available();
        let idle_after = sq.shards[1].data_iops.available();
        assert!(hot_after > hot_before, "hot shard should regain budget");
        assert!(
            hot_after > idle_after,
            "hot shard should out-budget idle shards"
        );
    }
}
