import { Component, type ErrorInfo, type ReactNode } from "react";

interface ErrorBoundaryProps {
  children: ReactNode;
  onReload?: () => void;
}

interface ErrorBoundaryState {
  failed: boolean;
}

export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("Base Search interface render failed", error, info);
  }

  private reload = (): void => {
    if (this.props.onReload) {
      this.props.onReload();
    } else {
      window.location.reload();
    }
  };

  render(): ReactNode {
    if (!this.state.failed) return this.props.children;

    return (
      <main className="fatal-state" role="alert">
        <div className="fatal-state-content">
          <h1>Base Search could not display this screen</h1>
          <p>
            Your data and running server are unaffected. Reload the interface to
            continue.
          </p>
          <button type="button" className="btn btn-primary" onClick={this.reload}>
            Reload Base Search
          </button>
        </div>
      </main>
    );
  }
}
