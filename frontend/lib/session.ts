// What the browser remembers about the goopy it spawned, under LOCALSTORAGE_KEY.
//
// The slug lets a reload resume the post-spawn page. The token is what lets this
// browser — and only this one — destroy the instance early (#203): gl-serv hands
// it out once, at spawn, and keeps only its hash. Lose it and the instance can
// only expire.
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution.

export interface StoredGoopy {
	slug: string;
	// Null when the spawn predates tokens, or the backend issued none.
	token: string | null;
}

// The shape of a slug (adjective-adjective-noun), which is all a pre-#203 entry
// held. Matched by shape rather than "not JSON" so that a damaged entry is not
// mistaken for one.
const LEGACY_SLUG = /^[a-z]+(-[a-z]+)+$/;

export function serializeStoredGoopy(goopy: StoredGoopy): string {
	return JSON.stringify(goopy);
}

/**
 * Read back what `serializeStoredGoopy` wrote — or what a build before #203
 * wrote, which was the bare slug. That entry still resumes, just without a
 * token, so it never offers the destroy button. Anything unrecognisable reads as
 * nothing stored.
 */
export function parseStoredGoopy(raw: string | null): StoredGoopy | null {
	if (raw === null || raw === "") {
		return null;
	}
	if (LEGACY_SLUG.test(raw)) {
		return { slug: raw, token: null };
	}
	try {
		const parsed: unknown = JSON.parse(raw);
		if (typeof parsed !== "object" || parsed === null) {
			return null;
		}
		const { slug, token } = parsed as Record<string, unknown>;
		if (typeof slug !== "string" || slug === "") {
			return null;
		}
		return { slug, token: typeof token === "string" && token !== "" ? token : null };
	} catch {
		return null;
	}
}
