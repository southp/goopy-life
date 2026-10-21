"use client";

import { useEffect, useState } from "react";
import { getGhostVersions } from "@/lib/api";
import type { OfferedVersions } from "@/lib/ghostVersions";

/**
 * Read `GET /ghost-versions` each time `enabled` turns on (#210).
 *
 * Once per entry, not on an interval: the list only changes with a backend
 * deploy. Re-reading on every return to idle is what recovers from the one way
 * it goes stale under a visitor — a deploy that drops the version they picked,
 * which the spawn refuses with `unknown_version` and "Try again" brings them
 * back from.
 *
 * Returns `"loading"` until the first read settles, then the list, or `null`
 * if that read failed. `null` means no choice to offer, so the CTA is a plain
 * button and the spawn takes the server's default (`versionSegment`): an
 * unreachable `/ghost-versions` costs the choice, never the button. A failed
 * re-read keeps the previous list; an aborted read is no failure at all.
 */
export function useGhostVersions(enabled: boolean): OfferedVersions {
	const [offered, setOffered] = useState<OfferedVersions>("loading");

	useEffect(() => {
		if (!enabled) {
			return;
		}
		const controller = new AbortController();
		getGhostVersions(controller.signal)
			.then(setOffered)
			.catch(() => {
				// Left idle before the answer (a returning visitor's mount does
				// this): nothing was learnt, so nothing changes.
				if (controller.signal.aborted) {
					return;
				}
				setOffered((prev) => (prev === "loading" ? null : prev));
			});
		return () => controller.abort();
	}, [enabled]);

	return offered;
}
