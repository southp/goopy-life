// Browser-side calls to the gl-serv API.
//
// Uses NEXT_PUBLIC_GL_API_URL, which must carry the NEXT_PUBLIC_ prefix so Next.js
// inlines it into the client bundle. The build-time config fetch lives in a separate,
// server-only module (./config) so its server-only env var never reaches the browser.
//
// Presence of NEXT_PUBLIC_GL_API_URL is enforced at build time in next.config.ts, so
// by the time this runs in the browser the value is always the baked-in literal.

import type {
	ApiError,
	CapacityResponse,
	GoopyResponse,
	SpawnResponse,
	VersionResponse,
} from "./types";

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

export async function spawnGoopy(): Promise<SpawnResponse> {
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

/**
 * Destroy the goopy this browser spawned, before it expires (#203).
 *
 * `token` is the `despawn_token` from `POST /goopies`; the slug alone proves
 * nothing, since it is the goopy's public subdomain.
 *
 * gl-serv's own 404 counts as success: the goopy is already gone, which is what
 * was asked for. Any other 404 — a proxy, a wrong API URL — throws, because the
 * caller forgets the token on success, and the token is the only way to destroy
 * a goopy that may still be running.
 */
export async function destroyGoopy(slug: string, token: string): Promise<void> {
	const res = await fetch(`${apiBase()}/goopies/${slug}`, {
		method: "DELETE",
		headers: { Authorization: `Bearer ${token}` },
	});
	if (res.ok) {
		return;
	}
	const err = await toApiError(res);
	if (isGone(res, err)) {
		return;
	}
	throw err;
}

/**
 * Whether a failed destroy is a refusal that retrying cannot change: the token
 * is not this goopy's (`forbidden`), or the goopy is not running (`conflict`).
 * Everything else — a rate limit, a 5xx, a network blip — may pass.
 */
export function isLastWord(err: unknown): boolean {
	return (
		err instanceof ApiRequestError &&
		(err.code === "forbidden" || err.code === "conflict")
	);
}

/** Whether a failed response is gl-serv saying the goopy does not exist. */
function isGone(res: Response, err: ApiRequestError): boolean {
	return res.status === 404 && err.code === "not_found";
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

// The commit the backend is running right now. Runtime, from the browser, for the
// same reason as getCapacity: a build-time fetch would freeze it at the last
// frontend build.
export async function getVersion(signal?: AbortSignal): Promise<VersionResponse> {
	const res = await fetch(`${apiBase()}/version`, { signal });
	if (!res.ok) {
		throw await toApiError(res);
	}
	return res.json();
}
