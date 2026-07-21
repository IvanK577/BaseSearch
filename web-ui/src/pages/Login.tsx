import { useState, type FormEvent } from "react";

import { api, ApiError } from "../api/client";
import { Icon } from "../components/Icon";
import { useStore } from "../state/store";

export function LoginScreen() {
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
    } catch (err) {
      setError((err as ApiError)?.message ?? "Sign-in failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="welcome">
      <form className="panel welcome-card" style={{ maxWidth: 380 }} onSubmit={submit}>
        <div className="welcome-mark">
          <Icon name="flame" size={34} />
        </div>
        <h1 style={{ fontSize: 26 }}>Base Search</h1>
        <p>Sign in to continue.</p>
        <div className="stack" style={{ gap: 12, textAlign: "left" }}>
          <div>
            <label className="field-label">Username</label>
            <input
              className="input"
              value={username}
              autoFocus
              autoComplete="username"
              onChange={(e) => setUsername(e.target.value)}
            />
          </div>
          <div>
            <label className="field-label">Password</label>
            <input
              className="input"
              type="password"
              value={password}
              autoComplete="current-password"
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
          {error ? <div className="banner">{error}</div> : null}
          <button
            className="btn btn-primary"
            type="submit"
            disabled={busy || !username.trim() || !password}
          >
            {busy ? "Signing in…" : "Sign in"}
          </button>
        </div>
      </form>
    </div>
  );
}
