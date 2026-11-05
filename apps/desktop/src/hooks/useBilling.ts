import { useMutation, useQuery } from "@tanstack/react-query";

import { createCheckout, fetchPlans, type BillingPlanId } from "../lib/billing";

export function useBillingPlans() {
  return useQuery({
    queryKey: ["billing", "plans"],
    queryFn: fetchPlans,
    retry: 1,
    staleTime: 5 * 60_000,
  });
}

export function useCreateCheckout() {
  return useMutation({
    mutationFn: (input: {
      plan: BillingPlanId;
      email?: string;
      locale: "zh" | "en";
      darkMode: boolean;
      /** 账号服务签发的会话令牌；未登录时服务端会拒绝。 */
      session: string;
    }) => createCheckout(input),
  });
}
