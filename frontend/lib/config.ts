// Server-only: fetches runtime config from gl-serv at Vercel build time.
//
// Reads GL_CONFIG_API_URL (no NEXT_PUBLIC_ prefix — never exposed to the browser).
// The URL is a hard requirement: if it is unset we throw a clear, named error so the
// build fails loudly. A configured-but-unreachable API is treated differently — it
// degrades to null values so the page can render `--` placeholders.
//
// The `server-only` import makes the boundary enforceable: if this module is ever
// pulled into a client bundle, the build fails instead of throwing at runtime.

import "server-only";

import type { ConfigResponse } from "./types";

const CONFIG_UNAVAILABLE: ConfigResponse = {
	life_in_days: null,
	storage_quota_mb: null,
};

export async function fetchConfig(): Promise<ConfigResponse> {
	const apiUrl = process.env.GL_CONFIG_API_URL;
	if (!apiUrl) {
		throw new Error(
			"GL_CONFIG_API_URL is not set. Point it at your gl-serv base URL " +
				"(e.g. http://localhost:3001) in .env.local or Vercel project settings.",
		);
	}

	try {
		const res = await fetch(`${apiUrl}/config`);
		if (!res.ok) {
			console.warn(
				`[fetchConfig] GET ${apiUrl}/config returned ${res.status} — rendering placeholder values.`,
			);
			return CONFIG_UNAVAILABLE;
		}
		return (await res.json()) as ConfigResponse;
	} catch (err) {
		console.warn(
			`[fetchConfig] GET ${apiUrl}/config failed (${err instanceof Error ? err.message : "unknown error"}) — rendering placeholder values.`,
		);
		return CONFIG_UNAVAILABLE;
	}
}
