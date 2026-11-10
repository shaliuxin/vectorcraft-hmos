import native from 'libvectorcraft.so';
import { AbilityConstant, Configuration, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';
import { publishSystemTheme, systemThemeFromColorMode } from '../SystemTheme';

class SystemThemeInput {
  kind: string = 'system_theme';
  text: string = '';
  constructor(text: string) { this.text = text; }
}

export default class EntryAbility extends UIAbility {
  private syncSystemTheme(colorMode: number | undefined): void {
    const theme = systemThemeFromColorMode(colorMode);
    publishSystemTheme(theme);
    // Push live changes into the native worker; init also carries the first value.
    try {
      native.input(JSON.stringify(new SystemThemeInput(theme)));
    } catch (error) {
      console.info('System theme input unavailable before native init');
    }
  }

  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    this.syncSystemTheme(this.context.config?.colorMode);
    // 冷启动的「打开方式」：want.uri 只出现在 onCreate，必须写进 AppStorage 让页面消费；
    // 只处理 onNewWant 会让首次从文件管理器点开的应用丢掉文件。
    if (want?.uri) {
      AppStorage.setOrCreate('vectorcraftOpenWant', want.uri);
    }
    console.info('VectorCraft native ability created');
  }

  onNewWant(want: Want): void {
    // 应用已在前台时，系统通过 onNewWant 送来新的文件 URI；页面用 @StorageLink/@Watch 立即消费。
    if (want?.uri) {
      AppStorage.setOrCreate('vectorcraftOpenWant', want.uri);
    }
  }

  onConfigurationUpdate(newConfig: Configuration): void {
    this.syncSystemTheme(newConfig.colorMode);
  }

  onForeground(): void {
    native.active(true);
  }

  onBackground(): void {
    native.active(false);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    // 系统标题栏的 X 由窗口管理器直接执行，不经过 egui；不拦下来就会绕过上游的
    // 「是否保存」确认直接销毁宿主。与 GridCraft / DeckCraft 真机验证过的写法一致：
    // 在窗口级 windowWillClose 里**无条件**调 native.close() 并返回 true 拦截本次关闭，
    // 由 Rust 检查未保存文档；Rust 确认后 notify("close") → Index.receive 主动 terminateSelf
    // （terminateSelf 不会再触发 windowWillClose）。用户点 Cancel 时 Rust 不发通知，窗口保留。
    // SDK 声明 `Callback<void, Promise<boolean>>`（@since 15）：true = 拦截，false = 放行。
    // 只订阅窗口级 event，不与 windowStageClose / onPrepareToTerminate 同时注册（优先级冲突）。
    try {
      const mainWindow = windowStage.getMainWindowSync();
      mainWindow.on('windowWillClose', async (): Promise<boolean> => {
        console.info('VectorCraft window X: intercepted, forwarding to native close validation');
        try {
          native.close();
          return true;
        } catch (error) {
          // native 未就绪时不能把窗口锁死：放行系统关闭作为兜底（保持旧行为）。
          console.warn(`VectorCraft native close unavailable: ${(error as Error).message}`);
          return false;
        }
      });
      console.info('VectorCraft close interceptor registered');
    } catch (error) {
      console.warn(`VectorCraft windowWillClose unavailable: ${(error as Error).message}`);
    }
    if (AppStorage.get<string>('vectorcraftSystemTheme') === undefined) {
      this.syncSystemTheme(this.context.config?.colorMode);
    }
    windowStage.loadContent('pages/Index', (error: BusinessError) => {
      if (error.code) {
        console.error(`VectorCraft page failed: ${error.code} ${error.message}`);
      }
    });
  }
}
