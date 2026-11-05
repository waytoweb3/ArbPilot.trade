import { create } from "zustand";

/** 执行器推送的一条通知，来源为新闻采集或资金费率套利机会。 */
export interface NoticeItem {
  id: string;
  kind: "news" | "funding";
  title: string;
  message: string;
  at: string;
}

interface ExecutorUiState {
  streamConnected: boolean;
  lastEventAt: string | null;
  lastEventType: string | null;
  /** 最近的应用内通知，最多保留 4 条。 */
  notices: NoticeItem[];
  setStreamConnected: (connected: boolean) => void;
  pushEvent: (type: string) => void;
  pushNotice: (notice: Omit<NoticeItem, "id">) => void;
  dismissNotice: (id: string) => void;
}

/** 通知 id 自增序号，避免同一毫秒内的多条通知共用 React key。 */
let noticeSeq = 0;

export const useExecutorStore = create<ExecutorUiState>((set) => ({
  streamConnected: false,
  lastEventAt: null,
  lastEventType: null,
  notices: [],
  setStreamConnected: (connected) => set({ streamConnected: connected }),
  pushEvent: (type) =>
    set({ lastEventType: type, lastEventAt: new Date().toISOString() }),
  pushNotice: (notice) =>
    set((state) => ({
      notices: [{ ...notice, id: `${Date.now()}-${noticeSeq++}` }, ...state.notices].slice(0, 4),
    })),
  dismissNotice: (id) =>
    set((state) => ({ notices: state.notices.filter((notice) => notice.id !== id) })),
}));
