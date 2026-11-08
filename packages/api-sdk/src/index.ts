export type FetchLike = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface ApiClientOptions {
  baseUrl: string | URL;
  fetch?: FetchLike;
  bearerToken?: string | (() => string | undefined | Promise<string | undefined>);
  headers?: HeadersInit | (() => HeadersInit | Promise<HeadersInit>);
}

export interface ApiRequestOptions {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
  headers?: HeadersInit;
}

export interface ApiBinaryResponse {
  blob: Blob;
  headers: Headers;
  status: number;
}

export class ApiHttpError extends Error {
  readonly status: number;
  readonly body: unknown;
  readonly headers: Headers;

  constructor(message: string, status: number, body: unknown, headers = new Headers()) {
    super(message);
    this.name = "ApiHttpError";
    this.status = status;
    this.body = body;
    this.headers = headers;
  }
}

export class ApiClient {
  readonly baseUrl: URL;
  private readonly fetchImpl: FetchLike;
  private readonly bearerToken?: ApiClientOptions["bearerToken"];
  private readonly headers?: ApiClientOptions["headers"];

  constructor(options: ApiClientOptions) {
    this.baseUrl = new URL(options.baseUrl);
    this.fetchImpl = options.fetch ?? fetch;
    this.bearerToken = options.bearerToken;
    this.headers = options.headers;
  }

  async request<T>(
    path: string,
    options: ApiRequestOptions = {},
  ): Promise<T> {
    const response = await this.requestResponse(path, options);
    return await readBody(response) as T;
  }

  async requestBinary(
    path: string,
    options: ApiRequestOptions = {},
  ): Promise<ApiBinaryResponse> {
    const response = await this.requestResponse(path, options);
    return { blob: await response.blob(), headers: response.headers, status: response.status };
  }

  private async requestResponse(path: string, options: ApiRequestOptions): Promise<Response> {
    const url = new URL(path, this.baseUrl);
    const headers = new Headers(
      typeof this.headers === "function" ? await this.headers() : this.headers,
    );
    new Headers(options.headers).forEach((value, key) => headers.set(key, value));

    const token =
      typeof this.bearerToken === "function"
        ? await this.bearerToken()
        : this.bearerToken;
    if (token) headers.set("authorization", `Bearer ${token}`);
    if (options.body !== undefined && !headers.has("content-type")) {
      headers.set("content-type", "application/json");
    }

    const requestInit: RequestInit = {
      method: options.method ?? "GET",
      headers,
    };
    if (options.signal !== undefined) requestInit.signal = options.signal;
    if (options.body !== undefined) requestInit.body = JSON.stringify(options.body);

    const response = await this.fetchImpl(url, requestInit);
    if (!response.ok) {
      const body = await readBody(response);
      const message =
        typeof body === "object" &&
        body !== null &&
        "error" in body &&
        typeof body.error === "object" &&
        body.error !== null &&
        "message" in body.error &&
        typeof body.error.message === "string"
          ? body.error.message
          : `Holon API request failed (${response.status})`;
      throw new ApiHttpError(message, response.status, body, response.headers);
    }
    return response;
  }
}

async function readBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}
