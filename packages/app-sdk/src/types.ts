export interface AppSession {
  readonly authenticated: boolean;
  readonly permissions: readonly string[];
}

export interface AppContext {
  readonly sdk_version: string;
  readonly agent_id: string;
  readonly app_id: string;
  readonly session: AppSession;
}

export interface AppResponse<T = unknown> {
  readonly ok: boolean;
  readonly version: string;
  readonly request_id: string;
  readonly agent_id: string;
  readonly app_id: string;
  readonly status: string;
  readonly message_id: string;
  readonly data?: T;
}

export interface AppEvent {
  readonly version: string;
  readonly agent_id: string;
  readonly app_id: string;
  readonly event: {
    readonly sequence: number;
    readonly type: string;
    readonly timestamp: string;
    readonly message_id: string;
    readonly request_id?: string;
  };
}

export interface AppEventsOptions {
  readonly signal?: AbortSignal;
  readonly lastEventId?: string;
}

export interface AppClientOptions {
  readonly baseUrl?: string | URL;
  readonly fetch?: (
    input: RequestInfo | URL,
    init?: RequestInit,
  ) => Promise<Response>;
  readonly bearerToken?: string | (() => string | undefined | Promise<string | undefined>);
  readonly sdkVersion?: string;
}
