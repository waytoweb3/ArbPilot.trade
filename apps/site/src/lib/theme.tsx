import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type ThemeMode = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "arbpilot.theme";
const QUERY = "(prefers-color-scheme: dark)";

function readStored(): ThemeMode {
  // 构建期预渲染在 Node 中执行，没有 localStorage。
  if (typeof window === "undefined") {
    return "system";
  }
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system" ? stored : "system";
}

function systemTheme(): ResolvedTheme {
  if (typeof window === "undefined") {
    return "dark";
  }
  return window.matchMedia(QUERY).matches ? "dark" : "light";
}

function resolve(mode: ThemeMode): ResolvedTheme {
  return mode === "system" ? systemTheme() : mode;
}

let currentMode: ThemeMode = readStored();
let currentResolved: ResolvedTheme = resolve(currentMode);

function apply(resolved: ResolvedTheme) {
  if (typeof document === "undefined") {
    return;
  }
  document.documentElement.classList.toggle("dark", resolved === "dark");
}

apply(currentResolved);

export function getResolvedTheme(): ResolvedTheme {
  return currentResolved;
}

type ThemeValue = {
  mode: ThemeMode;
  resolved: ResolvedTheme;
  setMode: (mode: ThemeMode) => void;
  toggleTheme: () => void;
};

const ThemeContext = createContext<ThemeValue | null>(null);

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>(() => currentMode);
  const [resolved, setResolved] = useState<ResolvedTheme>(() => currentResolved);

  useEffect(() => {
    const applyResolved = (next: ResolvedTheme) => {
      currentResolved = next;
      apply(next);
      setResolved(next);
    };

    applyResolved(resolve(mode));

    if (mode !== "system") {
      return;
    }

    const media = window.matchMedia(QUERY);
    const listener = (event: MediaQueryListEvent) => {
      applyResolved(event.matches ? "dark" : "light");
    };
    media.addEventListener("change", listener);
    return () => media.removeEventListener("change", listener);
  }, [mode]);

  const setMode = useCallback((next: ThemeMode) => {
    currentMode = next;
    localStorage.setItem(STORAGE_KEY, next);
    setModeState(next);
  }, []);

  const toggleTheme = useCallback(() => {
    setMode(getResolvedTheme() === "dark" ? "light" : "dark");
  }, [setMode]);

  const value = useMemo<ThemeValue>(
    () => ({ mode, resolved, setMode, toggleTheme }),
    [mode, resolved, setMode, toggleTheme],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeValue {
  const value = useContext(ThemeContext);
  if (!value) {
    throw new Error("useTheme must be used inside ThemeProvider");
  }
  return value;
}
