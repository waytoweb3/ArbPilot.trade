import { useEffect } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  AccountError,
  activateDevice,
  cancelSubscription,
  deviceFingerprint,
  fetchLicenseToken,
  fetchMe,
  fetchSubscriptions,
  logout,
  revokeDevice,
} from "../lib/accounts";
import { api, getExecutorMode, setLicenseToken } from "../lib/api";
import { useSessionStore } from "../lib/session";
import type { ExecutorLicense } from "../lib/types";

export function useAccount() {
  const token = useSessionStore((state) => state.token);
  const setToken = useSessionStore((state) => state.setToken);
  const query = useQuery({
    queryKey: ["account", token],
    queryFn: () => fetchMe(token!),
    enabled: Boolean(token),
    retry: 1,
    staleTime: 30_000,
    refetchInterval: 60_000,
  });

  // 会话过期或被撤销时清掉本地令牌，避免一直用失效身份请求。
  useEffect(() => {
    if (query.error instanceof AccountError && query.error.status === 401) {
      setToken(null);
    }
  }, [query.error, setToken]);

  return query;
}

export function useAccountSubscriptions() {
  const token = useSessionStore((state) => state.token);
  return useQuery({
    queryKey: ["account", "subscriptions", token],
    queryFn: () => fetchSubscriptions(token!),
    enabled: Boolean(token),
    retry: 1,
    staleTime: 30_000,
  });
}

export function useCancelAccountSubscription() {
  const token = useSessionStore((state) => state.token);
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (subscriptionId: string) => cancelSubscription(token!, subscriptionId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["account"] });
    },
  });
}

export function useRevokeDevice() {
  const token = useSessionStore((state) => state.token);
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (deviceId: string) => revokeDevice(token!, deviceId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["account"] });
    },
  });
}

export function useLogout() {
  const token = useSessionStore((state) => state.token);
  const setToken = useSessionStore((state) => state.setToken);
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async () => {
      if (token) {
        await logout(token);
      }
    },
    onSettled: () => {
      setToken(null);
      queryClient.removeQueries({ queryKey: ["account"] });
      // 令牌清空后 useLicenseSync 会撤销执行器授权，这里刷新一下状态卡。
      void queryClient.invalidateQueries({ queryKey: ["license"] });
    },
  });
}

/**
 * 把账号授权同步到执行器：先登记本机设备，再用设备指纹换取短期令牌。
 *
 * 执行器只用内置公钥离线验签，账号服务不可达时已安装的授权继续生效。
 */
async function applyLicense(token: string | null): Promise<ExecutorLicense> {
  if (!token) {
    setLicenseToken(null);
    return api.clearLicense();
  }
  await activateDevice(token);
  const grant = await fetchLicenseToken(token, deviceFingerprint());
  // 令牌装进执行器，同时留给访问官方只读数据接口的请求头使用。
  setLicenseToken(grant.token);
  return api.installLicense(grant.token);
}

/** 执行器当前的授权状态；ARBPILOT_LICENSE_REQUIRED 关闭时 required 为 false。 */
export function useExecutorLicense() {
  return useQuery({
    queryKey: ["license"],
    queryFn: () => api.licenseStatus(),
    retry: 1,
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** 手动重新同步授权，例如订阅生效或执行器重启后。 */
export function useSyncLicense() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: applyLicense,
    onSuccess: (license) => {
      queryClient.setQueryData(["license"], license);
    },
  });
}

/** 登录状态或套餐变化时自动同步；失败时保留执行器里已有的授权。 */
export function useLicenseSync() {
  const token = useSessionStore((state) => state.token);
  const account = useAccount();
  const { mutate } = useSyncLicense();
  // 支付回写后 entitlement 变化，重新签发令牌才能立刻解锁新模块。
  const entitlement = account.data?.entitlement;
  const signature = entitlement
    ? `${entitlement.status}:${entitlement.plans.join(",")}:${entitlement.expiresAt ?? ""}`
    : "";
  useEffect(() => {
    mutate(token);
  }, [token, signature, mutate]);

  // 内置执行器模式靠每个请求携带用户令牌访问官方只读数据接口，
  // 令牌有效期 24 小时，定时换新避免中途失效。
  useEffect(() => {
    if (!token || getExecutorMode() === "self") {
      return;
    }
    const timer = window.setInterval(() => mutate(token), 6 * 60 * 60 * 1_000);
    return () => window.clearInterval(timer);
  }, [token, mutate]);
}
