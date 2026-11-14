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
   本仓库当前对齐上游**正式版 v0.24.1**，提交 `f4922aa7ac99a4f6d56fc68a83a44945207ec610`
   （2026-11-12 发布；其 workspace 版本为 `0.24.1`，故本仓库 `versionName` = `0.24.1`、
   `versionCode` = `24001`）。
   path 依赖从 `native/rust/Cargo.toml` 出发为 `../../../vectorcraft/crates/*`。

   **升级上游时**：`[patch]` 只对 workspace 根生效，而本仓库的 Rust 适配层是独立
   workspace，所以必须把上游根 `Cargo.toml` 的 `[patch.crates-io]` **整段**同步到
   `native/rust/Cargo.toml`，再重生成锁：
   ```bash
   cargo generate-lockfile --manifest-path native/rust/Cargo.toml
   ```
   随后 `scripts/build_native.sh` 会以 `--locked` 构建。v0.24.1 的 `[patch.crates-io]` 与
   v0.23.0 逐行一致（含 `linesweeper`，craft-libs rev `481f66d2…`）；漏抄会让 pathops
   解析到 crates.io 的未打补丁版本。但 `[patch]` 一致不代表锁不变：v0.24.1 改了
   `[workspace.dependencies]` 的 git rev（`craft-ui` → `bb8320f5…`，
   `craft-vectorcraft-format` / `craft-update` → `6e03f65…`），并新增
   `vectorcraft-js` / `vectorcraft-apischema` / `skrifa`，因此**必须重生成锁**
   （本次 `[[package]]` 段 464 → 466）。
   ⚠️ `cargo generate-lockfile` 会把 `wgpu` 全系升到 crates.io 的 30.0.2，
   而 `third_party/wgpu-hal-30.0.1` 是 30.0.1 的 GLES uniform-block 修补。
   适配层已把 `wgpu` 钉成 `=30.0.1`；重生成锁后仍须用
   `cargo update -p <各 wgpu 子 crate> --precise 30.0.1` 把整条链对齐，
   否则渲染栈版本错配（详见 `移植报告.md` §15）。
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

## 中文输入法（IME，2026-11-09 真机通过）

HarmonyOS 官方「自定义编辑框」方案：1×1 透明锚点 `Text('').id('vectorcraft-ime-anchor').focusable(true)`
→ `requestFocus` → `await inputMethod.getController().attach(true, textConfig)` → **attach 成功后**才注册
`insertText/setPreviewText/finishTextPreview/deleteLeft/sendFunctionKey`（提前注册抛 `12800009 detached`）。
导入必须是 `import { inputMethod } from '@kit.IMEKit';`（命名导入），XComponent 已去掉 `.focusOnTouch(true)`。
真机日志：`VectorCraft native ime: show` → `IME attached` → `listeners bound`。

> PC 多窗口 + 外接物理键盘时系统不自动展开面板（系统「备忘录」同样只有悬浮工具栏），
> 需点右下悬浮栏 ⌨ —— 系统策略，非应用缺陷。


## License
MIT OR Apache-2.0，与上游一致（见 [`LICENSE-MIT`](LICENSE-MIT) / [`LICENSE-APACHE`](LICENSE-APACHE)）。
上游版权归 ArtCraft Team 及 VectorCraft contributors 所有；第三方素材清单见 [`NOTICE`](NOTICE)。

---

## ⚠️ 移植必查：OHOS 平台判定与资源探测（2026-11-11 全项目强制）

OHOS triple 仍报告 `target_os = "linux"`。上游里靠 `#[cfg(target_os = "linux")]` 分派的 Linux 专属逻辑（cgroup / procfs / 内存探测 / 字体路径 / 系统目录）在鸿蒙应用沙箱里会被误用，而且编译链接全绿、只在运行时出怪错。PhotoCraft v0.22.0 曾因此把可用内存测成 `0`，所有稍大的照片都报 `not enough memory ... 0.0 GB is free`。

开工 / 每次上游升级后必做：① `grep -rn 'target_os = "linux"\|cfg(unix)' <上游>/crates` 逐个判断平台假设；② OHOS 分支只用 `#[cfg(target_env = "ohos")]`，Linux 分支加 `not(target_env = "ohos")`；③ 资源探测「测不到」返回 `UNKNOWN`，绝不返回 `Some(0)`；④ 用「文件小、解码内存大」的样本做真机前后对照验收。完整根因链、修复模板与验收手法见《craft-鸿蒙移植通用检查清单》文首 ⭐ 铁律与 §18。

> 出处（MLA）：「§18. PhotoCraft 追加经验」，*craft-鸿蒙移植通用检查清单.md*, 本地工作区 `C:/Users/mingh/WorkBuddy/2026-11-01-23-18-01/`, 11 Nov. 2026.

---

## ⚠️ 移植必查：关窗保存确认（2026-11-11 全项目强制）

凡能编辑文档、会产生未保存状态的项目，点系统窗口 X（或返回手势）**必须**先走引擎的「是否保存」确认；未保存时静默退出属**数据丢失级缺陷**。该链路跨 ArkTS 拦截层、Rust 逻辑相帧顺序、上游「允许关闭」信号三层，任一层缺失都能编译通过、正常启动，但点 X 就是不弹框。

开工 / 每次上游升级后必做：① 先 `grep` 本上游用什么表达「可以关窗」（`allow_close` / `close_requested` / `ui.status` / `CancelClose`），绝不照抄兄弟工程字段；② ArkTS 只订阅一个窗口级关窗事件并直连 `native.close(); return true;`；③ **头号根因**：Rust `tick` 必须放进 `ctx.run_ui` 的 pass 内、`ui()` 之前，否则 `ViewportEvent::Close` 当帧不可见 → 静默关窗；④ 绝不把「本帧没有 `CancelClose`」当成「无未保存」；⑤ 关窗期间强制出帧（`active || close`）；⑥ **真机三态人工验收** Save / Don't Save / Cancel，hilog 里 `RegisterListener` 只证明监听器注册，不等于修好。完整 12 项必查清单见《craft-鸿蒙移植通用检查清单》文首 ⭐ 关窗保存确认铁律、§15 / §15.1 与 §19。

> 出处（MLA）：「⭐ 关窗保存确认铁律」，*craft-鸿蒙移植通用检查清单.md*, 本地工作区 `C:/Users/mingh/WorkBuddy/2026-11-01-23-18-01/`, 10–11 Nov. 2026.
