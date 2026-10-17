"use client";

import { useEffect, useState } from "react";
import { getGhostVersions } from "@/lib/api";
import type { GhostVersionsResponse } from "@/lib/types";

/**
 * Read `GET /ghost-versions` each time `enabled` turns on (#210).
 *
 * Once per entry, not on an interval: the list only changes with a backend
 * deploy. Re-reading on every return to idle is what recovers from the one way
 * it goes stale under a visitor — a deploy that drops the version they picked,
 * which the spawn refuses with `unknown_version` and "Try again" brings them
 * back from.
 *
 * Returns `null` until the first successful read. Callers treat that as "the
 * list is unknown": no choice to offer, so the CTA is a plain button — or,
 * for a browser that chose before, one holding its remembered version
 * (`versionSegment`). An unreachable `/ghost-versions` therefore costs the
 * choice, never the button. A failed re-read keeps the previous list.
 */
export function useGhostVersions(enabled: boolean): GhostVersionsResponse | null {
	const [offered, setOffered] = useState<GhostVersionsResponse | null>(null);

	useEffect(() => {
		if (!enabled) {
			return;
		}
		const controller = new AbortController();
		getGhostVersions(controller.signal)
			.then(setOffered)
			.catch(() => {
				// Swallowed on purpose — see "costs the choice, never the button".
			});
		return () => controller.abort();
	}, [enabled]);

	return offered;
}
