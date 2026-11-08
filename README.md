# VectorCraft for HarmonyOS

将矢量绘图编辑器 [VectorCraft](https://github.com/storytold/vectorcraft)（上游 `storytold/vectorcraft`）移植到 HarmonyOS / HarmonyNext（OpenHarmony API 26），交付一个**可安装、可运行**的 `.hap`。上游 `crates/*` **零改动**，全部适配工作集中在本仓库的宿主适配层。

> 完整的移植过程与真机验证见 [`移植报告.md`](移植报告.md)（中文）。

## 真机已验证
- wgpu（GLES/EGL 后端）出帧渲染（真机 GPU = Maleoon 916，麒麟）。
- 全中文 UI：菜单栏 / 左侧工具栏 / 右侧「属性·图层·库」/ 状态栏，CJK 字体兜底无豆腐块。
- 菜单系统、工具切换、浮动面板、状态栏实时刷新均正常工作。
- 双 ABI：`aarch64-unknown-linux-ohos`（真机/ARM 模拟器）+ `x86_64-unknown-linux-ohos`（x86 模拟器）。
- LTO（thin）瘦身后的 Rust 静态库。

## 架构（三层）
ArkTS（`EntryAbility` + `XComponent`）→ C++ NAPI 桥（产物 `libvectorcraft.so`，NAPI 模块名 `vectorcraft`）→ Rust 适配层（`native/rust`，静态库 `libvectorcraft_hmos.a`）。

- 上游 `vectorcraft/crates/*` 以 **path 依赖**引入（见 `native/rust/Cargo.toml`，从该文件出发的路径为 `../../../vectorcraft/crates/*`，即仓库同级的上游目录）。
- `wgpu-hal` 的 GLES 补丁 vendored 在 `third_party/wgpu-hal-30.0.1`（自包含）。
- FFI 边界：13 个 `vc_*` 符号 —— `vc_init` / `vc_input` / `vc_reply` / `vc_open` / `vc_drop` / `vc_drop_file` / `vc_command` / `vc_attach` / `vc_resize` / `vc_detach` / `vc_frame` / `vc_active` / `vc_close`。

## 目录
| 路径 | 说明 |
|---|---|
| `AppScope/` | 应用级配置：bundleName `io.github.shaliuxin.vectorcraft.hmos`、图标、`app_name` |
| `entry/` | ArkTS 入口 + 资源 |
| `native/rust/` | Rust 适配层（宿主缝 + 静态库构建） |
| `third_party/wgpu-hal-30.0.1/` | 上游 wgpu-hal 的 GLES 补丁（vendored） |
| `scripts/` | 构建 / 签名 / 校验脚本 |
| `移植报告.md` | 完整移植报告（中文） |

## 构建前置
1. **上游源码（必需）**：本仓库以 path 依赖引用上游，需把 [`storytold/vectorcraft`](https://github.com/storytold/vectorcraft) 克隆为**同级目录**：
   ```
   <parent>/
   ├── vectorcraft/       # 上游 storytold/vectorcraft
   └── vectorcraft-hmos/  # 本仓库
   ```
   本仓库当前对齐上游 `main` 提交 `0a702cc0a5f36346b420b7effc5698ef36b05b2d`
   （2026-11-08，其 workspace 版本仍为 `0.21.0`，故本仓库 `versionName` 保持 `0.21.0`）。
   path 依赖从 `native/rust/Cargo.toml` 出发为 `../../../vectorcraft/crates/*`。

   **升级上游时**：`[patch]` 只对 workspace 根生效，而本仓库的 Rust 适配层是独立
   workspace，所以必须把上游根 `Cargo.toml` 的 `[patch.crates-io]` **整段**同步到
   `native/rust/Cargo.toml`，再重生成锁：
   ```bash
   cargo generate-lockfile --manifest-path native/rust/Cargo.toml
   ```
   随后 `scripts/build_native.sh` 会以 `--locked` 构建。上游 `0a702cc0` 相对初版新增了
   `linesweeper`（craft-libs rev `481f66d2…`）这一条 patch，漏抄会导致 pathops 依赖解析到
   crates.io 的未打补丁版本。
2. **DevEco Studio**（HarmonyOS NEXT / API 26）及其 OHOS NDK。
3. **Rust**：安装两个目标 `rustup target add aarch64-unknown-linux-ohos x86_64-unknown-linux-ohos`。

## 构建步骤
```bash
# 1) 双 ABI 原生静态库（首次约 40 分钟；依赖已缓存后仅重编顶层 crate，约 5 分钟）
bash scripts/build_native.sh

# 2) 首次签名：在 DevEco「File → Project Structure → Signing Configs → 自动签名」
#    为 bundleName io.github.shaliuxin.vectorcraft.hmos 签发，
#    DevEco 会用你本机的真实四件套回写 build-profile.json5 的占位符。
#    注意：公开仓库里的 build-profile.json5 是**占位符版**，本地每次重新打包前都要先
#    临时恢复真实 material（本机可存为 signing/build-profile.local.json5，该目录已被
#    .gitignore 排除），否则 SignHap 报 00303107 Invalid storeFile value；
#    推送前务必把占位符版还原，避免把 AGC 签名口令写进公开仓库。

# 3) 打包 + 签名 HAP（DevEco 里 Run，或 hvigor CLI）
#    <DevEco>/tools/node/node.exe <DevEco>/tools/hvigor/bin/hvigorw.js \
#        --mode module -p module=entry@default -p product=default assembleHap
```
产物：`entry/build/default/outputs/default/entry-default-signed.hap`（双 ABI + 已签名）。

## 签名说明
`build-profile.json5` 的 `material` 四件套为**占位符**——本仓库刻意不保留签名口令、本机绝对路径，或绑定设备 UDID 的 profile 名。首次构建前请在 DevEco 自动签名（推荐）；`scripts/make_vectorcraft_signing.sh` 可生成本地 CA 链（`issuer=pki_internal`），**仅用于验证打包链路，真机安装会报 `9568257`**。

## License
MIT OR Apache-2.0，与上游一致（见 [`LICENSE-MIT`](LICENSE-MIT) / [`LICENSE-APACHE`](LICENSE-APACHE)）。
上游版权归 ArtCraft Team 及 VectorCraft contributors 所有；第三方素材清单见 [`NOTICE`](NOTICE)。
