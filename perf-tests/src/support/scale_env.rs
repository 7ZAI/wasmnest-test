//! 宿主规模边界环境。
//!
//! 两种被测物，分别回答两个问题：
//! - **N 个 wasm 实例**（同一 bench 组件、不同属主）：宿主能同时**运行**多少个
//!   应用（激活、命令、停用、RSS）。合成夹具只保留桥接原语，避免业务数据增长
//!   把规模曲线搅浑；
//! - **N 个 TS-only 清单**（无 wasm 实例）：宿主 → 前端的注册表投影
//!   （`plugin_list_loaded` 的同源数据）在多大数量级下仍然可序列化。这一层不做
//!   激活，专门隔离「清单体积」与「运行时实例」两个变量。
//!
//! 两者都走生产同形的 [`crate::support::host::assemble`]，临时目录隔离；不触碰
//! 平台仓 `resources/`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use wasmnest_desktop_lib::wasm_core::PluginHost;

use crate::support::{bridge, host, repo_paths};

/// 一次规模测点的宿主环境
pub struct ScaleEnv {
    pub host: Arc<PluginHost>,
    /// 按激活顺序排列的插件 id（= 目录名 = manifest id）
    pub ids: Vec<String>,
    temp_root: PathBuf,
}

impl ScaleEnv {
    /// 依次停用（反向；后激活的先退），失败只记录不中断——清理路径不能吞掉
    /// 前面的测点结果。
    pub async fn deactivate_all(&self, active: &[String]) -> Vec<String> {
        let mut failures = Vec::new();
        for id in active.iter().rev() {
            if let Err(e) = self.host.deactivate_plugin(id, false).await {
                failures.push(format!("{id}: {e}"));
            }
        }
        failures
    }

    /// 删临时目录（调用方负责先停用；本方法不碰宿主）
    pub fn cleanup(self) {
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

/// 建 N 个 wasm 实例目录并装配宿主（扫描期即实例化，激活由场景控制）。
pub async fn build_wasm_instances(count: usize) -> anyhow::Result<ScaleEnv> {
    anyhow::ensure!(count > 0, "规模边界至少需要 1 个实例");
    let temp_root = unique_root("scale-wasm");
    let plugins_dir = temp_root.join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;

    let ids = stage_wasm_instances(&plugins_dir, count)?;
    let assembled = host::assemble(&temp_root, &plugins_dir, Vec::new()).await?;
    Ok(ScaleEnv {
        host: assembled.host,
        ids,
        temp_root,
    })
}

/// 建 N 个 TS-only 插件目录并装配宿主（无 wasm 实例，只测清单投影）。
pub async fn build_manifests(count: usize) -> anyhow::Result<ScaleEnv> {
    anyhow::ensure!(count > 0, "清单投影至少需要 1 个插件");
    let temp_root = unique_root("scale-manifest");
    let plugins_dir = temp_root.join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;

    let ids = stage_manifests(&plugins_dir, count)?;
    let assembled = host::assemble(&temp_root, &plugins_dir, Vec::new()).await?;
    Ok(ScaleEnv {
        host: assembled.host,
        ids,
        temp_root,
    })
}

/// 同一组件摆成 N 个属主目录（id 唯一、api 声明清空——多实例同名 api 会触发
/// 注册表冲突，激活被 fail-closed 拒绝）。
fn stage_wasm_instances(plugins_dir: &Path, count: usize) -> anyhow::Result<Vec<String>> {
    let artifact = bridge::build_fixture_component()?;
    let manifest_src = repo_paths::desktop_root().join("packages/plugin-bench-test/plugin.json");
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_src)?)?;

    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = format!("com.wasmnest.scale.app{i:02}");
        let dir = plugins_dir.join(&id);
        std::fs::create_dir_all(&dir)?;

        let mut m = manifest.clone();
        m["id"] = serde_json::Value::String(id.clone());
        m["name"] = serde_json::Value::String(format!("Scale Probe {i:02}"));
        // 同名 api 只能有一个属主；规模实例只跑命令面，不需要互调服务声明
        m["api"] = serde_json::json!([]);
        std::fs::write(dir.join("plugin.json"), serde_json::to_string_pretty(&m)?)?;
        std::fs::copy(&artifact, dir.join(format!("{}.wasm", bridge::FIXTURE_LIB)))?;
        std::fs::write(dir.join("index.js"), "// perf scale fixture: 命令面专用\n")?;
        ids.push(id);
    }
    Ok(ids)
}

/// N 个最小 TS-only 清单：`plugin_list_loaded` 的投影对象，不含 wasm。
fn stage_manifests(plugins_dir: &Path, count: usize) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = format!("com.wasmnest.scale.manifest{i:03}");
        let dir = plugins_dir.join(&id);
        std::fs::create_dir_all(&dir)?;

        let manifest = serde_json::json!({
            "id": id,
            "name": format!("Manifest Probe {i:03}"),
            "version": "0.0.1",
            "platform": "desktop",
            "description": "perf registry projection fixture",
            "author": "WasmNest",
            "main": "index.js",
            "pluginType": "ts-only"
        });
        std::fs::write(dir.join("plugin.json"), serde_json::to_string_pretty(&manifest)?)?;
        std::fs::write(dir.join("index.js"), "// perf manifest fixture\n")?;
        ids.push(id);
    }
    Ok(ids)
}

fn unique_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "wasmnest_perf_{tag}_{}_{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}
