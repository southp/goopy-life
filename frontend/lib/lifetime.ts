// Turns an instance lifetime, configured in hours, into the words the landing page
// shows.
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution.

const HOURS_PER_DAY = 24;

function plural(count: number, unit: string): string {
	return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

/**
 * A whole number of days reads as days ("1 day", "3 days"); anything else stays
 * in hours ("1 hour", "8 hours", "36 hours").
 */
export function formatLifetime(hours: number): string {
	if (hours > 0 && hours % HOURS_PER_DAY === 0) {
		return plural(hours / HOURS_PER_DAY, "day");
	}
	return plural(hours, "hour");
}
