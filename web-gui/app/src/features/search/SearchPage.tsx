export function SearchPage() {
  return (
    <section className="page placeholder-page" aria-label="Search">
      <div className="page-inner">
        <section className="summary-panel empty-state">
          <span className="eyebrow">Global search</span>
          <h1>Search</h1>
          <p>
            Cross-agent lookup for messages, briefs, WorkItems, tool executions, and memory records. This remains a
            shell until Dashboard and Agent conversation are stable.
          </p>
        </section>
      </div>
    </section>
  );
}
