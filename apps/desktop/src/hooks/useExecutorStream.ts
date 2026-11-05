import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";

import { api } from "../lib/api";
import { useExecutorStore } from "../state/executor";

/** 订阅执行器 WebSocket 事件；断线时自动重连（指数退避上限 10s）。 */
export function useExecutorStream() {
  const queryClient = useQueryClient();
  const setStreamConnected = useExecutorStore((state) => state.setStreamConnected);
  const pushEvent = useExecutorStore((state) => state.pushEvent);
  const pushNotice = useExecutorStore((state) => state.pushNotice);

  useEffect(() => {
    let socket: WebSocket | null = null;
    let disposed = false;
    let retry = 0;
    let timer: number | undefined;

    const connect = () => {
      socket = new WebSocket(api.eventsUrl());

      socket.onopen = () => {
        retry = 0;
        setStreamConnected(true);
      };

      socket.onclose = () => {
        setStreamConnected(false);
        if (disposed) {
          return;
        }
        retry = Math.min(retry + 1, 5);
        timer = window.setTimeout(connect, Math.min(1_000 * 2 ** retry, 10_000));
      };

      socket.onmessage = (event) => {
        try {
          const payload = JSON.parse(String(event.data)) as {
            type?: string;
            kind?: "news" | "funding";
            title?: string;
            message?: string;
            at?: string;
          };
          const type = payload.type ?? "unknown";
          pushEvent(type);
          if (type === "notice" && payload.title && payload.message) {
            pushNotice({
              kind: payload.kind ?? "news",
              title: payload.title,
              message: payload.message,
              at: payload.at ?? new Date().toISOString(),
            });
          }
          if (type === "order_updated") {
            queryClient.invalidateQueries({ queryKey: ["orders"] });
            queryClient.invalidateQueries({ queryKey: ["positions"] });
          }
          if (type === "executor_status") {
            queryClient.invalidateQueries({ queryKey: ["health"] });
          }
        } catch {
          // 忽略无法解析的事件，保持连接。
        }
      };
    };

    connect();

    return () => {
      disposed = true;
      window.clearTimeout(timer);
      socket?.close();
    };
  }, [pushEvent, pushNotice, queryClient, setStreamConnected]);
}
