// Which Ghost to spawn, out of what gl-serv offers at `GET /ghost-versions`
// (#210).
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution.

// What `GET /ghost-versions` answers. Kept here rather than in ./types so that
// this module stays import-free; ./types re-exports it.
export interface GhostVersionsResponse {
	// What a spawn that names no version gets. One of `versions`.
	default: string;
	// In the order to offer them.
	versions: { version: string }[];
}

// Where this browser remembers the version it last spawned, so the next visit
// starts on it. A per-viewer convenience: losing it costs one click.
export const GHOST_VERSION_KEY = "goopy_ghost_version";

/**
 * Whether there is a choice to show. A host offering one version (every host
 * until #210's configs land, and any local Hello run) shows no selector at all,
 * and the page renders exactly as it did before there was one.
 */
export function offersChoice(offered: GhostVersionsResponse | null): offered is GhostVersionsResponse {
	return offered !== null && offered.versions.length >= 2;
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
	// True while the list is still unknown and the segment stands in for it
	// with the remembered version alone.
	pending: boolean;
}

/**
 * The version segment of the split CTA, or null for a plain button.
 *
 * With a choice on offer, every version, preselecting per `pickVersion`. While
 * the list is still unknown, a browser that chose before gets its remembered
 * version straight away, so that a click before the list lands still asks for
 * what the segment shows rather than silently for the default. Without either,
 * a plain button: one version on offer, or no history to go on.
 *
 * What a click asks `POST /goopies` for is this segment's `value`, or nothing
 * when there is no segment — in which case the request stays exactly what it
 * was before #210: no body, no Content-Type, and so no CORS preflight.
 */
export function versionSegment(
	offered: GhostVersionsResponse | null,
	remembered: string | null,
): VersionSegment | null {
	if (offersChoice(offered)) {
		return {
			options: offered.versions.map((v) => v.version),
			value: pickVersion(offered, remembered),
			pending: false,
		};
	}
	if (offered === null && remembered !== null) {
		return { options: [remembered], value: remembered, pending: true };
	}
	return null;
}

export function versionLabel(version: string): string {
	return `Ghost ${version}`;
}
