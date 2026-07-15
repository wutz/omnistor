//! omnistor-quota: 两级配额（租户总配额 → 租户内桶/卷/目录子配额）。
//! 硬约束：超限拒绝写入；写入与释放对称记账。
//!
//! 对应文档：docs/features/quota.md、docs/features/multitenancy.md

use std::collections::HashMap;

use omnistor_core::{Error, Result, TenantId};

/// 配额限额。`None` 表示该维度不限。
#[derive(Debug, Clone, Copy, Default)]
pub struct QuotaLimit {
    pub capacity_bytes: Option<u64>,
    pub object_count: Option<u64>,
}

/// 使用量计数器。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub objects: u64,
}

#[derive(Debug)]
struct Account {
    limit: QuotaLimit,
    usage: Usage,
}

impl Account {
    fn check(&self, add_bytes: u64, add_objects: u64, scope: &str) -> Result<()> {
        if let Some(cap) = self.limit.capacity_bytes {
            if self.usage.bytes.saturating_add(add_bytes) > cap {
                return Err(Error::QuotaExceeded {
                    scope: format!("{scope}:capacity"),
                });
            }
        }
        if let Some(cnt) = self.limit.object_count {
            if self.usage.objects.saturating_add(add_objects) > cnt {
                return Err(Error::QuotaExceeded {
                    scope: format!("{scope}:objects"),
                });
            }
        }
        Ok(())
    }

    fn commit(&mut self, add_bytes: u64, add_objects: u64) {
        self.usage.bytes += add_bytes;
        self.usage.objects += add_objects;
    }

    fn release(&mut self, bytes: u64, objects: u64) {
        self.usage.bytes = self.usage.bytes.saturating_sub(bytes);
        self.usage.objects = self.usage.objects.saturating_sub(objects);
    }
}

/// 两级配额管理器：租户级 + 租户内命名子配额（桶/卷/目录统一抽象为 scope 名）。
#[derive(Debug, Default)]
pub struct QuotaManager {
    tenants: HashMap<TenantId, Account>,
    scopes: HashMap<(TenantId, String), Account>,
}

impl QuotaManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 集群管理员：设置租户总配额。
    pub fn set_tenant_limit(&mut self, tenant: TenantId, limit: QuotaLimit) {
        self.tenants
            .entry(tenant)
            .and_modify(|a| a.limit = limit)
            .or_insert(Account {
                limit,
                usage: Usage::default(),
            });
    }

    /// 租户管理员：在租户内设子配额（桶/卷/目录）。
    pub fn set_scope_limit(
        &mut self,
        tenant: TenantId,
        scope: &str,
        limit: QuotaLimit,
    ) -> Result<()> {
        if !self.tenants.contains_key(&tenant) {
            return Err(Error::UnknownTenant(tenant));
        }
        self.scopes
            .entry((tenant, scope.to_string()))
            .and_modify(|a| a.limit = limit)
            .or_insert(Account {
                limit,
                usage: Usage::default(),
            });
        Ok(())
    }

    /// 写入前校验并记账：两级同时校验，任一超限即拒绝（原子：校验全过才提交）。
    pub fn charge(
        &mut self,
        tenant: TenantId,
        scope: &str,
        bytes: u64,
        objects: u64,
    ) -> Result<()> {
        let t = self
            .tenants
            .get(&tenant)
            .ok_or(Error::UnknownTenant(tenant))?;
        t.check(bytes, objects, "tenant")?;
        if let Some(s) = self.scopes.get(&(tenant, scope.to_string())) {
            s.check(bytes, objects, scope)?;
        }
        // 校验全部通过后提交
        self.tenants
            .get_mut(&tenant)
            .unwrap()
            .commit(bytes, objects);
        if let Some(s) = self.scopes.get_mut(&(tenant, scope.to_string())) {
            s.commit(bytes, objects);
        }
        Ok(())
    }

    /// 删除/下沉释放时对称扣减。
    pub fn release(&mut self, tenant: TenantId, scope: &str, bytes: u64, objects: u64) {
        if let Some(t) = self.tenants.get_mut(&tenant) {
            t.release(bytes, objects);
        }
        if let Some(s) = self.scopes.get_mut(&(tenant, scope.to_string())) {
            s.release(bytes, objects);
        }
    }

    pub fn tenant_usage(&self, tenant: TenantId) -> Option<Usage> {
        self.tenants.get(&tenant).map(|a| a.usage)
    }

    pub fn remove_tenant(&mut self, tenant: TenantId) {
        self.tenants.remove(&tenant);
        self.scopes.retain(|(t, _), _| *t != tenant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: TenantId = TenantId(1);

    fn mgr() -> QuotaManager {
        let mut m = QuotaManager::new();
        m.set_tenant_limit(
            T,
            QuotaLimit {
                capacity_bytes: Some(1000),
                object_count: Some(10),
            },
        );
        m
    }

    #[test]
    fn tenant_hard_limit_rejects() {
        let mut m = mgr();
        m.charge(T, "b1", 900, 5).unwrap();
        let err = m.charge(T, "b1", 200, 1).unwrap_err();
        assert!(matches!(err, Error::QuotaExceeded { .. }));
        // 未超限维度不受影响：字节还剩 100
        m.charge(T, "b1", 100, 1).unwrap();
    }

    #[test]
    fn scope_sublimit_within_tenant() {
        let mut m = mgr();
        m.set_scope_limit(
            T,
            "bucket-a",
            QuotaLimit {
                capacity_bytes: Some(100),
                object_count: None,
            },
        )
        .unwrap();
        m.charge(T, "bucket-a", 100, 1).unwrap();
        // 子配额满，租户还有余量 → 仍拒绝
        let err = m.charge(T, "bucket-a", 1, 1).unwrap_err();
        assert_eq!(
            err,
            Error::QuotaExceeded {
                scope: "bucket-a:capacity".into()
            }
        );
        // 其他 scope 不受影响
        m.charge(T, "bucket-b", 500, 1).unwrap();
    }

    #[test]
    fn release_returns_budget() {
        let mut m = mgr();
        m.charge(T, "b", 1000, 10).unwrap();
        m.release(T, "b", 600, 4);
        assert_eq!(
            m.tenant_usage(T).unwrap(),
            Usage {
                bytes: 400,
                objects: 6
            }
        );
        m.charge(T, "b", 600, 4).unwrap();
    }

    #[test]
    fn failed_check_commits_nothing() {
        let mut m = mgr();
        m.set_scope_limit(
            T,
            "s",
            QuotaLimit {
                capacity_bytes: Some(50),
                object_count: None,
            },
        )
        .unwrap();
        // scope 超限失败时，租户级也不应记账
        let before = m.tenant_usage(T).unwrap();
        assert!(m.charge(T, "s", 60, 1).is_err());
        assert_eq!(m.tenant_usage(T).unwrap(), before);
    }

    #[test]
    fn unknown_tenant_rejected() {
        let mut m = QuotaManager::new();
        assert_eq!(
            m.charge(TenantId(9), "x", 1, 1).unwrap_err(),
            Error::UnknownTenant(TenantId(9))
        );
    }
}
