import type { RuntimeConnection } from "../../runtime/types";

interface SettingsPageProps {
  connection: RuntimeConnection;
}

export function SettingsPage({ connection }: SettingsPageProps) {
  return (
    <section className="page placeholder-page" aria-label="Settings">
      <div className="page-inner">
        <section className="summary-panel empty-state">
          <span className="eyebrow">Runtime configuration</span>
          <h1>Settings</h1>
          <p>
            Local connection, providers, model defaults, and runtime posture. Current connection:{" "}
            <strong>{connection.mode}</strong> · {connection.summary}.
          </p>
        </section>
      </div>
    </section>
  );
}
