import { create } from "zustand";

import { getSessionToken, setSessionToken } from "./accounts";

interface SessionState {
  token: string | null;
  setToken: (token: string | null) => void;
}

/** 账号会话只保存在本机 localStorage；退出登录即清除。 */
export const useSessionStore = create<SessionState>((set) => ({
  token: getSessionToken(),
  setToken: (token) => {
    setSessionToken(token);
    set({ token });
  },
}));
