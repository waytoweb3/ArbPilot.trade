import { create } from "zustand";

interface ExecutorUiState {
  streamConnected: boolean;
  lastEventAt: string | null;
  lastEventType: string | null;
  setStreamConnected: (connected: boolean) => void;
  pushEvent: (type: string) => void;
}

export const useExecutorStore = create<ExecutorUiState>((set) => ({
  streamConnected: false,
  lastEventAt: null,
  lastEventType: null,
  setStreamConnected: (connected) => set({ streamConnected: connected }),
  pushEvent: (type) =>
    set({ lastEventType: type, lastEventAt: new Date().toISOString() }),
}));
