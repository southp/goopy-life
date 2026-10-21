import VersionSelect from "@/components/VersionSelect";
import { NO_FREE_SLOT_MESSAGE } from "@/lib/constants";
import type { VersionSegment } from "@/lib/ghostVersions";
import type { CapacityResponse } from "@/lib/types";

interface CtaReadoutProps {
	capacity: CapacityResponse | null;
	// The Ghost version picker, or null when there is no choice to offer.
	segment: VersionSegment | null;
	onVersionChange: (version: string) => void;
}

/**
 * The line under the CTA: the Ghost version picker (#210), the `x / y` slot
 * readout, and one more line when nothing is free.
 *
 * Both halves are things only the server knows, so both arrive after the
 * first paint, and they share one berth for it. The picker sits here rather
 * than a row of its own so the hero stays a button and a line of fine print —
 * and the line, not the button, is what moves when they arrive.
 *
 * The readout shows only `used` / `total` — the pair the server says is
 * binding. The distinction between a running instance and one that failed and
 * still holds a slot is ours, not the visitor's; they need one fact, which is
 * whether they can get a slot.
 *
 * Renders nothing while both are unknown (first answers not back, or the
 * endpoints unreachable) rather than a fabricated `-- / --`: the button stays
 * clickable in that state, so a placeholder would only mislead.
 *
 * The extra `.capacity-slot` wrapper is what lets the line arrive late
 * without disturbing anything — it hangs the block off the button's bottom edge
 * and clips it there, so the line slides out from behind the button instead of
 * popping into the layout. See the CSS for the mechanics.
 */
export default function CtaReadout({ capacity, segment, onVersionChange }: CtaReadoutProps) {
	if (!capacity && !segment) {
		return null;
	}
	const isFull = capacity?.is_full ?? false;

	return (
		<div className="capacity-slot">
			<div className="capacity-block">
				<div className="cta-readout-line">
					{segment && (
						<VersionSelect segment={segment} disabled={isFull} onChange={onVersionChange} />
					)}
					{capacity && (
						<p
							className={`capacity-indicator${isFull ? " full" : ""}`}
							// Chatty enough to be noise if announced on every poll, so it is
							// exposed as a labelled status a screen reader can query instead.
							aria-label={`${capacity.used} of ${capacity.total} slots in use`}
						>
							<span className="capacity-count">
								{capacity.used} / {capacity.total}
							</span>{" "}
							slots in use
						</p>
					)}
				</div>
				{isFull && <p className="capacity-full-reason">{NO_FREE_SLOT_MESSAGE}</p>}
			</div>
		</div>
	);
}
