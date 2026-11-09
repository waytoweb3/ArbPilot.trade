import { Button } from "./button";

interface ConfirmDialogProps {
  open: boolean;
  /** 可选标题；不传时只显示说明文字。 */
  title?: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  /** 提交中：禁用按钮，防止重复确认。 */
  pending?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * 应用内确认弹窗。
 *
 * Tauri 的 macOS WebView（wry）没有实现 JS 对话框委托，`window.confirm()`
 * 会直接返回 false，导致确认永远不成立；因此所有二次确认都改用这个原生 React 弹窗。
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  cancelLabel,
  pending = false,
  error,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      role="dialog"
      aria-modal="true"
    >
      <div className="w-full max-w-sm rounded-xl border border-line bg-panel shadow-xl">
        {title && (
          <div className="border-b border-line px-4 py-3">
            <p className="text-sm font-semibold text-fg">{title}</p>
          </div>
        )}
        <div className="space-y-3 p-4">
          <p className="text-xs leading-relaxed text-muted">{message}</p>
          {error && (
            <p className="break-words text-[11px] leading-relaxed text-danger">{error}</p>
          )}
          <div className="flex justify-end gap-2">
            <Button size="sm" variant="secondary" onClick={onCancel} disabled={pending}>
              {cancelLabel}
            </Button>
            <Button size="sm" variant="danger" onClick={onConfirm} disabled={pending}>
              {confirmLabel}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
