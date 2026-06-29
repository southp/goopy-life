// Browser-side calls to the gl-serv API.
//
// Uses NEXT_PUBLIC_GL_API_URL, which must carry the NEXT_PUBLIC_ prefix so Next.js
// inlines it into the client bundle. The build-time config fetch lives in a separate,
// server-only module (./config) so its server-only env var never reaches the browser.
//
// Presence of NEXT_PUBLIC_GL_API_URL is enforced at build time in next.config.ts, so
// by the time this runs in the browser the value is always the baked-in literal.

import type { ApiError, GoopyResponse } from "./types";

export function apiBase(): string {
	return process.env.NEXT_PUBLIC_GL_API_URL ?? "";
}

export async function extractError(res: Response): Promise<string> {
	try {
		const json: ApiError = await res.json();
		return json.error ?? `HTTP ${res.status}`;
	} catch {
		return `HTTP ${res.status}`;
	}
}

export async function spawnGoopy(): Promise<{ slug: string }> {
	const res = await fetch(`${apiBase()}/goopies`, { method: "POST" });
	if (!res.ok) {
		throw new Error(await extractError(res));
	}
	return res.json();
}

export async function getGoopy(
	slug: string,
	signal?: AbortSignal,
): Promise<GoopyResponse> {
	const res = await fetch(`${apiBase()}/goopies/${slug}`, { signal });
	if (res.status === 404) {
		throw new Error("not_found");
	}
	if (!res.ok) {
		throw new Error(await extractError(res));
	}
	return res.json();
}
