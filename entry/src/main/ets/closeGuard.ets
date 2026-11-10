// 关闭守卫：与 WordCraft / DeckCraft / GridCraft 同一套机制。
// 系统窗口的关闭按钮由窗口管理器直接执行，不经过 egui；不拦下来就会绕过上游的
// `unsaved::close_all`（Save / Don't Save / Cancel）直接销毁宿主、丢失未保存改动。
// 这里把系统关窗先拦住，转发给 Rust 的 ViewportEvent::Close 流程检查未保存文档；
// Rust 确认后才 notify("close") → ArkTS terminateSelf()。
let requestPageClose: (() => void) | null = null;
let confirmedClose: boolean = false;

export function resetCloseGuard(): void {
  confirmedClose = false;
  requestPageClose = null;
}

// 页面初始化完成后注册关闭处理器（native.close）；页面销毁时清掉。
export function setCloseRequest(handler: (() => void) | null): void {
  requestPageClose = handler;
}

// 系统发起关闭时调用：返回 true 拦截本次关闭，交给 Rust 检查未保存文档；
// 已确认放行（confirmedClose）或没有注册处理器时返回 false，放行系统关闭。
export function requestWindowClose(): boolean {
  if (confirmedClose || requestPageClose === null) {
    return false;
  }
  requestPageClose();
  return true;
}

// Rust 确认可以关闭（无未保存改动，或用户已选保存 / 丢弃）后调用，放行后续关闭。
export function allowConfirmedClose(): void {
  confirmedClose = true;
}

export function restoreCloseGuard(): void {
  confirmedClose = false;
}
