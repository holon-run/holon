export interface ResumeReconciliationScheduler {
  setTimeout(callback: () => void, delayMs: number): number;
  clearTimeout(timer: number): void;
}

export class ResumeReconciliationCoordinator {
  private timer: number | undefined;
  private inFlight: Promise<void> | undefined;
  private rerunRequested = false;

  constructor(
    private readonly reconcile: () => Promise<void>,
    private readonly scheduler: ResumeReconciliationScheduler,
    private readonly delayMs = 100,
  ) {}

  schedule(): void {
    if (this.timer != null) {
      this.scheduler.clearTimeout(this.timer);
    }
    this.timer = this.scheduler.setTimeout(() => {
      this.timer = undefined;
      this.run();
    }, this.delayMs);
  }

  dispose(): void {
    if (this.timer != null) {
      this.scheduler.clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.rerunRequested = false;
  }

  private run(): void {
    if (this.inFlight) {
      this.rerunRequested = true;
      return;
    }

    const reconciliation = this.reconcile();
    this.inFlight = reconciliation;
    const settle = () => {
      if (this.inFlight !== reconciliation) return;
      this.inFlight = undefined;
      if (!this.rerunRequested) return;
      this.rerunRequested = false;
      this.schedule();
    };
    void reconciliation.then(settle, settle);
  }
}
