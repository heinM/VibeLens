import { Component, type ErrorInfo, type ReactNode } from "react";

interface State {
  error: Error | null;
  componentStack: string | null;
}

/**
 * Last line of defence: a render error anywhere shows this screen instead of a blank window.
 * `scope` gives a compact inline variant for panels (agent, results) so one panel's crash doesn't
 * take the rest of the app with it.
 */
export class ErrorBoundary extends Component<{ children: ReactNode; scope?: string }, State> {
  state: State = { error: null, componentStack: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ componentStack: info.componentStack ?? null });
    console.error(`[VibeLens] ${this.props.scope ?? "app"} crashed:`, error, info.componentStack);
  }

  private details() {
    const { error, componentStack } = this.state;
    return `${error?.name}: ${error?.message}\n\n${error?.stack ?? ""}\n\nComponent stack:${componentStack ?? " (unavailable)"}`;
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    if (this.props.scope) {
      return (
        <div className="flex h-full flex-col items-center justify-center gap-2 p-6 text-center">
          <div className="text-[13px] font-medium text-fg">The {this.props.scope} hit an error</div>
          <div className="selectable max-w-md text-[12px] text-muted">{error.message}</div>
          <button
            type="button"
            className="mt-1 h-7 rounded-md border border-border bg-panel-2 px-3 text-[12px] text-fg hover:border-muted/60"
            onClick={() => this.setState({ error: null, componentStack: null })}
          >
            Try again
          </button>
        </div>
      );
    }

    return (
      <div className="flex h-full items-center justify-center bg-bg p-8 text-fg">
        <div className="w-full max-w-2xl rounded-xl border border-border bg-panel p-6 shadow-2xl">
          <h1 className="text-base font-semibold">VibeLens hit an unexpected error</h1>
          <p className="mt-1 text-[13px] text-muted">
            Your connections, saved worksheets and keychain secrets are unaffected. Reload to continue; if it keeps happening, copy the details into an issue.
          </p>
          <pre className="selectable mt-4 max-h-72 overflow-auto rounded-md border border-border bg-bg p-3 font-mono text-[11.5px] leading-relaxed text-danger">
            {this.details()}
          </pre>
          <div className="mt-4 flex gap-2">
            <button type="button" className="h-8 rounded-md bg-accent px-3 text-[13px] font-medium text-accent-fg" onClick={() => window.location.reload()}>
              Reload
            </button>
            <button
              type="button"
              className="h-8 rounded-md border border-border bg-panel-2 px-3 text-[13px]"
              onClick={() => void navigator.clipboard?.writeText(this.details()).catch(() => {})}
            >
              Copy details
            </button>
          </div>
        </div>
      </div>
    );
  }
}
