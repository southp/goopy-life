// Turns an instance's expiry into the time the page shows after "Available until".
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution. `now` is a parameter so tests control the clock.

// The wording and the time format are English whatever the browser's language, so
// the line never mixes languages ("Available until 週三上午2:15"). Only the time
// zone follows the viewer. en-GB gives a 24-hour "17:42" and a short "Wed".
const EXPIRY_LOCALE = "en-GB";

function sameCalendarDay(a: Date, b: Date): boolean {
	return (
		a.getFullYear() === b.getFullYear() &&
		a.getMonth() === b.getMonth() &&
		a.getDate() === b.getDate()
	);
}

/**
 * The expiry as a wall-clock time in the viewer's time zone: "17:42" when it
 * falls on the same calendar day as `now`, and with a short weekday
 * ("Tue 02:15") when it does not. No seconds, no time zone label.
 *
 * Returns null for an unparseable timestamp, so the caller shows nothing rather
 * than "Invalid Date".
 */
export function formatExpiry(expiresAt: string | Date, now: Date): string | null {
	const expiry = new Date(expiresAt);
	if (Number.isNaN(expiry.getTime())) {
		return null;
	}

	if (sameCalendarDay(expiry, now)) {
		return expiry.toLocaleTimeString(EXPIRY_LOCALE, {
			hour: "numeric",
			minute: "2-digit",
		});
	}
	return expiry.toLocaleString(EXPIRY_LOCALE, {
		weekday: "short",
		hour: "numeric",
		minute: "2-digit",
	});
}
