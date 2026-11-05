import { create } from "zustand";

interface AccountDialogState {
  open: boolean;
  /** true 表示切换账号：先退出当前账号，再让站点强制重新选择账号。 */
  switching: boolean;
  show: (switching?: boolean) => void;
  close: () => void;
}

/**
 * 账号登录弹窗的共享开关：顶栏头像与「设置 → 账号与账单」都能打开同一个弹窗，
 * 避免两处各写一套设备码登录流程。
 */
export const useAccountDialog = create<AccountDialogState>((set) => ({
  open: false,
  switching: false,
  show: (switching = false) => set({ open: true, switching }),
  close: () => set({ open: false, switching: false }),
}));
