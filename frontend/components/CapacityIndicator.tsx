import type { CapacityResponse } from "@/lib/types";

interface CapacityIndicatorProps {
	capacity: CapacityResponse | null;
}

/**
 * Why a spawn cannot be served right now, phrased for a visitor.
 *
 * Mirrors the two server-side caps, and checks the disk-bound one first: it is
 * the harder wall (a slot only frees when an instance expires and the sweep
 * reaps it), whereas a busy server clears as instances finish.
 */
function fullReason(capacity: CapacityResponse): string {
	if (capacity.provisioned >= capacity.max_provisioned) {
		return "The server is full — every slot is taken. One frees up when an instance expires.";
	}
	return "Too many instances are running right now. Try again in a few minutes.";
}

/**
 * The `x / y` headroom readout shown under the CTA, plus the reason when the
 * server is full.
 *
 * Renders nothing while capacity is unknown (the first poll has not landed, or
 * `/capacity` is unreachable) rather than showing a fabricated `-- / --`: the
 * button is still clickable in that state, so an indicator would only mislead.
 */
export default function CapacityIndicator({
	capacity,
}: CapacityIndicatorProps) {
	if (!capacity) {
		return null;
	}

	return (
		<div className="capacity-block">
			<p
				className={`capacity-indicator${capacity.is_full ? " full" : ""}`}
				// Chatty enough to be noise if announced on every poll, so it is
				// exposed as a labelled status a screen reader can query instead.
				aria-label={`${capacity.active} of ${capacity.max_active} instances live`}
			>
				<span className="capacity-count">
					{capacity.active} / {capacity.max_active}
				</span>{" "}
				instances live
			</p>
			{capacity.is_full && (
				<p className="capacity-full-reason">{fullReason(capacity)}</p>
			)}
		</div>
	);
}
