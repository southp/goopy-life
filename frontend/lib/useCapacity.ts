"use client";

import { useEffect, useState } from "react";
import { getCapacity } from "@/lib/api";
import { CAPACITY_POLL_INTERVAL_MS } from "@/lib/constants";
import type { CapacityResponse } from "@/lib/types";

/**
 * Poll `GET /capacity` while `enabled`, so the CTA can show live headroom and
 * refuse a click it already knows will fail.
 *
 * Advisory only. The reading can go stale between a poll and a click, and two
 * visitors can race for the last slot — the 503 from `POST /goopies` stays the
 * source of truth. Two consequences follow:
 *
 * - Returns `null` until the first successful read. Callers must treat `null`
 *   as "unknown", not as "full": the button stays clickable and the 503 does
 *   the refusing, so an unreachable `/capacity` cannot lock the whole page.
 * - A failed poll keeps the previous reading rather than resetting to `null`,
 *   so a transient blip does not flicker the indicator away.
 *
 * Toggling `enabled` back on re-reads immediately, which is what makes the
 * reading fresh again after a spawn was refused and the user returned to idle.
 */
export function useCapacity(enabled: boolean): CapacityResponse | null {
	const [capacity, setCapacity] = useState<CapacityResponse | null>(null);

	useEffect(() => {
		if (!enabled) {
			return;
		}

		const controller = new AbortController();
		let timeoutId: ReturnType<typeof setTimeout> | null = null;

		const tick = async () => {
			try {
				setCapacity(await getCapacity(controller.signal));
			} catch {
				// Swallowed on purpose — see the "advisory only" note above.
			}
			if (controller.signal.aborted) {
				return;
			}
			timeoutId = setTimeout(tick, CAPACITY_POLL_INTERVAL_MS);
		};

		// Read immediately: the indicator is blank until the first response, so
		// waiting a full interval would show nothing next to a live button.
		tick();

		return () => {
			controller.abort();
			if (timeoutId !== null) {
				clearTimeout(timeoutId);
			}
		};
	}, [enabled]);

	return capacity;
}
