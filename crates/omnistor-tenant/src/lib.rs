//! omnistor-tenant: 租户生命周期——注册、放置策略、每租户密钥（信封加密的 KEK 句柄）
//! 与删除时的密码学擦除（crypto-shredding）。
//!
//! 对应文档：docs/features/multitenancy.md

use std::collections::HashMap;

use omnistor_core::{Error, PoolId, Result, TenantId};

/// 租户数据放置策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// 逻辑隔离（默认）：共享存储池，键前缀 + 认证隔离。
    Shared,
    /// 物理隔离：绑定专属池，数据条带不与其他租户混布。
    DedicatedPool(PoolId),
}

/// 每租户 KEK 句柄。真实实现由 KMS 托管，这里保存密钥 ID 与代次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHandle {
    pub key_id: String,
    pub generation: u32,
}

#[derive(Debug, Clone)]
pub struct TenantSpec {
    pub name: String,
    pub placement: Placement,
}

#[derive(Debug)]
struct TenantEntry {
    spec: TenantSpec,
    kek: Option<KeyHandle>,
}

/// 租户注册表。
#[derive(Debug, Default)]
pub struct TenantRegistry {
    tenants: HashMap<TenantId, TenantEntry>,
    next_id: u32,
}

impl TenantRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建租户：名称集群内唯一，自动签发首代 KEK。
    pub fn create(&mut self, spec: TenantSpec) -> Result<TenantId> {
        if self.tenants.values().any(|t| t.spec.name == spec.name) {
            return Err(Error::Invalid(format!(
                "tenant name '{}' already exists",
                spec.name
            )));
        }
        self.next_id += 1;
        let id = TenantId(self.next_id);
        let kek = KeyHandle {
            key_id: format!("{}-kek", spec.name),
            generation: 1,
        };
        self.tenants.insert(
            id,
            TenantEntry {
                spec,
                kek: Some(kek),
            },
        );
        Ok(id)
    }

    pub fn get(&self, id: TenantId) -> Result<&TenantSpec> {
        self.tenants
            .get(&id)
            .map(|t| &t.spec)
            .ok_or(Error::UnknownTenant(id))
    }

    /// 数据路径取加密密钥：租户被删除（KEK 已销毁）后必然失败。
    pub fn key(&self, id: TenantId) -> Result<&KeyHandle> {
        self.tenants
            .get(&id)
            .ok_or(Error::UnknownTenant(id))?
            .kek
            .as_ref()
            .ok_or(Error::UnknownTenant(id))
    }

    /// 密钥轮换：代次 +1，旧数据由后台按新 KEK 重新包裹 DEK。
    pub fn rotate_key(&mut self, id: TenantId) -> Result<u32> {
        let entry = self.tenants.get_mut(&id).ok_or(Error::UnknownTenant(id))?;
        let kek = entry.kek.as_mut().ok_or(Error::UnknownTenant(id))?;
        kek.generation += 1;
        Ok(kek.generation)
    }

    /// 删除租户：先销毁 KEK（密码学擦除——所有 extent 的 DEK 立即不可解），
    /// 数据块由后台异步回收。
    pub fn delete(&mut self, id: TenantId) -> Result<()> {
        let entry = self.tenants.get_mut(&id).ok_or(Error::UnknownTenant(id))?;
        entry.kek = None; // crypto-shredding
        self.tenants.remove(&id);
        Ok(())
    }

    pub fn placement(&self, id: TenantId) -> Result<Placement> {
        Ok(self.get(id)?.placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> TenantSpec {
        TenantSpec {
            name: name.into(),
            placement: Placement::Shared,
        }
    }

    #[test]
    fn create_issues_unique_ids_and_keys() {
        let mut r = TenantRegistry::new();
        let a = r.create(spec("acme")).unwrap();
        let b = r.create(spec("globex")).unwrap();
        assert_ne!(a, b);
        assert_ne!(r.key(a).unwrap().key_id, r.key(b).unwrap().key_id);
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut r = TenantRegistry::new();
        r.create(spec("acme")).unwrap();
        assert!(r.create(spec("acme")).is_err());
    }

    #[test]
    fn delete_is_crypto_shredding() {
        let mut r = TenantRegistry::new();
        let id = r.create(spec("acme")).unwrap();
        assert!(r.key(id).is_ok());
        r.delete(id).unwrap();
        // 删除后密钥与租户都不可达
        assert_eq!(r.key(id).unwrap_err(), Error::UnknownTenant(id));
        assert_eq!(r.get(id).unwrap_err(), Error::UnknownTenant(id));
    }

    #[test]
    fn key_rotation_bumps_generation() {
        let mut r = TenantRegistry::new();
        let id = r.create(spec("acme")).unwrap();
        assert_eq!(r.key(id).unwrap().generation, 1);
        assert_eq!(r.rotate_key(id).unwrap(), 2);
        assert_eq!(r.key(id).unwrap().generation, 2);
    }

    #[test]
    fn dedicated_pool_placement() {
        let mut r = TenantRegistry::new();
        let id = r
            .create(TenantSpec {
                name: "bank".into(),
                placement: Placement::DedicatedPool(PoolId(7)),
            })
            .unwrap();
        assert_eq!(
            r.placement(id).unwrap(),
            Placement::DedicatedPool(PoolId(7))
        );
    }
}
