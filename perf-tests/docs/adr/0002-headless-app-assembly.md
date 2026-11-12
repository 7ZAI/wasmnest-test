# 无头装配替代 GUI 授权

应用基准要在没有 Tauri `AppHandle` 的进程里跑真实应用命令。两条路：重建一套
命令 mock（会测不到真实链路），或按生产装配装载真实宿主并绕开弹窗。我们选择
后者：宿主装配与产品同形（消息总线 dispatcher、guest 事件派发器与第一方目录
清单都按 GUI bootstrap 注入），文件目录用 `fs_granted_paths` 预授权；鉴权弹窗、
依赖窗口的命令与依赖对端推送的命令（如 file-transfer `set-settings`）明确排除。

guest 事件派发器是总线投递与 `on-process-done` 回调的前置：缺它时宿主报
`guest events unwired` 并按 trap 自动重载，插件会在运行中被停用——桥梁场景
（D / E2 / F1）会连续失败，看起来像协议问题，实际是装配漏项。

## Consequences

- 测量覆盖真实参数解析、私有库/文件 IO 与回执序列化，但不覆盖 GUI 交互本身。
- 新增应用场景时先确认其命令在无头路径上可完成；被排除的路径应写进场景注释，
  而不是让它在基准里静默失败。
