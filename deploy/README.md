# deploy

部署与编排。

## 计划

- `baremetal/` — CNode/SNode 节点初始化脚本与角色调度配置
- `compose/` — 单机/小规模开发环境（docker-compose）
- `k8s/` — Kubernetes 部署清单（Helm chart）
- `ansible/` — 物理机批量部署 playbook

## 部署形态

- **开发**：compose 单节点全角色，验证功能链路。
- **生产**：通用 x86 服务器集群（CNode + SNode），角色由调度器分配，DASE 共享池。
