import { useMutation, useQuery } from "@tanstack/react-query";

import {
  createCheckout,
  fetchLicense,
  fetchPlans,
  type BillingPlanId,
} from "../lib/billing";

export function useBillingPlans() {
  return useQuery({
    queryKey: ["billing", "plans"],
    queryFn: fetchPlans,
    retry: 1,
    staleTime: 5 * 60_000,
  });
}

export function useLicense(ref: string | null) {
  return useQuery({
    queryKey: ["billing", "license", ref],
    queryFn: () => fetchLicense(ref!),
    enabled: Boolean(ref),
    retry: 1,
    refetchInterval: 5_000,
  });
}

export function useCreateCheckout() {
  return useMutation({
    mutationFn: (input: {
      plan: BillingPlanId;
      email?: string;
      locale: "zh" | "en";
      darkMode: boolean;
    }) => createCheckout(input),
  });
}
