// Turns a build's commit id into what the footer shows: a short label, and a link
// to that commit only when the id really names one.
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution; callers pass the repository URL in.

export const UNKNOWN_SHA = "unknown";

const SHORT_SHA_LEN = 7;
const FULL_SHA = /^[0-9a-f]{40}$/;

export interface ShaDisplay {
	label: string;
	// Null whenever the id does not name exactly what is running: `unknown`, a
	// `-dirty` build (the commit exists, but is not what was built), or anything
	// that is not a full commit id. A link there would claim a commit that the
	// running code is not.
	href: string | null;
}

/**
 * Mirrors gl-core's `build_info::abbreviate`: a full commit id is shortened to
 * seven hex digits, keeping any `-dirty` suffix; anything else is shown whole,
 * so a truncation never produces a string that merely reads like a commit.
 */
export function describeSha(
	sha: string | null | undefined,
	repoUrl: string,
): ShaDisplay {
	if (!sha) {
		return { label: UNKNOWN_SHA, href: null };
	}

	const dash = sha.indexOf("-");
	const hex = dash === -1 ? sha : sha.slice(0, dash);
	const suffix = dash === -1 ? "" : sha.slice(dash);

	if (!FULL_SHA.test(hex)) {
		return { label: sha, href: null };
	}

	return {
		label: hex.slice(0, SHORT_SHA_LEN) + suffix,
		href: suffix === "" ? `${repoUrl}/commit/${hex}` : null,
	};
}
