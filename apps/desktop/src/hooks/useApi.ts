import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../lib/api";
import type { CreateOrderRequest, StrategyStatus } from "../lib/types";

export function useHealth() {
  return useQuery({
    queryKey: ["health"],
    queryFn: api.health,
    refetchInterval: 5_000,
  });
}

export function useOpportunities() {
  return useQuery({
    queryKey: ["opportunities"],
    queryFn: api.opportunities,
    refetchInterval: 2_000,
  });
}

export function usePositions() {
  return useQuery({
    queryKey: ["positions"],
    queryFn: api.positions,
    refetchInterval: 5_000,
  });
}

export function useOrders() {
  return useQuery({
    queryKey: ["orders"],
    queryFn: api.orders,
    refetchInterval: 5_000,
  });
}

export function useStrategies() {
  return useQuery({
    queryKey: ["strategies"],
    queryFn: api.strategies,
    refetchInterval: 10_000,
  });
}

export function useRiskLimits() {
  return useQuery({
    queryKey: ["risk-limits"],
    queryFn: api.riskLimits,
    staleTime: Number.POSITIVE_INFINITY,
  });
}

export function usePauseExecutor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (reason?: string) => api.pause(reason),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["health"] }),
  });
}

export function useResumeExecutor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.resume(),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["health"] }),
  });
}

export function useCreateOrder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ body, idempotencyKey }: { body: CreateOrderRequest; idempotencyKey: string }) =>
      api.createOrder(body, idempotencyKey),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["orders"] });
      queryClient.invalidateQueries({ queryKey: ["positions"] });
      queryClient.invalidateQueries({ queryKey: ["health"] });
    },
  });
}

export function useUpdateStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, status }: { id: string; status: StrategyStatus }) =>
      api.updateStrategy(id, status),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["strategies"] }),
  });
}
