//! 宿主装配共用段。
//!
//! bridge（夹具实例）与 apps（真实应用）必须走**同一份**装配：
//! 临时内核库 → `PluginHost`（无 AppHandle，端事实 = `DesktopHostFacts`）→
//! 消息总线分发器 → guest 事件派发器 → 插件私有库根。任何一处漏掉都会让测点
//! 口径漂移（少 `init_message_bus` 时总线订阅与互调全部静默失效；少 guest 事件
//! 派发器时总线投递与 process-done 回调报 unwired，宿主会当 trap 自动重载）。

use std::path::Path;
use std::sync::{Arc, Mutex};

use wasmnest_desktop_lib::db::Database;
use wasmnest_desktop_lib::wasm_core::PluginHost;
use wasmnest_wasm_core::security::fs_auth::TrustedDir;

pub struct AssembledHost {
    pub host: Arc<PluginHost>,
    pub db: Arc<Mutex<Database>>,
}

/// 在 `temp_root` 下建内核库 / 用户插件目录 / 插件私有库根，并装配宿主。
///
/// `first_party_dirs`：产品清单由调用方决定——bridge 传空表（只测桥接原语），
/// apps 传 `wasmnest_desktop_lib::first_party_dirs::first_party_dirs()`（生产同形）。
pub async fn assemble(
    temp_root: &Path,
    plugins_dir: &Path,
    first_party_dirs: Vec<(&'static str, Vec<TrustedDir>)>,
) -> anyhow::Result<AssembledHost> {
    let user_plugins_dir = temp_root.join("user-plugins");
    std::fs::create_dir_all(&user_plugins_dir)?;

    let db = Arc::new(Mutex::new(Database::new(&temp_root.join("kernel.db"))?));
    db.lock().expect("db lock").init_schema()?;

    let host = PluginHost::new(
        db.clone(),
        plugins_dir,
        &user_plugins_dir,
        None,
        None,
        first_party_dirs,
        wasmnest_wasm_core::manager::runtime::AbiPolicy {
            form: Some(wasmnest_plugin_api::form::FORM_COMPONENT),
        },
        Arc::new(wasmnest_desktop_lib::plugin::host_facts::DesktopHostFacts),
        None,
        None,
    )
    .await;

    // 生产由 `lib.rs` 在宿主建成后调用；`PluginHost::new` 自身不调
    host.init_message_bus().await;
    // 与 GUI bootstrap 同款：晚绑定共享槽，扫描期已实例化的插件同样生效
    host.wasm_runtime().set_guest_events(Arc::new(
        wasmnest_desktop_lib::plugin::guest_events::DesktopGuestEvents,
    ));

    let plugin_db_root = temp_root.join("plugin-dbs");
    std::fs::create_dir_all(&plugin_db_root)?;
    host.wasm_host_ctx()
        .set_plugin_db_root(Some(plugin_db_root));

    Ok(AssembledHost { host, db })
}
