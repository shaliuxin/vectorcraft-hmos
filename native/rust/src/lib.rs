//! 鸿蒙专属 FFI 边界。矢量绘图引擎与全部文档处理逻辑保持在安全 Rust 上游（vectorcraft）。
//!
//! 与 gridcraft/wordcraft 同构：VectorCraft 的 `VectorcraftApp` 已把宿主缝直接暴露为
//! `new(Session, Services)` / `install_ime_reset_hooks(ctx)` / `raw_input_hook(ctx, raw)` /
//! `tick(ctx)` / `finish_ime_logic(ctx)` / `ui(ui)` / `run(id, params)`（上游 app.rs 注释：
//! "eframe isn't a dependency of this crate (the host owns the event loop)"），本适配层正是它的
//! 第三个宿主实现（与 apps/vectorcraft 桌面、apps/vectorcraft-web 并列），与 gridcraft 的
//! SheetApp / solidcraft 的 SolveApp 同构。
//!
//! 与 gridcraft 的关键差异（须注意）：
//! - 字体/主题：上游 `VectorcraftApp::ui` 内部做 `ui_setup`（`theme::interface_fonts` + `set_fonts`
//!   + 图片加载器），**不需要**宿主像 gridcraft 那样显式调 `setup_context`。CJK 兜底仍须宿主用
//!   `Context::add_font(FontInsert)` 以最低优先级追加（上游 `interface_fonts` 无 CJK）。
//! - 文件：上游 open/save **依赖 `Services.read`/`write`**（`io/open.rs` 无 reader 即报
//!   "no file reader"，`io/save.rs` 同理），不像 gridcraft 直接 `std::fs`。OHOS 沙箱路径可被
//!   `std::fs` 读写，故这里以 `std::fs` 实现 read/write/exists/read_head，打开/另存为经 bridge
//!   弹 ArkTS 选择器拿沙箱路径（桌面式，download 留空）。
//! - 关闭：vectorcraft 在 `VectorcraftApp::tick` 里读 `ctx.input(|i| i.viewport().close_requested())`
//!   （`crates/ui-egui/src/app.rs:345`），有未保存文档就发 `ViewportCommand::CancelClose` 并弹
//!   `saveChanges`。**宿主必须像 eframe 的 `App::logic`、web shell 的 `logic`、GridCraft 的
//!   `editor_frame` 那样，把 tick 放在 `ctx.run_ui` 的 pass 内、`ui()` 之前**：上游注释明确写了
//!   logic-only pass 读的是「上一帧已显示帧」的 input。若把 tick 放在 run_ui 之外，
//!   `ViewportEvent::Close` 永远慢一帧，宿主当帧收不到 CancelClose 就误判「无未保存」，直接
//!   notify("close") → terminateSelf()，窗口静默关闭、不弹保存框（真机实测过的故障）。
//!   保存/丢弃完成后上游写 `app.ui.status == "quit"`（桌面宿主 `apps/vectorcraft/src/app.rs:159`
//!   据此发 `ViewportCommand::Close`），鸿蒙宿主没有 eframe 包装层，须自己锁存该信号后才放行。
#![allow(clippy::needless_lifetimes)]
mod appearance;
mod bridge;
mod display_color;
mod file_open;
mod form_factor;
mod input;
mod locale;
mod render;

use serde::Deserialize;
use vectorcraft_ui_egui::io::open_path;
use vectorcraft_ui_egui::{FilePick, Services, VectorcraftApp};
use std::{
    pin::Pin,
    sync::{
        OnceLock,
        mpsc,
        atomic::{AtomicI8, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};
use std::sync::Arc;
use std::sync::Mutex;

/// 把 `log` crate 的记录转到 hilog：wgpu-hal 的 EGL 诊断（平台选择 / surface 创建 / swap 失败）
/// 默认没有 logger 就会被丢掉，设备上完全看不见。
#[cfg(target_env = "ohos")]
mod glog {
    use std::ffi::{CString, c_char};

    unsafe extern "C" {
        fn vc_hilog(level: i32, message: *const c_char);
    }

    struct Hilog;

    impl log::Log for Hilog {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            let level = match record.level() {
                log::Level::Error => 2,
                log::Level::Warn => 1,
                _ => 0,
            };
            let line = format!("[{}] {}: {}", record.level(), record.target(), record.args());
            if let Ok(text) = CString::new(line) {
                // SAFETY: 有效的 NUL 结尾 C 字符串；vc_hilog 只读该指针。
                unsafe { vc_hilog(level, text.as_ptr()) };
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: Hilog = Hilog;

    pub fn install() {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Config {
    files: String,
    #[serde(default)]
    documents: String,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    language: String,
    /// 系统 locale（`zh-Hans-CN` 这类 BCP-47 标签），决定「跟随系统」解析到哪种语言。
    #[serde(default, rename = "systemLocale")]
    system_locale: String,
    #[serde(default, rename = "displayP3")]
    display_p3: bool,
    #[serde(default, rename = "systemTheme")]
    system_theme: String,
    #[serde(default)]
    fullscreen: bool,
    #[serde(default, rename = "deviceType")]
    device_type: String,
    #[serde(default, rename = "widthVp")]
    width_vp: f32,
}
fn default_scale() -> f32 {
    1.
}

#[allow(dead_code)]
enum Message {
    Attach(usize, u32, u32),
    Resize(u32, u32),
    Detach,
    Frame,
    Active(bool),
    Input(input::Input),
    Open(String, Vec<u8>),
    Drop(String, Vec<u8>, f32, f32),
    DropFile(String, std::fs::File, f32, f32),
    Close,
    Wake(Duration),
    Command(String, serde_json::Value),
}

static SEND: OnceLock<mpsc::Sender<Message>> = OnceLock::new();
fn send(message: Message) {
    if let Some(tx) = SEND.get() {
        let _ = tx.send(message);
    }
}

/// 把 ArkTS 上报的系统主题透传给 appearance 模块。
static SYSTEM_THEME_TOKEN: AtomicI8 = AtomicI8::new(0);

/// # Safety
/// `ptr`/`len` 必须在调用期间指向可读内存；调用方保证。
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len > isize::MAX as usize || (ptr.is_null() && len != 0) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn copy_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(data.len())
        .map_err(|e| format!("Not enough memory to open file: {e}"))?;
    bytes.extend_from_slice(data);
    Ok(bytes)
}

fn write_temp_file(dir: &str, name: &str, bytes: &[u8]) -> std::io::Result<String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = std::path::Path::new(dir).join(format!("{stamp}_{safe}"));
    std::fs::write(&path, bytes)?;
    Ok(path.to_string_lossy().into_owned())
}

/// 读文件最多 `max` 字节（`io/save.rs`/`panels/actions/files.rs` 用它判断超限，避免全读）。
#[cfg(target_env = "ohos")]
fn read_file_head(path: &str, max: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(max.min(64 * 1024));
    file.take(max as u64)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

// ===================== FFI 导出（C++ 边界调用） =====================

/// # Safety
/// 指针/长度对必须指向可读内存直到本调用返回；notify 回调在进程生命周期内保持有效。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_init(ptr: *const u8, len: usize, notify: bridge::Notify) -> bool {
    let Some(data) = (unsafe { bytes(ptr, len) }) else {
        return false;
    };
    let Ok(config) = serde_json::from_slice::<Config>(data) else {
        return false;
    };
    if !config.scale.is_finite() || config.scale <= 0. || config.scale > 8. {
        return false;
    }
    bridge::init(notify);
    #[cfg(target_env = "ohos")]
    glog::install();
    // 必须在 UI 创建之前：vectorcraft 的界面语言经 `i18n::Lang::from_pref("auto")` →
    // `i18n::system_lang()` 首次求值。最可靠的是显式 `set_system_locales`（直接告诉它系统首选），
    // 辅以写 POSIX locale 环境变量兜底（`detect_system_lang` 读 VECTORCRAFT_LOCALE/LC_ALL/LANG）。
    locale::install(&config.files, &config.system_locale, &config.language);
    {
        let tag = [config.system_locale.as_str(), config.language.as_str()]
            .iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(|s| s.chars().map(|c| if c == '_' { '-' } else { c }).collect::<String>());
        if let Some(tag) = tag {
            vectorcraft_ui_egui::i18n::set_system_locales(&[tag]);
        }
    }
    if !config.system_theme.is_empty() {
        let dark = config.system_theme.eq_ignore_ascii_case("dark");
        SYSTEM_THEME_TOKEN.store(if dark { 1 } else { -1 }, Ordering::Release);
        appearance::set_dark(dark);
    }
    let (tx, rx) = mpsc::channel();
    if SEND.set(tx).is_err() {
        return true;
    }
    std::thread::Builder::new()
        .name("vectorcraft-native".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker(config, rx)))
                .is_err()
            {
                bridge::notify(0, "error", "Native host initialization failed", &[]);
            }
        })
        .is_ok()
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_input(ptr: *const u8, len: usize) {
    let Some(data) = (unsafe { bytes(ptr, len) }) else { return; };
    let Ok(value) = serde_json::from_slice(data) else { return; };
    send(Message::Input(value));
}

/// # Safety
/// 同 `vc_init`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_reply(
    id: u64,
    ok: bool,
    text: *const u8,
    text_len: usize,
    data: *const u8,
    len: usize,
) {
    let Some(text) = (unsafe { bytes(text, text_len) }) else {
        bridge::reply(id, Err("Invalid reply text buffer".into()));
        return;
    };
    let Some(data) = (unsafe { bytes(data, len) }) else {
        bridge::reply(id, Err("Invalid reply data buffer".into()));
        return;
    };
    let text = String::from_utf8_lossy(text).into_owned();
    bridge::reply(
        id,
        if ok {
            copy_bytes(data).map(|data| (text, data))
        } else {
            Err(text)
        },
    );
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_open(text: *const u8, n: usize, data: *const u8, len: usize) {
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        match copy_bytes(data) {
            Ok(data) => send(Message::Open(String::from_utf8_lossy(text).into_owned(), data)),
            Err(error) => bridge::notify(0, "error", &error, &[]),
        }
    }
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_drop(
    text: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    x: f32,
    y: f32,
) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        send(Message::Drop(
            String::from_utf8_lossy(text).into_owned(),
            data.to_vec(),
            x,
            y,
        ));
    }
}

/// # Safety
/// `text`/`n` 必须可读且 `fd` 打开直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_drop_file(text: *const u8, n: usize, fd: i32, x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    let Some(text) = (unsafe { bytes(text, n) }) else {
        return;
    };
    match unsafe { file_open::duplicate(fd) } {
        Ok(file) => send(Message::DropFile(
            String::from_utf8_lossy(text).into_owned(),
            file,
            x,
            y,
        )),
        Err(error) => bridge::notify(0, "error", &error, &[]),
    }
}

/// # Safety
/// `ptr`/`len` 为自由文本命令标识。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_command(ptr: *const u8, len: usize) {
    let Some(data) = (unsafe { bytes(ptr, len) }) else { return; };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return; };
    let Some(id) = v.get("id").and_then(|v| v.as_str()) else { return; };
    send(Message::Command(
        id.into(),
        v.get("params").cloned().unwrap_or_default(),
    ));
}

/// # Safety
/// `window` 必须是存活的 OHNativeWindow，且已获取一个原生引用；该引用所有权转移给 worker 及其 wgpu surface。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vc_attach(window: usize, w: u32, h: u32) {
    if window != 0 {
        send(Message::Attach(window, w, h));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vc_resize(w: u32, h: u32) {
    send(Message::Resize(w, h));
}

#[unsafe(no_mangle)]
pub extern "C" fn vc_detach() {
    bridge::cancel();
    send(Message::Detach);
}

#[unsafe(no_mangle)]
pub extern "C" fn vc_frame() {
    send(Message::Frame);
}

#[unsafe(no_mangle)]
pub extern "C" fn vc_active(active: bool) {
    send(Message::Active(active));
}

#[unsafe(no_mangle)]
pub extern "C" fn vc_close() {
    send(Message::Close);
}

// ===================== 仅 OHOS 宿主逻辑 =====================

#[cfg(target_env = "ohos")]
unsafe extern "C" {
    fn vc_request_vsync();
}

#[cfg(target_env = "ohos")]
fn worker(config: Config, rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "stage", "Rust worker entered", &[]);
    let ctx = egui::Context::default();
    let form = form_factor::Form::new(&config.device_type, config.width_vp);
    bridge::notify(
        0,
        "diagnostic",
        &format!(
            "form factor {:?}, {:.0} vp, {:?}",
            form.device_type, form.width_vp, form.width_class
        ),
        &[],
    );

    // vectorcraft 的 session：`new` 会为每个已开文档建 view；空 session 时上游显示 Home（文件>新建/打开）。
    // 与 gridcraft 一样，首启给一个空文档，编辑器立即可用（`file.new` 走引擎，尺寸取可视区比例）。
    let session = vectorcraft_engine::Session::new();
    let mut app = VectorcraftApp::new(session, services(&config.files));
    if app.session.documents().is_empty() {
        if let Err(e) = app.run("file.new", serde_json::json!({ "width": 800, "height": 600 })) {
            bridge::notify(0, "diagnostic", &format!("file.new skipped: {e}"), &[]);
        }
    }
    bridge::notify(
        0,
        "diagnostic",
        &format!("interface language: {}", app.ui_language().code()),
        &[],
    );

    // 上游 `ui()` 内部会 `install_hooks` + `set_fonts`（`theme::interface_fonts`，无 CJK），
    // 但 `ui_setup` 的注释要求：在**首个 run_ui pass 之前**宿主先 `install_ime_reset_hooks`。
    VectorcraftApp::install_ime_reset_hooks(&ctx);

    // 鸿蒙 CJK 字体注入（真机实测必需）：上游 `theme::interface_fonts()` 无 CJK 字形，中文工具栏/
    // 菜单全是 tofu 方块。用 egui 0.37 的增量 API 把系统字体字节以最低优先级追加到所有字体族：
    // 只在既有字体找不到字形时兜底，不覆盖拉丁字形、不改上游字体定义与主题。`ui_setup` 的
    // `set_fonts` 只设基础定义，`add_font` 的插入在其之上叠加，二者不互相覆盖。
    if let Some(bytes) = locale::cjk_font_bytes(&config.language) {
        let families: Vec<egui::FontFamily> =
            vectorcraft_ui_egui::theme::interface_fonts()
                .families
                .keys()
                .cloned()
                .collect();
        let count = families.len();
        ctx.add_font(egui::FontInsert {
            name: "hmos-cjk-han".to_owned(),
            data: egui::FontData::from_owned(bytes),
            families: families
                .into_iter()
                .map(|family| egui::InsertFontFamily {
                    family,
                    priority: egui::FontPriority::Lowest,
                })
                .collect(),
        });
        bridge::notify(
            0,
            "diagnostic",
            &format!("CJK fallback injected into {count} font families"),
            &[],
        );
    }

    bridge::notify(0, "stage", "editor ready", &[]);

    // 起始逻辑帧（无用户输入）：让 app 在宿主登记后先跑一次 tick + IME 收尾。
    app.tick(&ctx);
    app.finish_ime_logic(&ctx);

    let mut render = match render::Render::new(config.display_p3) {
        Ok(render) => render,
        Err(error) => {
            bridge::notify(0, "error", &error, &[]);
            return;
        }
    };
    let mut active = true;
    let mut visible = false;
    let mut deadline: Option<Instant> = None;
    let started = Instant::now();
    let mut events: Vec<egui::Event> = Vec::new();
    let mut input_state = input::State::default();
    let mut close = false;
    // 上游 `ui.status == "quit"` 的锁存：用户在保存框选保存 / 丢弃后允许关窗（等价于
    // gridcraft 的 `allow_close`）。一旦观察到就保持，避免状态栏刷新把信号冲掉。
    let mut quit_requested = false;
    // 已通知宿主关窗：不再重复发 notify，也不再处理新的关闭请求。
    let mut terminating = false;
    // 软键盘显隐：egui 的 TextEdit 获得焦点时会把 `platform_output.ime` 置为 Some，
    // 失焦置 None。只在**状态变化**时通知 ArkTS，避免每帧刷消息。
    let mut ime_shown = false;

    ctx.set_request_repaint_callback(|info| send(Message::Wake(info.delay)));
    bridge::notify(0, "ready", "Native host initialized", &[]);

    loop {
        let wait = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let message = match wait {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    deadline = None;
                    if (active || close) && visible && render.attached() {
                        unsafe { vc_request_vsync() };
                    }
                    continue;
                }
                Err(_) => break,
            },
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), String> {
                match message {
                    Message::Attach(ptr, w, h) => {
                        visible = w > 0 && h > 0;
                        ime_shown = false;
                        render.bind(render::Window(ptr), w, h)?;
                        bridge::resume();
                        deadline = Some(Instant::now());
                        bridge::notify(0, "ready", "Native egui / wgpu GLES", &[]);
                    }
                    Message::Resize(w, h) => {
                        visible = w > 0 && h > 0;
                        render.resize(w, h)?;
                        deadline = Some(Instant::now());
                    }
                    Message::Detach => {
                        visible = false;
                        deadline = None;
                        render.detach();
                        input_state.cancel(&mut events);
                        // 表面销毁时输入客户端一定已断：复位状态并显式收起软键盘，
                        // 否则下次 Attach 会以为键盘还开着而不再弹。
                        if ime_shown {
                            ime_shown = false;
                            bridge::notify(0, "ime", "hide", &[]);
                        }
                    }
                    Message::Active(value) => {
                        active = value;
                        if active {
                            bridge::resume();
                            deadline = Some(Instant::now());
                        } else {
                            deadline = None;
                            input_state.cancel(&mut events);
                        }
                    }
                    Message::Input(value) => {
                        if value.kind == "system_theme" {
                            let dark = value.text.eq_ignore_ascii_case("dark");
                            appearance::set_dark(dark);
                            deadline = Some(Instant::now());
                            return Ok(());
                        }
                        input_state.push(value, config.scale, &mut events);
                        deadline = Some(Instant::now());
                    }
                    Message::Open(name, data) => {
                        if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                            open_into_app(&mut app, &path);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Drop(name, data, _x, _y) => {
                        if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                            open_into_app(&mut app, &path);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::DropFile(name, file, _x, _y) => {
                        match file_open::read(file) {
                            Ok(data) => {
                                if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                                    open_into_app(&mut app, &path);
                                }
                            }
                            Err(e) => bridge::notify(0, "error", &e, &[]),
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Close => {
                        // 系统 X 已被 ArkTS 的 windowWillClose 拦下。窗口管理器在关窗流程里
                        // 可能先让窗口失焦（onBackground → native.active(false)），若不管，
                        // Frame 分支会直接跳过，保存框永远画不出来也点不到。
                        bridge::resume();
                        active = true;
                        close = true;
                        bridge::notify(
                            0,
                            "diagnostic",
                            "close guard: system X forwarded, validating unsaved documents",
                            &[],
                        );
                        deadline = Some(Instant::now());
                    }
                    Message::Wake(delay) => {
                        if (active || close) && visible {
                            if let Some(next) = Instant::now().checked_add(delay) {
                                deadline = Some(deadline.map_or(next, |d| d.min(next)));
                            }
                        }
                    }
                    Message::Command(id, params) => {
                        let result = app.run(&id, params);
                        bridge::notify(
                            0,
                            "command",
                            &format!("{id}: {result:?}"),
                            &[],
                        );
                        deadline = Some(Instant::now());
                    }
                    Message::Frame => {
                        // 关窗校验进行中：即使窗口已失焦也要出帧，否则「是否保存」对话框
                        // 不可见、不可点。close 在确认/取消后会复位。
                        if (!active || !visible) && !close {
                            return Ok(());
                        }
                        if !render.attached() {
                            return Ok(());
                        }
                        let size = egui::vec2(
                            render.width as f32 / config.scale,
                            render.height as f32 / config.scale,
                        );
                        let mut raw = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            time: Some(started.elapsed().as_secs_f64()),
                            events: std::mem::take(&mut events),
                            system_theme: appearance::current(),
                            ..Default::default()
                        };
                        let viewport = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
                        viewport.native_pixels_per_point = Some(config.scale);
                        viewport.inner_rect = raw.screen_rect;
                        viewport.focused = Some(true);
                        let validating_close = close;
                        if close {
                            viewport.events.push(egui::ViewportEvent::Close);
                            close = false;
                        }
                        // vectorcraft 的宿主缝（与桌面 eframe::App 一致）：raw_input_hook 做迭代边界 +
                        // 填充 raw；tick 在 logic 相；finish_ime_logic 收尾；ui 在 run_ui 相。
                        app.raw_input_hook(&ctx, &mut raw);
                        let output = editor_frame(&ctx, &mut app, raw);
                        for command in &output.platform_output.commands {
                            if let egui::OutputCommand::OpenUrl(url) = command {
                                bridge::notify(0, "url", &url.url, &[]);
                            }
                        }
                        // egui 文本焦点变化 → 通知 ArkTS 弹起/收起软键盘（只在变化时发）。
                        let ime_wanted = output.platform_output.ime.is_some();
                        if ime_wanted != ime_shown {
                            ime_shown = ime_wanted;
                            bridge::notify(0, "ime", if ime_wanted { "show" } else { "hide" }, &[]);
                        }
                        let mut commands = Vec::new();
                        let mut repaint_delay: Option<Duration> = None;
                        if let Some(v) = output.viewport_output.get(&egui::ViewportId::ROOT) {
                            commands.extend(v.commands.clone());
                            repaint_delay = Some(v.repaint_delay);
                        }
                        // 上游把「保存 / 丢弃已完成」写进 ui.status（桌面宿主据此发
                        // ViewportCommand::Close，见 apps/vectorcraft/src/app.rs:159）。鸿蒙宿主
                        // 没有 eframe 包装层，必须自己锁存该信号，否则用户点完 Save / Don't Save
                        // 后窗口永远不关。CancelClose（有未保存 / 文件选择器占用）优先级最高，
                        // 交给上游的「是否保存」对话框；用户取消则不发通知。
                        quit_requested |= app.ui.status == "quit";
                        if !terminating {
                            match close_action(validating_close, &commands, quit_requested) {
                                CloseAction::Confirm => {
                                    bridge::notify(
                                        0,
                                        "diagnostic",
                                        "close guard: confirmed, notifying host",
                                        &[],
                                    );
                                    bridge::notify(0, "close", "", &[]);
                                    terminating = true;
                                }
                                CloseAction::Cancel => {
                                    // 上游弹了「是否保存」：窗口保留，继续出帧等用户作答。
                                    if validating_close {
                                        bridge::notify(
                                            0,
                                            "diagnostic",
                                            "close guard: saveChanges pending, window kept",
                                            &[],
                                        );
                                    }
                                    deadline = Some(Instant::now());
                                }
                                CloseAction::None => {}
                            }
                        }
                        let painted = render.paint(&ctx, output)?;
                        if first_frame_marker(painted) {
                            bridge::notify(0, "first_frame", "Native UI presented", &[]);
                        }
                        // 按 egui 的 `repaint_delay` 排下一帧：视口在动时它是 0（连续重绘），
                        // 静止时是一个很大的值或 MAX（等输入/vsync 唤醒），避免空转耗电。
                        if painted && (active || close) && visible && render.attached() {
                            deadline = match repaint_delay {
                                Some(delay) if delay.is_zero() => Some(Instant::now()),
                                Some(delay) if delay < Duration::from_secs(3600) => {
                                    Some(Instant::now() + delay)
                                }
                                _ => None,
                            };
                        }
                    }
                }
                Ok(())
            },
        ));
        let _ = result;
    }
}

#[cfg(target_env = "ohos")]
#[allow(dead_code)]
fn first_frame_marker(painted: bool) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRST: AtomicBool = AtomicBool::new(false);
    if painted && !FIRST.swap(true, Ordering::AcqRel) {
        true
    } else {
        false
    }
}

#[cfg(target_env = "ohos")]
fn editor_frame(
    ctx: &egui::Context,
    app: &mut VectorcraftApp,
    raw: egui::RawInput,
) -> egui::FullOutput {
    // 与 eframe `App::logic`、web shell `logic`、GridCraft `editor_frame` 一致：logic
    // （tick + IME 收尾）必须在 run_ui 的 pass **内**、ui() 之前，tick 才能读到本帧 raw 的
    // viewport 状态（尤其 `close_requested()`）。放在 pass 外会读到上一帧 input，关窗事件
    // 慢一帧，宿主当帧收不到 CancelClose 便误判「无未保存」而静默关闭窗口。
    ctx.run_ui(raw, |ui| {
        if ui.ctx().current_pass_index() == 0 {
            app.tick(ui.ctx());
            app.finish_ime_logic(ui.ctx());
        }
        app.ui(ui);
    })
}

#[cfg(target_env = "ohos")]
#[derive(Debug, PartialEq, Eq)]
enum CloseAction {
    None,
    Cancel,
    Confirm,
}

#[cfg(target_env = "ohos")]
fn close_action(
    validating: bool,
    commands: &[egui::ViewportCommand],
    quit_requested: bool,
) -> CloseAction {
    if commands.contains(&egui::ViewportCommand::CancelClose) {
        // 上游还有未保存文档或文件选择器占用：暂缓关闭，等用户在「是否保存」里作答。
        CloseAction::Cancel
    } else if validating {
        // 本帧已注入 ViewportEvent::Close 且上游没有发 CancelClose：无未保存文档，放行。
        // 成立的前提是 tick 在本帧 run_ui pass 内（见 editor_frame），否则这一帧永远
        // 看不到上游的 CancelClose，会把有未保存改动的窗口直接关掉。
        CloseAction::Confirm
    } else if quit_requested || commands.contains(&egui::ViewportCommand::Close) {
        // 用户已选保存 / 丢弃（ui.status == "quit"），或 egui 主动请求关闭：直接放行，
        // 不再注入一次 ViewportEvent::Close 让上游复查（那会重复弹框）。
        CloseAction::Confirm
    } else {
        CloseAction::None
    }
}

/// 平台服务（OHOS 宿主与桌面不同的关键点，与 gridcraft 交付模型一致）：
/// - open：上游 `io/open.rs` **依赖 `Services.read`**（无则报 "no file reader"）。OHOS 沙箱路径
///   可被 `std::fs` 读，故以 `std::fs` 实现 read/read_head/exists/write；打开经 bridge 弹 ArkTS
///   选择器（kind "open"），把 picker 授权文件复制进沙箱后回传**真实沙箱路径**，上游按路径 read。
/// - save/export：走**web 式**（`download` 非空 → 上游 `is_web` 成立，`pick_finished` 直接用建议名、
///   `write_to` 走 `download(name, bytes)`，**无需 `pick_save`**）。字节经 bridge（kind "download"）
///   交给 ArkTS，ArkTS 弹保存位置选择器 + fileIo 写盘。这与 gridcraft 已验证的 ArkTS 处理
///   （`open`/`file` + `download` 两个分支）完全对齐，零 ArkTS 逻辑改动。
/// - 桌面专属服务（system_clipboard/print/reveal/open_file/pick_folder/screen_color/…）鸿蒙首版
///   留空（None），对应菜单项在上游会给出"不可用"提示。
#[cfg(target_env = "ohos")]
fn services(files: &str) -> Services {
    let mut s = Services::default();
    let files = files.to_string();
    // 上游 io/open.rs `read()` 无 reader 即报错；用 std::fs 读沙箱路径。
    s.read = Some(Box::new(|p: &str| std::fs::read(p).map_err(|e| e.to_string())));
    s.read_head = Some(Box::new(|p: &str, max: usize| read_file_head(p, max)));
    // 保存覆盖当前文档（已有沙箱路径）时上游用 services.write；web 式下载路径不受影响。
    s.write = Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string())));
    s.exists = Some(Box::new(|p: &str| std::path::Path::new(p).exists()));
    // 打开：经 bridge 弹 ArkTS 选择器，拿沙箱路径。上游闭包收 `&FilePick`（建议名/过滤器），
    // 鸿蒙首版忽略它，仅按 open 语义取一条路径。
    s.pick_open = Some(Box::new(move |_: &FilePick| file_picker("open", &files)));
    // 另存为 / 导出：web 式下载（见上方说明）——把编码后字节交给 ArkTS 写盘（同步请求，等写盘结果）。
    s.download = Some(Box::new(|name: &str, bytes: &[u8]| {
        match bridge::request("download", name, bytes) {
            Ok(_) => {
                log::info!("[vc] download saved: {name} ({} bytes)", bytes.len());
                Ok(())
            }
            Err(e) => {
                log::warn!("[vc] download failed ({name}): {e}");
                Err(e)
            }
        }
    }));
    s.pick_save = None;
    // 打开外链：经 bridge 通知 ArkTS（best-effort）。
    s.open_url = Some(Box::new(|url: &str| bridge::notify(0, "url", url, &[])));
    s
}

/// 把沙箱文件路径交给上游 `open_path`（读字节 → `document.open`）。内部自行处理密码/错误。
#[cfg(target_env = "ohos")]
fn open_into_app(app: &mut VectorcraftApp, path: &str) {
    match open_path(app, path) {
        Ok(_) => log::info!("[vc] open_path ok: {path}"),
        Err(e) => {
            log::warn!("[vc] open_path failed ({path}): {e}");
            bridge::notify(0, "error", &e, &[]);
        }
    }
}

/// 文件选择器：在独立线程里同步向 ArkTS 请求文件，拿到沙箱路径后交给上游 open/save 逻辑。
#[cfg(target_env = "ohos")]
fn file_picker(kind: &str, text: &str) -> Option<String> {
    struct Inner {
        value: Mutex<Option<Option<String>>>,
        waker: Mutex<Option<Waker>>,
    }
    impl Wake for Inner {
        fn wake(self: Arc<Self>) {
            if let Some(w) = self.waker.lock().unwrap().take() {
                w.wake();
            }
        }
    }
    struct Answer {
        inner: Arc<Inner>,
    }
    impl std::future::Future for Answer {
        type Output = Option<String>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Some(v) = self.inner.value.lock().unwrap().take() {
                return Poll::Ready(v);
            }
            *self.inner.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
    let inner = Arc::new(Inner {
        value: Mutex::new(None),
        waker: Mutex::new(None),
    });
    let answer = Answer { inner: inner.clone() };
    let kind = kind.to_string();
    let text = text.to_string();
    let join = std::thread::spawn(move || {
        log::info!("[vc] file_picker request kind={kind} text={text:?}");
        let path = match bridge::request(&kind, &text, &[]) {
            // ArkTS 已在沙箱准备好文件；直接把回传路径交给上游 open/save 逻辑。
            Ok((name, _bytes)) if !name.is_empty() => Some(name),
            Ok((name, _bytes)) => {
                log::warn!("[vc] file_picker kind={kind} returned an empty name (len={})", name.len());
                None
            }
            Err(e) => {
                // 用户在系统选择器点「取消」时，ArkTS 以 "cancelled" 回传——这是正常流程，不是错误。
                let lower = e.to_ascii_lowercase();
                if lower == "cancelled" || lower.contains("cancel") {
                    log::info!("[vc] file_picker kind={kind} cancelled by user ({e})");
                } else {
                    log::warn!("[vc] file_picker kind={kind} failed: {e}");
                    bridge::notify(0, "error", &e, &[]);
                }
                None
            }
        };
        log::info!("[vc] file_picker kind={kind} resolved path={path:?}");
        *inner.value.lock().unwrap() = Some(path);
        Arc::clone(&inner).wake();
    });
    let path = pollster::block_on(answer);
    let _ = join.join();
    path
}

#[cfg(not(target_env = "ohos"))]
fn worker(_config: Config, _rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "error", "Native host requires an OHOS target", &[]);
}
