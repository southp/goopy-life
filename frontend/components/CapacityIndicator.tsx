import { NO_FREE_SLOT_MESSAGE } from "@/lib/constants";
import type { CapacityResponse } from "@/lib/types";

interface CapacityIndicatorProps {
	capacity: CapacityResponse | null;
}

/**
 * The `x / y` slot readout under the CTA, plus one line when nothing is free.
 *
 * Shows only `used` / `total` — the pair the server says is binding. The
 * distinction between a running instance and one that failed and still holds a
 * slot is ours, not the visitor's; they need one fact, which is whether they
 * can get a slot.
 *
 * Renders nothing while capacity is unknown (first poll not back, or
 * `/capacity` unreachable) rather than a fabricated `-- / --`: the button stays
 * clickable in that state, so an indicator would only mislead.
 *
 * The extra `.capacity-slot` wrapper is what lets the readout arrive late
 * without disturbing anything — it hangs the block off the button's bottom edge
 * and clips it there, so the line slides out from behind the button instead of
 * popping into the layout. See the CSS for the mechanics.
 */
export default function CapacityIndicator({
	capacity,
}: CapacityIndicatorProps) {
	if (!capacity) {
		return null;
	}

	return (
		<div className="capacity-slot">
			<div className="capacity-block">
				<p
					className={`capacity-indicator${capacity.is_full ? " full" : ""}`}
					// Chatty enough to be noise if announced on every poll, so it is
					// exposed as a labelled status a screen reader can query instead.
					aria-label={`${capacity.used} of ${capacity.total} slots in use`}
				>
					<span className="capacity-count">
						{capacity.used} / {capacity.total}
					</span>{" "}
					slots in use
				</p>
				{capacity.is_full && (
					<p className="capacity-full-reason">{NO_FREE_SLOT_MESSAGE}</p>
				)}
			</div>
		</div>
	);
}
