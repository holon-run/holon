import type { FileReference, ResolveFileReferenceResult, ResolveFileReferencesResponse } from "../../runtime/types";

type Resolver = (references: FileReference[]) => Promise<ResolveFileReferencesResponse>;
const failure = (message: string): ResolveFileReferenceResult => ({ status: "unresolved", reason: "unavailable", message });

/** Identity changes invalidate both cached locations and outstanding replies. */
export class ReferenceCache {
  private epoch = 0;
  private scope = "";
  private values = new Map<string, { result: ResolveFileReferenceResult; expires: number }>();
  private pending = new Map<string, Promise<ResolveFileReferenceResult>>();
  constructor(private now = Date.now) {}
  setScope(scope: string): void {
    if (this.scope === scope) return;
    this.scope = scope;
    this.epoch++;
    this.values.clear();
    this.pending.clear();
  }
  async resolve(scope: string, items: { key: string; reference: FileReference }[], resolver: Resolver): Promise<Map<string, ResolveFileReferenceResult>> {
    // A render from an old identity must never reactivate its scope.
    if (scope !== this.scope) return new Map();
    const epoch = this.epoch;
    const promises = new Map<string, Promise<ResolveFileReferenceResult>>();
    const missing: typeof items = [];
    const finish = new Map<string, (value: ResolveFileReferenceResult) => void>();
    for (const item of items) {
      if (promises.has(item.key)) continue;
      const cached = this.values.get(item.key);
      if (cached && cached.expires > this.now()) {
        this.values.delete(item.key);
        this.values.set(item.key, cached);
        promises.set(item.key, Promise.resolve(cached.result));
      } else if (this.pending.has(item.key)) {
        promises.set(item.key, this.pending.get(item.key)!);
      } else {
        this.values.delete(item.key);
        const promise = new Promise<ResolveFileReferenceResult>((resolve) => finish.set(item.key, resolve));
        promises.set(item.key, promise);
        this.pending.set(item.key, promise);
        missing.push(item);
      }
    }
    const complete = (key: string, result: ResolveFileReferenceResult) => {
      if (epoch === this.epoch) {
        this.pending.delete(key);
        if (result.status === "resolved") {
          this.values.set(key, { result, expires: this.now() + 30_000 });
          while (this.values.size > 512) this.values.delete(this.values.keys().next().value!);
        }
      }
      finish.get(key)?.(result);
    };
    for (let start = 0; start < missing.length; start += 64) {
      const batch = missing.slice(start, start + 64);
      void resolver(batch.map((item) => item.reference)).then(({ results }) => {
        batch.forEach((item, index) => complete(item.key, results[index] ?? failure("Incomplete resolver response")));
      }).catch((error: unknown) => batch.forEach((item) => complete(item.key, failure(error instanceof Error ? error.message : "File resolution failed"))));
    }
    const entries = await Promise.all([...promises].map(async ([key, promise]) => [key, await promise] as const));
    return epoch === this.epoch ? new Map(entries) : new Map();
  }
}
