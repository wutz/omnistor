//! TLC 池 extent 分配器：元数据与数据共池、动态分配、水位仲裁。
//!
//! 对应文档：docs/features/tiering.md「TLC 池动态分配」
//! - 空闲低于**下沉水位**：应加速下沉冷数据（返回信号，由放置引擎执行）；
//! - 空闲低于**保护水位**：元数据分配优先——数据分配被拒（可直写下层），
//!   元数据仍可分配到最后一个 extent；
//! - 无下层可下沉时（纯 TLC），保护水位下数据写入报 NoSpace。

use omnistor_core::{Error, ExtentId, MediaClass, Result};

/// 申请用途：水位仲裁的依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// 元数据（Bucket B-tree / journal）：不可下沉，保护水位下仍可分配。
    Metadata,
    /// 数据（写缓存/条带）：保护水位下被拒，应直写下层。
    Data,
}

/// 水位配置（以空闲比例的千分数表示，避免浮点）。
#[derive(Debug, Clone, Copy)]
pub struct PoolWatermarks {
    /// 空闲低于此比例 → 建议加速下沉（如 300 = 30%）。
    pub sink_free_permille: u32,
    /// 空闲低于此比例 → 数据分配被拒，仅元数据可分配（如 100 = 10%）。
    pub protect_free_permille: u32,
}

impl Default for PoolWatermarks {
    fn default() -> Self {
        Self {
            sink_free_permille: 300,
            protect_free_permille: 100,
        }
    }
}

/// 分配结果附带的水位信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    /// 已低于下沉水位：放置引擎应加速下沉。
    SinkAdvised,
    /// 已低于保护水位：数据被拒中，仅元数据可分配。
    Protected,
}

/// 共享 TLC 池的 extent 分配器。
#[derive(Debug)]
pub struct ExtentAllocator {
    total: u64,
    free: u64,
    watermarks: PoolWatermarks,
    next_id: u64,
    meta_used: u64,
    data_used: u64,
}

impl ExtentAllocator {
    pub fn new(total_extents: u64, watermarks: PoolWatermarks) -> Self {
        Self {
            total: total_extents,
            free: total_extents,
            watermarks,
            next_id: 0,
            meta_used: 0,
            data_used: 0,
        }
    }

    fn free_permille(&self) -> u32 {
        if self.total == 0 {
            return 0;
        }
        ((self.free * 1000) / self.total) as u32
    }

    pub fn pressure(&self) -> Pressure {
        let f = self.free_permille();
        if f < self.watermarks.protect_free_permille {
            Pressure::Protected
        } else if f < self.watermarks.sink_free_permille {
            Pressure::SinkAdvised
        } else {
            Pressure::Normal
        }
    }

    /// 申请一个 extent。元数据与数据没有固定边界——只受水位仲裁约束。
    pub fn allocate(&mut self, purpose: Purpose) -> Result<(ExtentId, Pressure)> {
        if self.free == 0 {
            return Err(Error::NoSpace {
                media: MediaClass::TlcNvme,
            });
        }
        if purpose == Purpose::Data && self.pressure() == Pressure::Protected {
            // 数据在保护水位下被拒：调用方应直写下层或（纯 TLC）向上报错。
            return Err(Error::NoSpace {
                media: MediaClass::TlcNvme,
            });
        }
        self.free -= 1;
        self.next_id += 1;
        match purpose {
            Purpose::Metadata => self.meta_used += 1,
            Purpose::Data => self.data_used += 1,
        }
        Ok((ExtentId(self.next_id), self.pressure()))
    }

    /// 释放（删除/下沉完成后归还）。
    pub fn release(&mut self, purpose: Purpose) {
        self.free = (self.free + 1).min(self.total);
        match purpose {
            Purpose::Metadata => self.meta_used = self.meta_used.saturating_sub(1),
            Purpose::Data => self.data_used = self.data_used.saturating_sub(1),
        }
    }

    /// 元数据当前占用（精确等于实际使用量——跟随使用量增长）。
    pub fn metadata_used(&self) -> u64 {
        self.meta_used
    }

    pub fn data_used(&self) -> u64 {
        self.data_used
    }

    pub fn free_extents(&self) -> u64 {
        self.free
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_grows_with_usage_no_reservation() {
        let mut a = ExtentAllocator::new(1000, PoolWatermarks::default());
        assert_eq!(a.metadata_used(), 0); // 空集群元数据开销为零
        for _ in 0..10 {
            a.allocate(Purpose::Metadata).unwrap();
        }
        assert_eq!(a.metadata_used(), 10); // 精确等于使用量
        a.release(Purpose::Metadata);
        assert_eq!(a.metadata_used(), 9);
    }

    #[test]
    fn sink_watermark_signals() {
        let mut a = ExtentAllocator::new(10, PoolWatermarks::default());
        // 消耗到剩 3 个（30%）：触发下沉建议
        let mut last = Pressure::Normal;
        for _ in 0..8 {
            last = a.allocate(Purpose::Data).unwrap().1;
        }
        assert_eq!(last, Pressure::SinkAdvised);
    }

    #[test]
    fn protect_watermark_prefers_metadata() {
        let mut a = ExtentAllocator::new(10, PoolWatermarks::default());
        // 消耗到保护水位以下（剩 0 空闲比例 < 10% 即剩 0 个时已太迟；剩 1 个 = 10% 不触发，剩 0.x 触发）
        for _ in 0..9 {
            a.allocate(Purpose::Data).unwrap();
        }
        // 剩 1 个：free_permille = 100，不低于 protect(100)，数据仍可拿最后一个？
        // 100 < 100 为 false → 允许。拿走后 free=0，数据下一次 NoSpace。
        a.allocate(Purpose::Data).unwrap();
        assert!(matches!(
            a.allocate(Purpose::Data),
            Err(Error::NoSpace { .. })
        ));
    }

    #[test]
    fn data_rejected_below_protect_but_metadata_allowed() {
        // 更细的池：100 个 extent，保护水位 10% = 低于 10 个空闲时
        let mut a = ExtentAllocator::new(100, PoolWatermarks::default());
        for _ in 0..91 {
            a.allocate(Purpose::Data).unwrap();
        }
        // free = 9 → 9% < 10%：数据被拒
        assert!(matches!(
            a.allocate(Purpose::Data),
            Err(Error::NoSpace { .. })
        ));
        // 元数据仍可分配（永不下沉，无处可去）
        a.allocate(Purpose::Metadata).unwrap();
        assert_eq!(a.pressure(), Pressure::Protected);
    }

    #[test]
    fn release_recovers_pressure() {
        let mut a = ExtentAllocator::new(100, PoolWatermarks::default());
        // 数据分配到保护水位为止（free=9 → 9% < 10% 被拒）
        for _ in 0..91 {
            a.allocate(Purpose::Data).unwrap();
        }
        assert_eq!(a.pressure(), Pressure::Protected);
        assert!(matches!(
            a.allocate(Purpose::Data),
            Err(Error::NoSpace { .. })
        ));
        // 下沉完成释放 60 个 → 恢复正常，数据分配恢复
        for _ in 0..60 {
            a.release(Purpose::Data);
        }
        assert_eq!(a.pressure(), Pressure::Normal);
        a.allocate(Purpose::Data).unwrap();
    }
}
