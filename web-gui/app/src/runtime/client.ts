import { runtimeFixture } from "./fixtures";

export interface RuntimeClientOptions {
  baseUrl?: string;
}

export function createRuntimeClient(_options: RuntimeClientOptions = {}) {
  return {
    async getBootstrap() {
      return runtimeFixture;
    },
  };
}
