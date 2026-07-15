//! omnistor-console: 管理控台——REST API + 内嵌 Web 前端。
//!
//! 两种角色视图（docs/features/multitenancy.md 的两级管理模型）：
//! - 集群管理员：租户 CRUD、池水位总览、集群用量；
//! - 租户管理员：本租户用量/配额、写入模拟（观察 QoS/Quota 行为）。
//!
//! HTTP 层为纯 std 实现（TcpListener + 手写请求解析），无外部依赖。

use std::sync::Mutex;

use omnistor::Cluster;
use omnistor_core::{Error, MediaClass, PoolId, TenantId};
use omnistor_placement::PoolState;
use omnistor_qos::QosSpec;
use omnistor_quota::QuotaLimit;
use omnistor_tenant::Placement;

pub mod json;
pub mod server;

use json::{parse_flat, Json};

/// 控台应用：持有集群句柄，路由 REST 请求。
pub struct Console {
    cluster: Mutex<Cluster>,
}

/// 简化的 HTTP 响应。
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    fn json(status: u16, j: Json) -> Response {
        Response {
            status,
            content_type: "application/json",
            body: j.render(),
        }
    }

    fn ok(j: Json) -> Response {
        Response::json(200, j)
    }

    fn err(status: u16, msg: impl Into<String>) -> Response {
        Response::json(
            status,
            Json::Obj(vec![("error".into(), Json::Str(msg.into()))]),
        )
    }

    fn html(body: &'static str) -> Response {
        Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.into(),
        }
    }
}

fn error_response(e: &Error) -> Response {
    let status = match e {
        Error::Throttled { .. } => 429,
        Error::QuotaExceeded { .. } => 507,
        Error::NoSpace { .. } => 507,
        Error::UnknownTenant(_) => 404,
        Error::Invalid(_) => 400,
        _ => 500,
    };
    Response::err(status, e.to_string())
}

fn media_name(m: MediaClass) -> &'static str {
    match m {
        MediaClass::TlcNvme => "tlc-nvme",
        MediaClass::QlcNvme => "qlc-nvme",
        MediaClass::Hdd => "hdd",
        MediaClass::ExternalS3 => "external-s3",
    }
}

fn parse_media(s: &str) -> Option<MediaClass> {
    match s {
        "tlc-nvme" | "tlc" => Some(MediaClass::TlcNvme),
        "qlc-nvme" | "qlc" => Some(MediaClass::QlcNvme),
        "hdd" => Some(MediaClass::Hdd),
        _ => None,
    }
}

impl Console {
    pub fn new(cluster: Cluster) -> Self {
        Self {
            cluster: Mutex::new(cluster),
        }
    }

    /// 带演示数据的控台（池 + 两个租户 + 少量对象）。
    pub fn with_demo_data() -> Self {
        let mut c = Cluster::new(4096, 1_000_000);
        for (id, media, cap, used) in [
            (1u32, MediaClass::TlcNvme, 500_000u64, 120_000u64),
            (2, MediaClass::TlcNvme, 500_000, 60_000),
            (3, MediaClass::QlcNvme, 2_000_000, 400_000),
            (4, MediaClass::Hdd, 8_000_000, 2_400_000),
        ] {
            c.add_pool(PoolState {
                id: PoolId(id),
                media,
                capacity: cap,
                used,
                load_headroom_permille: 700,
                dedicated_to: None,
            });
        }
        let qos = QosSpec {
            metadata_iops: 50_000,
            data_iops: 100_000,
            data_bw_bytes: 10 << 30,
            burst_multiple: 2,
        };
        let acme = c
            .create_tenant(
                "acme",
                QuotaLimit {
                    capacity_bytes: Some(1 << 50),
                    object_count: Some(10_000_000_000),
                },
                qos,
                Placement::Shared,
            )
            .expect("demo tenant");
        c.create_tenant(
            "globex",
            QuotaLimit {
                capacity_bytes: Some(1 << 45),
                object_count: Some(1_000_000_000),
            },
            qos,
            Placement::Shared,
        )
        .expect("demo tenant");
        for i in 0..64 {
            let _ = c.put_object(acme, "photos", &format!("demo/img-{i}.jpg"), 4096 + i * 17);
        }
        Self::new(c)
    }

    /// 路由入口：method + path (+query) + body → Response。
    pub fn handle(&self, method: &str, path: &str, body: &str) -> Response {
        let (path, query) = match path.split_once('?') {
            Some((p, q)) => (p, q),
            None => (path, ""),
        };
        let segs: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        match (method, segs.as_slice()) {
            ("GET", []) | ("GET", ["index.html"]) => Response::html(UI_HTML),
            ("GET", ["api", "cluster"]) => self.cluster_summary(),
            ("GET", ["api", "pools"]) => self.list_pools(),
            ("POST", ["api", "pools"]) => self.create_pool(body),
            ("GET", ["api", "tenants"]) => self.list_tenants(),
            ("POST", ["api", "tenants"]) => self.create_tenant(body),
            ("GET", ["api", "tenants", id]) => self.tenant_detail(id),
            ("DELETE", ["api", "tenants", id]) => self.delete_tenant(id),
            ("POST", ["api", "tenants", id, "objects"]) => self.put_objects(id, body),
            ("POST", ["api", "tenants", id, "quota"]) => self.set_scope_quota(id, body),
            ("POST", ["api", "tick"]) => self.tick(query),
            _ => Response::err(404, format!("no route: {method} {path}")),
        }
    }

    fn cluster_summary(&self) -> Response {
        let c = self.cluster.lock().unwrap();
        let pools = c.placement.pools();
        let (cap, used): (u64, u64) = pools
            .iter()
            .fold((0, 0), |(c0, u0), p| (c0 + p.capacity, u0 + p.used));
        Response::ok(Json::Obj(vec![
            ("tenants".into(), Json::num(c.tenants.list().len() as u32)),
            ("pools".into(), Json::num(pools.len() as u32)),
            ("capacity_extents".into(), Json::num(cap as f64)),
            ("used_extents".into(), Json::num(used as f64)),
            (
                "tlc_free_extents".into(),
                Json::num(c.tlc_extents.free_extents() as f64),
            ),
            (
                "tlc_metadata_used".into(),
                Json::num(c.tlc_extents.metadata_used() as f64),
            ),
            (
                "tlc_data_used".into(),
                Json::num(c.tlc_extents.data_used() as f64),
            ),
            (
                "active_buckets".into(),
                Json::num(c.active_buckets() as u32),
            ),
        ]))
    }

    fn list_pools(&self) -> Response {
        let c = self.cluster.lock().unwrap();
        let items = c
            .placement
            .pools()
            .into_iter()
            .map(|p| {
                Json::Obj(vec![
                    ("id".into(), Json::num(p.id.0)),
                    ("media".into(), Json::str(media_name(p.media))),
                    ("capacity".into(), Json::num(p.capacity as f64)),
                    ("used".into(), Json::num(p.used as f64)),
                    (
                        "load_headroom_permille".into(),
                        Json::num(p.load_headroom_permille),
                    ),
                    (
                        "dedicated_to".into(),
                        p.dedicated_to.map_or(Json::Null, |t| Json::num(t.0)),
                    ),
                ])
            })
            .collect();
        Response::ok(Json::Arr(items))
    }

    fn create_pool(&self, body: &str) -> Response {
        let f = parse_flat(body);
        let (Some(id), Some(media), Some(capacity)) = (
            f.get("id").and_then(|v| v.parse::<u32>().ok()),
            f.get("media").and_then(|v| parse_media(v)),
            f.get("capacity").and_then(|v| v.parse::<u64>().ok()),
        ) else {
            return Response::err(400, "need id, media(tlc|qlc|hdd), capacity");
        };
        let mut c = self.cluster.lock().unwrap();
        c.add_pool(PoolState {
            id: PoolId(id),
            media,
            capacity,
            used: 0,
            load_headroom_permille: 1000,
            dedicated_to: f
                .get("dedicated_to")
                .and_then(|v| v.parse().ok())
                .map(TenantId),
        });
        Response::ok(Json::Obj(vec![("id".into(), Json::num(id))]))
    }

    fn list_tenants(&self) -> Response {
        let c = self.cluster.lock().unwrap();
        let items = c
            .tenants
            .list()
            .into_iter()
            .map(|(id, spec)| {
                let usage = c.quotas.tenant_usage(id).unwrap_or_default();
                Json::Obj(vec![
                    ("id".into(), Json::num(id.0)),
                    ("name".into(), Json::str(spec.name.clone())),
                    (
                        "placement".into(),
                        match spec.placement {
                            Placement::Shared => Json::str("shared"),
                            Placement::DedicatedPool(p) => Json::str(format!("dedicated:{}", p.0)),
                        },
                    ),
                    ("used_bytes".into(), Json::num(usage.bytes as f64)),
                    ("objects".into(), Json::num(usage.objects as f64)),
                ])
            })
            .collect();
        Response::ok(Json::Arr(items))
    }

    fn create_tenant(&self, body: &str) -> Response {
        let f = parse_flat(body);
        let Some(name) = f.get("name").filter(|n| !n.is_empty()) else {
            return Response::err(400, "need name");
        };
        let quota = QuotaLimit {
            capacity_bytes: f.get("capacity_bytes").and_then(|v| v.parse().ok()),
            object_count: f.get("object_count").and_then(|v| v.parse().ok()),
        };
        let qos = QosSpec {
            metadata_iops: f
                .get("metadata_iops")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50_000),
            data_iops: f
                .get("data_iops")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100_000),
            data_bw_bytes: f
                .get("data_bw_bytes")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 << 30),
            burst_multiple: 2,
        };
        let placement = match f.get("dedicated_pool").and_then(|v| v.parse::<u32>().ok()) {
            Some(p) => Placement::DedicatedPool(PoolId(p)),
            None => Placement::Shared,
        };
        let mut c = self.cluster.lock().unwrap();
        match c.create_tenant(name, quota, qos, placement) {
            Ok(id) => Response::json(
                201,
                Json::Obj(vec![
                    ("id".into(), Json::num(id.0)),
                    ("name".into(), Json::str(name.clone())),
                ]),
            ),
            Err(e) => error_response(&e),
        }
    }

    fn tenant_detail(&self, id: &str) -> Response {
        let Some(tid) = id.parse::<u32>().ok().map(TenantId) else {
            return Response::err(400, "bad tenant id");
        };
        let c = self.cluster.lock().unwrap();
        match c.tenants.get(tid) {
            Ok(spec) => {
                let usage = c.quotas.tenant_usage(tid).unwrap_or_default();
                let key = c.tenants.key(tid).ok();
                Response::ok(Json::Obj(vec![
                    ("id".into(), Json::num(tid.0)),
                    ("name".into(), Json::str(spec.name.clone())),
                    ("used_bytes".into(), Json::num(usage.bytes as f64)),
                    ("objects".into(), Json::num(usage.objects as f64)),
                    (
                        "key".into(),
                        key.map_or(Json::Null, |k| {
                            Json::Obj(vec![
                                ("key_id".into(), Json::str(k.key_id.clone())),
                                ("generation".into(), Json::num(k.generation)),
                            ])
                        }),
                    ),
                ]))
            }
            Err(e) => error_response(&e),
        }
    }

    fn delete_tenant(&self, id: &str) -> Response {
        let Some(tid) = id.parse::<u32>().ok().map(TenantId) else {
            return Response::err(400, "bad tenant id");
        };
        let mut c = self.cluster.lock().unwrap();
        match c.delete_tenant(tid) {
            Ok(()) => Response::ok(Json::Obj(vec![("deleted".into(), Json::num(tid.0))])),
            Err(e) => error_response(&e),
        }
    }

    /// 租户管理员：写入模拟（观察 QoS 限流 / Quota 拒绝 / Bucket 散布）。
    fn put_objects(&self, id: &str, body: &str) -> Response {
        let Some(tid) = id.parse::<u32>().ok().map(TenantId) else {
            return Response::err(400, "bad tenant id");
        };
        let f = parse_flat(body);
        let count: u32 = f
            .get("count")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
            .min(100_000);
        let size: u64 = f
            .get("size_bytes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096);
        let scope = f.get("scope").cloned().unwrap_or_else(|| "default".into());
        let prefix = f.get("prefix").cloned().unwrap_or_else(|| "obj".into());
        let mut c = self.cluster.lock().unwrap();
        let base = c.quotas.tenant_usage(tid).map(|u| u.objects).unwrap_or(0);
        let (mut ok, mut throttled, mut quota_exceeded, mut no_space) = (0u32, 0u32, 0u32, 0u32);
        let mut first_error = None;
        for i in 0..count {
            match c.put_object(
                tid,
                &scope,
                &format!("{prefix}/{}", base + u64::from(i)),
                size,
            ) {
                Ok(()) => ok += 1,
                Err(e) => {
                    match &e {
                        Error::Throttled { .. } => throttled += 1,
                        Error::QuotaExceeded { .. } => quota_exceeded += 1,
                        Error::NoSpace { .. } => no_space += 1,
                        Error::UnknownTenant(_) => return error_response(&e),
                        _ => {}
                    }
                    if first_error.is_none() {
                        first_error = Some(e.to_string());
                    }
                }
            }
        }
        Response::ok(Json::Obj(vec![
            ("requested".into(), Json::num(count)),
            ("ok".into(), Json::num(ok)),
            ("throttled".into(), Json::num(throttled)),
            ("quota_exceeded".into(), Json::num(quota_exceeded)),
            ("no_space".into(), Json::num(no_space)),
            (
                "first_error".into(),
                first_error.map_or(Json::Null, Json::Str),
            ),
            (
                "active_buckets".into(),
                Json::num(c.active_buckets() as u32),
            ),
        ]))
    }

    /// 租户管理员：设子配额。
    fn set_scope_quota(&self, id: &str, body: &str) -> Response {
        let Some(tid) = id.parse::<u32>().ok().map(TenantId) else {
            return Response::err(400, "bad tenant id");
        };
        let f = parse_flat(body);
        let Some(scope) = f.get("scope").filter(|s| !s.is_empty()) else {
            return Response::err(400, "need scope");
        };
        let limit = QuotaLimit {
            capacity_bytes: f.get("capacity_bytes").and_then(|v| v.parse().ok()),
            object_count: f.get("object_count").and_then(|v| v.parse().ok()),
        };
        let mut c = self.cluster.lock().unwrap();
        match c.quotas.set_scope_limit(tid, scope, limit) {
            Ok(()) => Response::ok(Json::Obj(vec![("scope".into(), Json::str(scope.clone()))])),
            Err(e) => error_response(&e),
        }
    }

    /// 时间推进（补充 QoS 令牌）：`/api/tick?n=5`。
    fn tick(&self, query: &str) -> Response {
        let n: u32 = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("n="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
            .min(10_000);
        let mut c = self.cluster.lock().unwrap();
        for _ in 0..n {
            c.tick();
        }
        Response::ok(Json::Obj(vec![("ticked".into(), Json::num(n))]))
    }
}

/// 内嵌单页前端。
pub const UI_HTML: &str = include_str!("ui.html");

#[cfg(test)]
mod tests {
    use super::*;

    fn console() -> Console {
        Console::with_demo_data()
    }

    fn get(c: &Console, path: &str) -> (u16, String) {
        let r = c.handle("GET", path, "");
        (r.status, r.body)
    }

    fn post(c: &Console, path: &str, body: &str) -> (u16, String) {
        let r = c.handle("POST", path, body);
        (r.status, r.body)
    }

    #[test]
    fn serves_ui_and_summary() {
        let c = console();
        let (status, body) = get(&c, "/");
        assert_eq!(status, 200);
        assert!(body.contains("OmniStor"));
        let (status, body) = get(&c, "/api/cluster");
        assert_eq!(status, 200);
        assert!(body.contains("\"tenants\":2"));
        assert!(body.contains("\"pools\":4"));
    }

    #[test]
    fn tenant_crud_via_api() {
        let c = console();
        let (status, body) = post(
            &c,
            "/api/tenants",
            r#"{"name": "initech", "capacity_bytes": 1048576, "object_count": 100}"#,
        );
        assert_eq!(status, 201, "{body}");
        assert!(body.contains("initech"));
        // 重名被拒
        let (status, _) = post(&c, "/api/tenants", r#"{"name": "initech"}"#);
        assert_eq!(status, 400);
        // 详情含密钥代次
        let (status, body) = get(&c, "/api/tenants/3");
        assert_eq!(status, 200);
        assert!(body.contains("initech-kek"));
        // 删除即密码学擦除
        let r = c.handle("DELETE", "/api/tenants/3", "");
        assert_eq!(r.status, 200);
        let (status, _) = get(&c, "/api/tenants/3");
        assert_eq!(status, 404);
    }

    #[test]
    fn write_simulation_reports_quota_rejects() {
        let c = console();
        post(
            &c,
            "/api/tenants",
            r#"{"name": "tiny", "capacity_bytes": 8192, "object_count": 100}"#,
        );
        // 8 KiB 配额，写 10 × 1 KiB → 8 成功 2 配额拒绝
        let (status, body) = post(
            &c,
            "/api/tenants/3/objects",
            r#"{"count": 10, "size_bytes": 1024}"#,
        );
        assert_eq!(status, 200);
        assert!(body.contains("\"ok\":8"), "{body}");
        assert!(body.contains("\"quota_exceeded\":2"), "{body}");
    }

    #[test]
    fn write_simulation_reports_throttling_and_tick_recovers() {
        let c = console();
        let (_, body) = post(
            &c,
            "/api/tenants",
            r#"{"name": "slow", "metadata_iops": 10, "data_iops": 10000, "data_bw_bytes": 1000000000}"#,
        );
        assert!(body.contains("slow"));
        let (_, body) = post(
            &c,
            "/api/tenants/3/objects",
            r#"{"count": 100, "size_bytes": 1}"#,
        );
        assert!(body.contains("\"throttled\""));
        assert!(
            !body.contains("\"throttled\":0"),
            "expected throttling: {body}"
        );
        // tick 后恢复
        post(&c, "/api/tick?n=10", "");
        let (_, body) = post(
            &c,
            "/api/tenants/3/objects",
            r#"{"count": 5, "size_bytes": 1}"#,
        );
        assert!(body.contains("\"ok\":5"), "{body}");
    }

    #[test]
    fn scope_quota_enforced() {
        let c = console();
        post(
            &c,
            "/api/tenants/1/quota",
            r#"{"scope": "photos", "capacity_bytes": 1024}"#,
        );
        let (_, body) = post(
            &c,
            "/api/tenants/1/objects",
            r#"{"count": 3, "size_bytes": 512, "scope": "photos", "prefix": "p"}"#,
        );
        // 1 KiB 子配额：2 个 512B 成功，第 3 个被拒
        assert!(body.contains("\"ok\":2"), "{body}");
        assert!(body.contains("\"quota_exceeded\":1"), "{body}");
    }

    #[test]
    fn pool_create_and_list() {
        let c = console();
        let (status, _) = post(
            &c,
            "/api/pools",
            r#"{"id": 9, "media": "hdd", "capacity": 100000}"#,
        );
        assert_eq!(status, 200);
        let (_, body) = get(&c, "/api/pools");
        assert!(body.contains("\"id\":9"));
        // 参数不全报 400
        let (status, _) = post(&c, "/api/pools", r#"{"media": "hdd"}"#);
        assert_eq!(status, 400);
    }

    #[test]
    fn unknown_route_is_404() {
        let c = console();
        let (status, _) = get(&c, "/api/nope");
        assert_eq!(status, 404);
    }
}
