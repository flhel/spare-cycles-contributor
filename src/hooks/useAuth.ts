import { useCallback, useState } from "react";
import { login as apiLogin, register as apiRegister } from "../lib/api";
import { clearSession, loadSession, saveSession } from "../lib/session";
import type { AuthSession } from "../types";

export function useAuth() {
  const [session, setSession] = useState<AuthSession | null>(loadSession);

  const login = useCallback(async (email: string, password: string) => {
    const newSession = await apiLogin(email, password);
    saveSession(newSession);
    setSession(newSession);
  }, []);

  const register = useCallback(async (email: string, password: string) => {
    const newSession = await apiRegister(email, password);
    saveSession(newSession);
    setSession(newSession);
  }, []);

  const logout = useCallback(() => {
    clearSession();
    setSession(null);
  }, []);

  return { session, login, register, logout };
}
