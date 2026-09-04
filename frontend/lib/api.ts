// Browser-side calls to the gl-serv API.
//
// Uses NEXT_PUBLIC_GL_API_URL, which must carry the NEXT_PUBLIC_ prefix so Next.js
// inlines it into the client bundle. The build-time config fetch lives in a separate,
// server-only module (./config) so its server-only env var never reaches the browser.
//
// Presence of NEXT_PUBLIC_GL_API_URL is enforced at build time in next.config.ts, so
// by the time this runs in the browser the value is always the baked-in literal.

import type { ApiError, CapacityResponse, GoopyResponse } from "./types";

export function apiBase(): string {
	return process.env.NEXT_PUBLIC_GL_API_URL ?? "";
}

/**
 * An error carrying the API's machine-readable `code` alongside its message.
 *
 * The code is what lets the UI tell an expected condition (`server_full`,
 * `server_busy`, `too_many_requests`) apart from a genuine fault, which decides
 * whether we invite the user to file a GitHub issue. `code` is null when the
 * response body was not the API's JSON error shape at all (e.g. a proxy error
 * page), i.e. when there is nothing trustworthy to branch on.
 */
export class ApiRequestError extends Error {
	readonly code: string | null;

	constructor(message: string, code: string | null) {
		super(message);
		this.name = "ApiRequestError";
		this.code = code;
	}
}

export async function toApiError(res: Response): Promise<ApiRequestError> {
	try {
		const json: ApiError = await res.json();
		return new ApiRequestError(json.error ?? `HTTP ${res.status}`, json.code ?? null);
	} catch {
		return new ApiRequestError(`HTTP ${res.status}`, null);
	}
}

export async function spawnGoopy(): Promise<{ slug: string }> {
	const res = await fetch(`${apiBase()}/goopies`, { method: "POST" });
	if (!res.ok) {
		throw await toApiError(res);
	}
	return res.json();
}

export async function getGoopy(
	slug: string,
	signal?: AbortSignal,
): Promise<GoopyResponse> {
	const res = await fetch(`${apiBase()}/goopies/${slug}`, { signal });
	if (res.status === 404) {
		throw new ApiRequestError("not_found", "not_found");
	}
	if (!res.ok) {
		throw await toApiError(res);
	}
	return res.json();
}

// Live capacity. Cheap read, polled from the browser — unlike GET /config, which is
// fetched once at build time and so cannot carry anything that changes at runtime.
export async function getCapacity(
	signal?: AbortSignal,
): Promise<CapacityResponse> {
	const res = await fetch(`${apiBase()}/capacity`, { signal });
	if (!res.ok) {
		throw await toApiError(res);
	}
	return res.json();
}
