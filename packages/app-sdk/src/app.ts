import { ApiClient } from "@holon/api-sdk";
import { events } from "./events.js";
import type {
  AppClientOptions,
  AppContext,
  AppEvent,
  AppEventsOptions,
  AppResponse,
} from "./types.js";

export interface AppConversationFacade {
  readonly api: ApiClient;
}

export interface HolonAppClient {
  readonly version: string;
  readonly conversation: AppConversationFacade;
  context(): Promise<AppContext>;
  request<T = unknown>(
    requestType: string,
    payload?: unknown,
    requestId?: string,
  ): Promise<AppResponse<T>>;
  events(options?: AppEventsOptions): AsyncIterable<AppEvent>;
}

export function createHolonApp(
  options: AppClientOptions = {},
): HolonAppClient {
  const baseUrl = new URL(
    options.baseUrl ?? ".",
    globalThis.location?.href ?? "http://localhost/",
  );
  const version = options.sdkVersion ?? "1";
  const apiOptions = {
    baseUrl,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    ...(options.bearerToken === undefined
      ? {}
      : { bearerToken: options.bearerToken }),
  };
  const api = new ApiClient(apiOptions);
  return Object.freeze({
    version,
    conversation: { api },
    context: () => api.request<AppContext>("context"),
    request: <T>(requestType: string, payload?: unknown, requestId?: string) =>
      api.request<AppResponse<T>>("request", {
        method: "POST",
        body: {
          version,
          request_type: requestType,
          payload: payload ?? null,
          ...(requestId === undefined ? {} : { request_id: requestId }),
        },
      }),
    events: (eventOptions?: AppEventsOptions) => events(baseUrl, eventOptions),
  });
}
