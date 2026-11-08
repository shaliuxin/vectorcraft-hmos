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
