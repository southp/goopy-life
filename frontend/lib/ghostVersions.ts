// Which Ghost to spawn, out of what gl-serv offers at `GET /ghost-versions`
// (#210).
//
// Deliberately import-free (bar types), so `node --test` can load it without
// Next's module resolution.

import type { GhostVersionsResponse } from "./types";

// Where this browser remembers the version it last spawned, so the next visit
// starts on it. A per-viewer convenience: losing it costs one click.
export const GHOST_VERSION_KEY = "goopy_ghost_version";

/**
 * What is known of the list on offer: the list itself, `"loading"` while the
 * first read is still in flight, or `null` once that read has failed —
 * `/ghost-versions` unreachable, or a backend from before #210 without it.
 */
export type OfferedVersions = GhostVersionsResponse | "loading" | null;

/**
 * Whether there is a choice to show. A host offering one version (every host
 * until #210's configs land, and any local Hello run) shows no selector at all,
 * and the page renders exactly as it did before there was one.
 */
export function offersChoice(offered: OfferedVersions): offered is GhostVersionsResponse {
	return offered !== null && offered !== "loading" && offered.versions.length >= 2;
}

/**
 * The version to preselect: the one this browser remembers, while the server
 * still offers it, else the server's default. A remembered version the server
 * has since dropped is not an error the visitor should meet; it just stops
 * being remembered.
 */
export function pickVersion(offered: GhostVersionsResponse, remembered: string | null): string {
	if (remembered !== null && offered.versions.some((v) => v.version === remembered)) {
		return remembered;
	}
	return offered.default;
}

/** What the version segment of the CTA shows, when it shows at all. */
export interface VersionSegment {
	// The versions to list, in order.
	options: string[];
	// The one shown, and the one a click asks for.
	value: string;
	// True while the first read is in flight and the segment stands in for the
	// list with the remembered version alone.
	pending: boolean;
}

/**
 * The version segment of the split CTA, or null for a plain button.
 *
 * With a choice on offer, every version, preselecting per `pickVersion`. While
 * the first read is in flight, a browser that chose before gets its remembered
 * version straight away, so that a click before the list lands still asks for
 * what the segment shows rather than silently for the default. Otherwise a
 * plain button: one version on offer, no history to go on, or a list that
 * could not be read — which fails open to the server's default rather than
 * holding a picker that can never open.
 *
 * What a click asks `POST /goopies` for is this segment's `value`, or nothing
 * when there is no segment — in which case the request stays exactly what it
 * was before #210: no body, no Content-Type, and so no CORS preflight.
 */
export function versionSegment(
	offered: OfferedVersions,
	remembered: string | null,
): VersionSegment | null {
	if (offersChoice(offered)) {
		return {
			options: offered.versions.map((v) => v.version),
			value: pickVersion(offered, remembered),
			pending: false,
		};
	}
	if (offered === "loading" && remembered !== null) {
		return { options: [remembered], value: remembered, pending: true };
	}
	return null;
}

export function versionLabel(version: string): string {
	return `Ghost ${version}`;
}
