# 性能基准集中到 wasmnest-test/perf-tests

平台桥接基准原先以 `[[test]] wasm_bridge_bench` 住在 `wasmnest-desktop` 里，
只能覆盖合成夹具；应用侧的性能无人负责。我们决定把**宿主侧驱动**迁到
`wasmnest-test/perf-tests`（独立 workspace、独立 release profile），并将真实
随包应用作为第二个模块接入。理由：测量需要 release 构建 + 已构建的应用产物 +
跨四仓的环境快照，这三者都属于测试容器仓的职责，而不是产品仓的测试面。

夹具插件、打包脚本（`bench:zip`）与 e2e 用例留在平台仓：它们是产品仓的
测试资产，迁移只会制造跨仓构建依赖。平台仓删除 `wasm_bridge_bench` 测试目标
与 `bench` / `bench:full` 脚本，`bench:zip` 保留。

## Consequences

- 跨仓路径依赖必须走 sibling 路径（与 `cross-end-tests` 同一铁律），能力 crate
  在依赖图中只有一份。
- 性能数字只在 release 下采集；debug 构建被入口拒绝。
- 应用产物需先构建，缺失时 apps 模块 fail-visible，不静默跳过。
