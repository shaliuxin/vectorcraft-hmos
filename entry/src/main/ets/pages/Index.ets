import native from 'libvectorcraft.so';
import { common } from '@kit.AbilityKit';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { i18n } from '@kit.LocalizationKit';
import { BusinessError, deviceInfo } from '@kit.BasicServicesKit';
import { display } from '@kit.ArkUI';
import { inputMethod } from '@kit.IMEKit';
import { SYSTEM_THEME_DARK, SYSTEM_THEME_LIGHT } from '../SystemTheme';

// Mirrors the Rust `Config` deserialized in native/rust/src/lib.rs.
class NativeConfig {
  files: string;
  documents: string;
  scale: number;
  language: string;
  systemLocale: string;
  displayP3: boolean;
  systemTheme: string;
  fullscreen: boolean;
  deviceType: string;
  widthVp: number;
  constructor(files: string, documents: string, scale: number, language: string, systemLocale: string,
              displayP3: boolean, systemTheme: string, fullscreen: boolean, deviceType: string, widthVp: number) {
    this.files = files;
    this.documents = documents;
    this.scale = scale;
    this.language = language;
    this.systemLocale = systemLocale;
    this.displayP3 = displayP3;
    this.systemTheme = systemTheme;
    this.fullscreen = fullscreen;
    this.deviceType = deviceType;
    this.widthVp = widthVp;
  }
}

/**
 * 系统语言的完整 BCP-47 标签（如 `zh-Hans-CN`）：Rust 侧用它决定界面语言，也据此挑中文字体。
 * getSystemLocale 在个别设备上可能不可用，退回只带语言的 getSystemLanguage。
 */
function systemLocaleTag(): string {
  try {
    const locale = i18n.System.getSystemLocale();
    if (locale && locale.length > 0) {
      return locale;
    }
  } catch (error) {
    console.info(`System locale unavailable: ${(error as Error).message}`);
  }
  return i18n.System.getSystemLanguage();
}

/**
 * 对文件名或 file:// URI 里的百分号（%xx）解码，得到真实可读名（如 文档1）。
 * vectorcraft 用回传路径的末段（file_stem）作文档标题，若不先解码，标题/信息页/最近列表
 * 会显示原始百分号串（%E6%96%87…）。
 * - 含 `://`（完整 URI）：只解码各路径段，保留 scheme 与分隔符。
 * - 其余（裸文件名）：整体解码。
 * 解码失败则原样返回，绝不影响原值。
 */
function percentDecodeName(s: string): string {
  if (!s || s.length === 0) { return s; }
  try {
    if (s.includes('://')) {
      const parts = s.split('/');
      for (let i = 1; i < parts.length; i++) { parts[i] = decodeURIComponent(parts[i]); }
      return parts.join('/');
    }
    return decodeURIComponent(s);
  } catch (error) {
    return s;
  }
}

/**
 * 把 file:// URI 各路径段重新百分号编码（保留 scheme 与分隔符），用于把「解码后的名」
 * 还原成 picker 原始 URI 形式，供 fileIo.openSync 打开写入（兜底：若解码形式打不开则用此）。
 */
function percentEncodePath(uri: string): string {
  try {
    const parts = uri.split('/');
    for (let i = 1; i < parts.length; i++) { parts[i] = encodeURIComponent(parts[i]); }
    return parts.join('/');
  } catch (error) {
    return uri;
  }
}

class NativeMessage { kind: string = ''; text: string = ''; }

@Entry
@Component
struct Index {
  @State private failure: string = '';
  @State private initialized: boolean = false;
  @State private startupVisible: boolean = true;
  @State private startupOpacity: number = 1;
  private alive: boolean = false;
  private generation: number = 0;
  private context: common.UIAbilityContext = this.getUIContext().getHostContext() as common.UIAbilityContext;
  @State private deviceScale: number = 1;
  // 中文输入法（官方「自定义编辑框」方案，与 DeckCraft/WordCraft/GridCraft/SolidCraft 同一修法）：
  // attach 前必须先有一个获焦的 ArkUI 编辑组件（XComponent 原生表面不算），
  // 且监听器必须在 attach 成功之后注册（提前注册抛 12800009，键盘完全不弹）。
  private imeController: inputMethod.InputMethodController | undefined = undefined;
  private imeAttached: boolean = false;
  private imeShown: boolean = false;
  private imeInComposition: boolean = false;
  private imeBound: boolean = false;
  private imeWanted: boolean = false;
  // 系统「打开方式」：EntryAbility 把 want.uri 写进 AppStorage。应用已在前台时页面不会重新
  // aboutToAppear，必须用 @StorageLink/@Watch 监听变化并立即消费，否则文件永远打不开。
  @StorageLink('vectorcraftOpenWant') @Watch('onOpenWantChanged') private openWantUri: string = '';

  aboutToAppear(): void {
    console.info('VectorCraft native page appearing');
    this.alive = true;
    this.generation++;
    this.initializeHost(this.generation);
    // 冷启动：onCreate 已把 want.uri 放进 AppStorage，首帧初始化完成后消费一次。
    this.drainOpenWant();
    // 启动兜底：遮罩盖在 XComponent 之上，若首帧通知因任何原因没到，它会永久遮住原生画面
    // （表现为满屏漆黑）。到点必须揭开。
    setTimeout((): void => { this.hideStartup(); }, 4000);
  }

  aboutToDisappear(): void {
    this.alive = false;
    this.generation++;
    this.initialized = false;
    native.active(false);
  }

  private async initializeHost(generation: number): Promise<void> {
    try {
      let scale: number = this.getUIContext().vp2px(1);
      this.deviceScale = scale;
      let systemTheme = AppStorage.get<string>('vectorcraftSystemTheme') ?? '';
      if (systemTheme !== SYSTEM_THEME_DARK && systemTheme !== SYSTEM_THEME_LIGHT) {
        systemTheme = '';
      }
      const filesDir = this.context.filesDir;
      const documents = filesDir + '/Documents';
      try { fileIo.mkdirSync(documents, true); } catch (e) { /* exists */ }
      native.init(JSON.stringify(new NativeConfig(
          filesDir, documents, scale, i18n.System.getSystemLanguage(), systemLocaleTag(), false, systemTheme,
          false, deviceInfo.deviceType, this.getUIContext().px2vp(display.getDefaultDisplaySync().width))),
        (id: number, message: string, data: ArrayBuffer): void => { this.receive(id, message, data); });
      this.initialized = true;
      this.failure = '';
    } catch (error) {
      if (this.alive && generation === this.generation) { this.failure = (error as Error).message; }
    }
  }

  private onOpenWantChanged(): void {
    this.drainOpenWant();
  }

  private drainOpenWant(): void {
    if (!this.alive || !this.initialized) { return; }
    const uri = this.openWantUri;
    if (!uri || uri.length === 0) { return; }
    this.openWantUri = '';   // 双向写回 AppStorage，避免同一 URI 被重复打开
    this.openWant(uri);
  }

  private openWant(uri: string): void {
    try {
      const path = new fileUri.FileUri(uri).path;
      const rawName = path.length > 0 ? path.substring(path.lastIndexOf('/') + 1) : 'document';
      const name = percentDecodeName(rawName);
      let bytes: ArrayBuffer;
      try {
        // want / picker 授予的是 URI 级临时读权限，必须优先用原始 URI 打开。
        const file = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
        bytes = this.readAll(file.fd);
        fileIo.closeSync(file);
      } catch (uriErr) {
        console.warn(`VectorCraft open want: URI open failed (${(uriErr as Error).message}); trying path`);
        const file = fileIo.openSync(percentDecodeName(path), fileIo.OpenMode.READ_ONLY);
        bytes = this.readAll(file.fd);
        fileIo.closeSync(file);
      }
      native.open(name, bytes);
      console.info(`VectorCraft open want: ${name} (${bytes.byteLength} bytes)`);
    } catch (error) {
      const err = error as Error;
      console.error(`VectorCraft open want failed: ${err.message}\n${err.stack ?? ''}`);
    }
  }

  private readAll(fd: number): ArrayBuffer {
    const chunks: ArrayBuffer[] = [];
    let total = 0;
    const bufSize = 1024 * 1024;
    while (true) {
      const buf = new ArrayBuffer(bufSize);
      const n = fileIo.readSync(fd, buf);
      if (n <= 0) { break; }
      chunks.push(n === bufSize ? buf : buf.slice(0, n));
      total += n;
    }
    const out = new Uint8Array(total);
    let offset = 0;
    for (const c of chunks) {
      out.set(new Uint8Array(c), offset);
      offset += c.byteLength;
    }
    return out.buffer;
  }

  private receive(id: number, message: string, data: ArrayBuffer): void {
    const request = JSON.parse(message) as NativeMessage;
    console.info(`VectorCraft native ${request.kind}: ${request.text}`);
    if (!this.alive) {
      if (id > 0) { native.reply(id, false, 'cancelled', new ArrayBuffer(0)); }
      return;
    }
    if (request.kind === 'error') {
      this.failure = request.text;
    } else if (request.kind === 'ready' || request.kind === 'first_frame') {
      this.failure = '';
      if (request.kind === 'first_frame') { this.revealEditor(); }
    } else if (request.kind === 'close') {
      this.context.terminateSelf();
    } else if (request.kind === 'url') {
      this.context.startAbility({ action: 'ohos.want.action.viewData', uri: request.text })
        .catch((error: BusinessError) => { console.error(`External link: ${error.message}`); });
    } else if (request.kind === 'open' || request.kind === 'file') {
      this.pickDocument(id, false);
    } else if (request.kind === 'download') {
      // VectorCraft 没有 host_save：另存为 / PDF 导出把文件字节交给宿主（见 Rust services()）。
      // 宿主先弹保存选择器拿到 picker 授权 URI，再用 fileIo 把字节写盘，最后回包唤醒 Rust worker。
      this.downloadSave(id, request.text, data);
    } else if (request.kind === 'ime') {
      // Rust 通知：egui 文本字段获得/失去焦点 → 弹起/收起软键盘。
      this.setImeKeyboard(request.text === 'show');
    }
  }

  // ===================== 中文输入法（IME） =====================
  // 鸿蒙软键盘中文输入法走 InputMethod 框架，不经 XComponent 原始按键事件。
  // 顺序（真机实测，不可颠倒）：锚点获焦 → attach(true) → **attach 成功后**注册监听。

  private imeControllerOrGet(): inputMethod.InputMethodController | undefined {
    if (this.imeController) {
      return this.imeController;
    }
    try {
      const controller: inputMethod.InputMethodController = inputMethod.getController();
      this.imeController = controller;
      return controller;
    } catch (error) {
      console.error(`VectorCraft IME controller failed: ${(error as Error).message}`);
      return undefined;
    }
  }

  private bindImeListeners(controller: inputMethod.InputMethodController): void {
    if (this.imeBound) {
      return;
    }
    try {
      // insertText = 已确认上屏文本；setPreviewText = 拼音组合态；finishTextPreview = 组合结束；
      // deleteLeft = 退格（组合态内由键盘自行处理）；sendFunctionKey = 回车；sendKeyboardStatus = 显隐。
      controller.on('insertText', (text: string): void => { this.sendIme('commit', text); });
      controller.on('setPreviewText', (text: string, _range: inputMethod.Range): void => {
        this.imeInComposition = (text.length > 0);
        this.sendIme('preedit', text);
      });
      controller.on('finishTextPreview', (): void => { this.imeInComposition = false; this.sendIme('preedit', ''); });
      controller.on('deleteLeft', (length: number): void => {
        if (!this.imeInComposition) { this.sendIme('delete', String(length)); }
      });
      controller.on('sendFunctionKey', (_key: inputMethod.FunctionKey): void => { this.sendIme('enter', ''); });
      controller.on('sendKeyboardStatus', (status: inputMethod.KeyboardStatus): void => {
        this.imeShown = (status === inputMethod.KeyboardStatus.SHOW);
      });
      this.imeBound = true;
      console.info('VectorCraft IME listeners bound');
    } catch (error) {
      console.warn(`VectorCraft IME bind failed: ${(error as Error).message}`);
    }
  }

  private async setImeKeyboard(show: boolean): Promise<void> {
    if (!this.alive) {
      return;
    }
    this.imeWanted = show;
    const controller = this.imeControllerOrGet();
    if (!controller) {
      return;
    }
    if (show) {
      try {
        this.getUIContext().getFocusController().requestFocus('vectorcraft-ime-anchor');
      } catch (error) {
        console.warn(`VectorCraft IME anchor focus failed: ${(error as Error).message}`);
      }
      try {
        await controller.attach(true, this.imeTextConfig());
        this.imeAttached = true;
        this.imeShown = true;
        console.info('VectorCraft IME attached');
      } catch (error) {
        console.warn(`VectorCraft IME attach failed: ${(error as Error).message}`);
        try {
          await controller.showTextInput(inputMethod.RequestKeyboardReason.OTHER);
          this.imeAttached = true;
          this.imeShown = true;
        } catch (e2) {
          console.warn(`VectorCraft IME show failed: ${(e2 as Error).message}`);
        }
      }
      if (this.imeAttached) {
        this.bindImeListeners(controller);
        try {
          // PC 模式/外接键盘下 attach(true) 可能只出现悬浮工具栏，TOUCH 理由尝试强制展开面板。
          await controller.showTextInput(inputMethod.RequestKeyboardReason.TOUCH);
        } catch (error) {
          console.warn(`VectorCraft IME touch-show failed: ${(error as Error).message}`);
        }
      }
    } else {
      this.imeInComposition = false;
      try {
        if (this.imeAttached) { await controller.hideTextInput(); }
      } catch (error) {
        console.warn(`VectorCraft IME hide failed: ${(error as Error).message}`);
      }
      try {
        if (this.imeAttached) { await controller.detach(); }
      } catch (error) {
        // 未绑定 / 重复 detach：忽略。
      }
      this.imeAttached = false;
      this.imeShown = false;
    }
  }

  private imeTextConfig(): inputMethod.TextConfig {
    return {
      inputAttribute: {
        textInputType: inputMethod.TextInputType.TEXT,
        enterKeyType: inputMethod.EnterKeyType.NEWLINE,
        consumeKeyEvents: true,
      },
    };
  }

  private sendIme(op: string, text: string): void {
    if (!this.alive) {
      return;
    }
    native.input(JSON.stringify({ kind: 'ime', op, text }));
  }

  private async pickDocument(id: number, _gallery: boolean): Promise<void> {
    const generation = this.generation;
    try {
      const uris = await new picker.DocumentViewPicker(this.context).select(new picker.DocumentSelectOptions());
      if (!this.alive || generation !== this.generation || uris.length === 0) {
        console.info(`VectorCraft pick open: cancelled (alive=${this.alive} gen ${generation}!=${this.generation} n=${uris.length})`);
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      const uriStr = uris[0];
      const info = new fileUri.FileUri(uriStr);
      // 文件名先解码，落沙箱的临时名与 vectorcraft 显示的文档名都应是真实可读名（而非 %xx 串）。
      const baseName = percentDecodeName(info.name || uriStr.substring(uriStr.lastIndexOf('/') + 1) || 'document');
      console.info(`VectorCraft pick open: uri=${uriStr} path=${info.path} name=${baseName}`);

      // 把 picker 授权的文件复制进沙箱 .inbox，Rust 侧再用 std::fs 按路径读取。
      // 关键：fileIo.copyFileSync 原生支持 picker 返回的 file:// URI（含非 ASCII 名），
      // 直接完成 URI→沙箱复制；不要自己 open+writeSync 循环——writeSync 的 buffer 参数
      // 必须是 ArrayBuffer/string，传 Uint8Array 视图会触发 "Invalid arraybuffer or options"。
      const inbox = this.context.filesDir + '/.inbox';
      try {
        fileIo.mkdirSync(inbox, true);
      } catch (e) {
        // 目录已存在。
      }
      const dst = inbox + '/' + Date.now() + '_' + baseName;
      try {
        fileIo.copyFileSync(uriStr, dst);
      } catch (uriErr) {
        // 极少数情况下按 URI 复制失败，退回用解码后的真实路径复制。
        console.warn(`VectorCraft pick open: copySync by URI failed (${(uriErr as Error).message}); trying decoded path`);
        fileIo.copyFileSync(info.path, dst);
      }
      const size = fileIo.statSync(dst).size;
      console.info(`VectorCraft pick open: copied ${size} bytes -> ${dst}`);
      // 把沙箱路径回传给 Rust（open/file 读此路径，save 写此路径）。
      native.reply(id, true, dst, new ArrayBuffer(0));
    } catch (error) {
      const err = error as Error;
      console.error(`VectorCraft pick open FAILED: ${err.message}\n${err.stack ?? ''}`);
      native.reply(id, false, String(err.message ?? error), new ArrayBuffer(0));
    }
  }

  // 另存为 / 导出：弹系统保存位置选择器（DocumentViewPicker.save），把 VectorCraft 交来的
  // 文件字节（data）写到用户所选的 picker 授权 URI。沙箱外位置只能经授权 URI 用 fileIo 写，
  // Rust std::fs 写不了，故 VectorCraft 的 saveAs / PDF 导出统一走 download(name, bytes)。
  private async downloadSave(id: number, name: string, bytes: ArrayBuffer): Promise<void> {
    const generation = this.generation;
    try {
      if (!this.alive || generation !== this.generation) {
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      const suggestedName = name.length > 0 ? name : 'Untitled.svg';
      const options = new picker.DocumentSaveOptions();
      options.newFileNames = [suggestedName];
      const uris = await new picker.DocumentViewPicker(this.context).save(options);
      if (!this.alive || generation !== this.generation || !uris || uris.length === 0) {
        console.info(`VectorCraft download: picker cancelled (alive=${this.alive} n=${uris ? uris.length : 0})`);
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      console.info(`VectorCraft download: picker -> ${uris[0]} (name=${suggestedName}, ${bytes.byteLength} bytes)`);
      // 写盘 + 回包（唤醒阻塞中的 Rust worker），复用 host_save 时代的字节写入逻辑。
      this.writeSave(id, uris[0], bytes);
    } catch (error) {
      const err = error as Error;
      console.error(`VectorCraft download FAILED: ${err.message}`);
      native.reply(id, false, String(err.message ?? error), new ArrayBuffer(0));
    }
  }

  // 把文件字节写到 picker 授权的 URI（沙箱外位置）。ArrayBuffer 恰为字节长度（见 C++ CallJS）。
  // downloadSave 传入的是 DocumentViewPicker.save 返回的原始 URI；写完后回包唤醒 Rust worker。
  private writeSave(id: number, uri: string, bytes: ArrayBuffer): void {
    if (!uri || uri.length === 0) {
      native.reply(id, false, 'no save target uri', new ArrayBuffer(0));
      return;
    }
    // downloadSave 回传的是 picker 原始 URI。写入时优先按此 URI 打开；
    // 若系统 fileIo 要求百分号编码形式，则回退到编码形式重试——两条都试，保证写盘成功。
    const targets: string[] = [uri];
    const enc = percentEncodePath(uri);
    if (enc !== uri) { targets.push(enc); }
    let lastErr = '';
    for (const target of targets) {
      try {
        const file = fileIo.openSync(target, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC);
        try {
          fileIo.writeSync(file.fd, bytes);
        } finally {
          fileIo.closeSync(file);
        }
        console.info(`VectorCraft save: wrote ${bytes.byteLength} bytes -> ${target}`);
        native.reply(id, true, target, new ArrayBuffer(0));
        return;
      } catch (error) {
        lastErr = String((error as Error).message ?? error);
        console.warn(`VectorCraft save write: openSync failed for ${target}: ${lastErr}`);
      }
    }
    console.error(`VectorCraft save write FAILED: ${lastErr}`);
    native.reply(id, false, lastErr, new ArrayBuffer(0));
  }

  private hideStartup(): void {
    if (!this.alive || !this.startupVisible) { return; }
    this.startupOpacity = 0;
    this.startupVisible = false;
  }

  private revealEditor(): void {
    if (!this.startupVisible) { return; }
    // 遮罩必须无条件消失：动画在任何环节失败都不能把原生画面永久盖住。
    try {
      this.getUIContext().animateTo({ duration: 320, curve: Curve.EaseOut, onFinish: (): void => this.hideStartup() },
        (): void => { this.startupOpacity = 0; });
    } catch (error) {
      console.warn(`Reveal animation failed: ${(error as Error).message}`);
      this.hideStartup();
    }
    // 兜底：动画若被系统丢弃（回调线程/丢帧），首帧之后必须让原生画面露出来。
    setTimeout((): void => this.hideStartup(), 600);
  }

  build() {
    Stack() {
      if (this.initialized) {
        XComponent({ id: 'vectorcraft-native', type: XComponentType.SURFACE, libraryname: 'vectorcraft' })
          .width('100%').height('100%')
          .onLoad((): void => {
            try { this.getUIContext().getFocusController().requestFocus('vectorcraft-editor'); } catch (e) {}
          })
          // 不设 focusOnTouch：打字期间点击画布若把焦点抢回 XComponent（非编辑代理），
          // 会断开输入法绑定导致键盘收起；触摸/指针事件不经 ArkUI 焦点，不受影响。
          .id('vectorcraft-editor').focusable(true).defaultFocus(true)
      }
      // 隐藏的 IME 锚点：输入法框架只认「获得焦点的编辑组件」为可输入位置，
      // attach 前必须先聚焦它（官方自定义编辑框方案）。
      Text('')
        .id('vectorcraft-ime-anchor')
        .focusable(true)
        .width(1).height(1)
        .opacity(0)
        .hitTestBehavior(HitTestMode.None)
        .onBlur((): void => {
          if (this.imeAttached) {
            this.imeAttached = false;
            this.imeShown = false;
            console.info('VectorCraft IME anchor blurred, binding reset');
          }
        })
      if (this.startupVisible) {
        Column() {
          Column({ space: 18 }) {
            Image($r('app.media.app_icon')).width(76).height(76).borderRadius(18)
            Text('VectorCraft').fontSize(30).fontWeight(FontWeight.Medium).fontColor('#F4F4F5')
            Row({ space: 10 }) {
              LoadingProgress().width(18).height(18).color('#A5BFFF')
              Text(this.initialized ? '正在启动编辑器…' : '正在准备文档目录…')
                .fontSize(14).fontColor('#B8BAC2')
            }
          }
        }
        .width('100%').height('100%').justifyContent(FlexAlign.Center)
        .backgroundColor('#10203F').opacity(this.startupOpacity)
        .hitTestBehavior(HitTestMode.Block)
      }
      if (this.failure.length > 0) {
        Column() {
          Text('VectorCraft 原生运行错误').fontSize(20).fontColor('#FFFFFF')
          Text(this.failure).fontSize(14).fontColor('#FFFFFF').margin({ top: 12 })
          if (!this.initialized) {
            Button('重试').onClick((): void => { this.failure = ''; this.initializeHost(this.generation); })
          } else {
            Button('关闭提示').onClick((): void => { this.failure = ''; })
          }
        }.padding(24).backgroundColor('#DD262626').borderRadius(8).width('80%')
      }
    }.width('100%').height('100%').backgroundColor('#262626')
  }
}
