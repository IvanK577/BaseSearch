import { useState, type FormEvent } from "react";

import { api, ApiError } from "../api/client";
import { Icon } from "../components/Icon";
import { Banner, Spinner } from "../components/ui";
import { useI18n, type Translate } from "../lib/i18n";
import { useStore } from "../state/store";

function loginErrorMessage(error: unknown, t: Translate): string {
  if (error instanceof ApiError) {
    if (error.code === "network") return t("login_unavailable");
    if (error.status === 401 || error.code === "invalid_credentials") {
      return t("login_invalid_credentials");
    }
  }
  return t("login_failed");
}

export function LoginScreen() {
  const { t } = useI18n();
  const { refreshAuth } = useStore();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.login(username.trim(), password);
      refreshAuth();
    } catch (nextError) {
      setError(loginErrorMessage(nextError, t));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="login-shell">
      <header className="login-brand">
        <img
          className="login-brand-mark"
          src="/base-search-icon.png"
          alt={t("appName")}
          width="36"
          height="36"
        />
        <div>
          <div className="brand-name">{t("appName")}</div>
          <div className="brand-sub">{t("tagline")}</div>
        </div>
      </header>

      <main className="login-main">
        <section className="login-surface" aria-labelledby="login-title">
          <div className="login-context">
            <Icon name="users" size={16} />
            <span>{t("login_trusted_lan")}</span>
          </div>
          <h1 id="login-title">{t("login_sign_in")}</h1>
          <p id="login-hint">{t("login_hint")}</p>

          <form className="login-form" onSubmit={submit} noValidate>
            <div>
              <label className="field-label" htmlFor="login-username">
                {t("settings_username")}
              </label>
              <input
                id="login-username"
                className="input"
                value={username}
                autoFocus
                autoComplete="username"
                aria-invalid={Boolean(error)}
                aria-describedby={error ? "login-hint login-error" : "login-hint"}
                onChange={(event) => setUsername(event.target.value)}
              />
            </div>
            <div>
              <label className="field-label" htmlFor="login-password">
                {t("settings_password")}
              </label>
              <input
                id="login-password"
                className="input"
                type="password"
                value={password}
                autoComplete="current-password"
                aria-invalid={Boolean(error)}
                aria-describedby={error ? "login-hint login-error" : "login-hint"}
                onChange={(event) => setPassword(event.target.value)}
              />
            </div>
            {error ? (
              <div id="login-error">
                <Banner>{error}</Banner>
              </div>
            ) : null}
            <button
              className="btn btn-primary login-submit"
              type="submit"
              disabled={busy || !username.trim() || !password}
            >
              {busy ? <Spinner /> : null}
              {busy ? t("login_signing_in") : t("login_sign_in")}
            </button>
          </form>
        </section>
      </main>
    </div>
  );
}

export { loginErrorMessage };
