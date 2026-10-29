import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Navigate, useLocation } from "react-router";
import { useQuery, useMutation } from "urql";
import { authRuntimeStateQuery } from "@/lib/graphql/queries";
import { completeInitialAuthenticationMutation } from "@/lib/graphql/mutations";
import {
  AUTH_SESSION_CHANGED_EVENT,
  useAuth,
  type AuthUser,
} from "@/lib/hooks/use-auth";
import type { AuthRuntimeState } from "@/lib/types/settings";
import { useTranslate } from "@/lib/context/translate-context";
import { userFacingGraphQlErrorMessage } from "@/lib/graphql/error-message";
import { getRuntimeBasePath } from "@/lib/runtime-config";
import {
  initialAuthenticationView,
  initialAuthenticationReloadPath,
  canCompleteInitialAuthentication,
} from "@/lib/utils/initial-authentication";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SetupIntroLoading } from "./setup-intro";
import { SetupRestoreView } from "./setup-restore-view";

export function InitialAuthenticationGate({
  children,
}: {
  children: ReactNode;
}) {
  const t = useTranslate();
  const location = useLocation();
  const [{ data, error, fetching }, refresh] = useQuery<{
    authRuntimeState: AuthRuntimeState;
  }>({
    query: authRuntimeStateQuery,
    requestPolicy: "network-only",
  });
  const stage = initialAuthenticationView(
    data?.authRuntimeState?.initialAuthenticationStage,
  );
  const [observedStage, setObservedStage] =
    useState<ReturnType<typeof initialAuthenticationView>>("error");
  const reloadPath = initialAuthenticationReloadPath(observedStage, stage);
  const authoritative = !fetching && !error && data && stage !== "error";
  if (authoritative && observedStage !== stage && !reloadPath) {
    setObservedStage(stage);
  }
  useEffect(() => {
    if (!authoritative || !reloadPath) return;
    const base = getRuntimeBasePath();
    window.location.replace(`${base === "/" ? "" : base}${reloadPath}`);
  }, [authoritative, reloadPath]);
  const pending = stage === "choice" || stage === "password_replacement";
  useEffect(() => {
    const recheck = () => refresh({ requestPolicy: "network-only" });
    window.addEventListener(AUTH_SESSION_CHANGED_EVENT, recheck);
    window.addEventListener("focus", recheck);
    return () => {
      window.removeEventListener(AUTH_SESSION_CHANGED_EVENT, recheck);
      window.removeEventListener("focus", recheck);
    };
  }, [refresh]);
  useEffect(() => {
    if (pending) refresh({ requestPolicy: "network-only" });
  }, [location.key, pending, refresh]);
  if (fetching && (!data || observedStage === "error"))
    return <SetupIntroLoading />;
  if (error || !data || stage === "error")
    return (
      <SetupFrame>
        <h1 className="text-2xl font-semibold">{t("initialAuth.title")}</h1>
        <p role="alert">{t("initialAuth.loadFailed")}</p>
        <Button onClick={() => refresh({ requestPolicy: "network-only" })}>
          {t("initialAuth.retry")}
        </Button>
      </SetupFrame>
    );
  if (reloadPath) return <SetupIntroLoading />;
  if (stage === "choice")
    return (
      <AuthenticationChoice
        state={data.authRuntimeState}
        onStale={() => refresh({ requestPolicy: "network-only" })}
      />
    );
  if (stage === "password_replacement" && location.pathname !== "/login")
    return <Navigate to="/login?redirect=%2Fsetup" replace />;
  return children;
}

function SetupFrame({ children }: { children: ReactNode }) {
  return (
    <main className="min-h-screen bg-background px-6 py-16 text-foreground">
      <div className="mx-auto flex max-w-lg flex-col gap-6 rounded-xl border bg-card p-8">
        {children}
      </div>
    </main>
  );
}

function AuthenticationChoice({
  state,
  onStale,
}: {
  state: AuthRuntimeState;
  onStale: () => void;
}) {
  const t = useTranslate();
  const { adoptSession } = useAuth();
  const [mode, setMode] = useState<"PASSWORD" | "NO_AUTHENTICATION" | null>(
    null,
  );
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [setupCode, setSetupCode] = useState("");
  const [restoring, setRestoring] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [{ fetching }, complete] = useMutation<{
    completeInitialAuthentication: {
      completed: boolean;
      login: {
        token: string;
        persistSession: boolean;
        user: AuthUser | null;
      } | null;
    };
  }>(completeInitialAuthenticationMutation);
  const minimum = state.passwordMinLength ?? 8;
  const blockedByEnvironment = (choice: string) =>
    state.envOverrideActive &&
    (choice === "PASSWORD") !== state.effectiveFormLoginEnabled;
  const canSubmit = canCompleteInitialAuthentication({
    mode,
    password,
    confirmation,
    minimum,
    acknowledged,
    codeRequired: state.initialAuthenticationCodeRequired === true,
    code: setupCode,
    environmentMode: state.envOverrideActive
      ? state.effectiveFormLoginEnabled
      : null,
  });
  const submit = useCallback(
    async (event: React.FormEvent) => {
      event.preventDefault();
      if (!canSubmit || fetching) return;
      setError(null);
      const result = await complete({
        input: {
          mode,
          password: mode === "PASSWORD" ? password : null,
          acknowledgeNoAuthentication:
            mode === "NO_AUTHENTICATION" && acknowledged,
          setupCode: state.initialAuthenticationCodeRequired ? setupCode : null,
        },
      });
      if (
        result.error ||
        !result.data?.completeInitialAuthentication.completed
      ) {
        setError(
          userFacingGraphQlErrorMessage(
            result.error,
            t("initialAuth.saveFailed"),
            t,
          ),
        );
        onStale();
        return;
      }
      const login = result.data.completeInitialAuthentication.login;
      if (login) adoptSession(login.token, login.user, login.persistSession);
      const base = getRuntimeBasePath();
      window.location.assign(`${base === "/" ? "" : base}/setup`);
    },
    [
      canSubmit,
      fetching,
      complete,
      mode,
      password,
      acknowledged,
      state.initialAuthenticationCodeRequired,
      setupCode,
      t,
      onStale,
      adoptSession,
    ],
  );
  if (restoring) {
    return (
      <SetupFrame>
        <SetupRestoreView
          t={t}
          setupCode={state.initialAuthenticationCodeRequired ? setupCode : null}
          onBack={() => setRestoring(false)}
        />
      </SetupFrame>
    );
  }
  return (
    <SetupFrame>
      <h1 className="text-2xl font-semibold">{t("initialAuth.title")}</h1>
      <p className="text-muted-foreground">{t("initialAuth.description")}</p>
      <form onSubmit={(event) => void submit(event)} className="space-y-6">
        <fieldset disabled={fetching} className="space-y-3">
          <legend className="mb-3 font-medium">
            {t("initialAuth.choose")}
          </legend>
          {(["PASSWORD", "NO_AUTHENTICATION"] as const).map((choice) => (
            <label key={choice} className="flex gap-3 rounded-lg border p-4">
              <input
                type="radio"
                name="authentication-mode"
                value={choice}
                checked={mode === choice}
                disabled={blockedByEnvironment(choice)}
                onChange={() => {
                  setMode(choice);
                  setError(null);
                }}
              />
              <span>
                {t(
                  choice === "PASSWORD"
                    ? "initialAuth.passwordMode"
                    : "initialAuth.noAuthMode",
                )}
              </span>
            </label>
          ))}
        </fieldset>
        {state.envOverrideActive && <p>{t("initialAuth.environment")}</p>}
        {mode === "PASSWORD" && (
          <div className="space-y-3">
            <p>{t("initialAuth.admin", { minLength: minimum })}</p>
            <label className="block">
              {t("initialAuth.password")}
              <Input
                type="password"
                autoComplete="new-password"
                required
                minLength={minimum}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            <label className="block">
              {t("initialAuth.confirm")}
              <Input
                type="password"
                autoComplete="new-password"
                required
                value={confirmation}
                onChange={(e) => setConfirmation(e.target.value)}
              />
            </label>
            {confirmation && password !== confirmation && (
              <p role="alert">{t("initialAuth.mismatch")}</p>
            )}
          </div>
        )}
        {mode === "NO_AUTHENTICATION" && (
          <label className="flex gap-3">
            <input
              type="checkbox"
              checked={acknowledged}
              onChange={(e) => setAcknowledged(e.target.checked)}
            />
            <span>{t("initialAuth.acknowledge")}</span>
          </label>
        )}
        {state.initialAuthenticationCodeRequired && (
          <label className="block">
            {t("initialAuth.code")}
            <Input
              autoComplete="one-time-code"
              value={setupCode}
              maxLength={32}
              onChange={(e) => setSetupCode(e.target.value)}
              required
              aria-describedby="setup-code-help"
            />
            <span
              id="setup-code-help"
              className="mt-2 block text-sm text-muted-foreground"
            >
              {t("initialAuth.codeHelp")}
            </span>
          </label>
        )}
        {error && <p role="alert">{error}</p>}
        <Button
          type="submit"
          disabled={!canSubmit || fetching}
          className="w-full"
        >
          {t(
            fetching
              ? "initialAuth.saving"
              : mode === "NO_AUTHENTICATION"
                ? "initialAuth.continueWithout"
                : "initialAuth.continue",
          )}
        </Button>
      </form>
      <Button
        type="button"
        variant="outline"
        disabled={
          fetching ||
          (state.initialAuthenticationCodeRequired && !setupCode.trim())
        }
        onClick={() => setRestoring(true)}
      >
        {t("setup.restoreTitle")}
      </Button>
    </SetupFrame>
  );
}
