import native from 'libvectorcraft.so';
import { AbilityConstant, Configuration, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';
import { publishSystemTheme, systemThemeFromColorMode } from '../SystemTheme';
import { requestWindowClose, resetCloseGuard } from '../closeGuard';

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
    resetCloseGuard();
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
    // 系统标题栏的 X 由窗口管理器直接执行，不经过 egui；首版点 X 会直接退出、
    // 不弹「是否保存」。这里把系统关闭请求拦下来，转给 Rust 已有的
    // ViewportEvent::Close 流程（native.close → 未保存确认 → notify "close"）。
    // 依据 craft 检查清单 §8：只订阅窗口级 windowWillClose（@since 15，回调必须返回
    // Promise<boolean>），不与 windowStageClose / onPrepareToTerminate 同时注册以免优先级冲突。
    // true = 拦截（窗口保留），false = 放行；terminateSelf 属其它关闭方式，不会再触发此事件。
    try {
      const mainWindow = windowStage.getMainWindowSync();
      mainWindow.on('windowWillClose', async (): Promise<boolean> => {
        try {
          return requestWindowClose();
        } catch (error) {
          // native 未就绪时不能把窗口锁死：放行系统关闭作为兜底。
          console.warn(`VectorCraft close guard unavailable: ${(error as Error).message}`);
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
