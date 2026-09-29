// Turns an instance's expiry into the time the page shows after "Available until".
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution. `now` is a parameter so tests control the clock.

function sameCalendarDay(a: Date, b: Date): boolean {
	return (
		a.getFullYear() === b.getFullYear() &&
		a.getMonth() === b.getMonth() &&
		a.getDate() === b.getDate()
	);
}

/**
 * The expiry as a local wall-clock time, in the viewer's locale and time zone:
 * "17:42" when it falls on the same calendar day as `now`, and with a short
 * weekday ("Tue 02:15") when it does not. No seconds, no time zone label.
 *
 * `locale` defaults to the runtime's; tests pin it. Returns null for an
 * unparseable timestamp, so the caller shows nothing rather than "Invalid Date".
 */
export function formatExpiry(
	expiresAt: string | Date,
	now: Date,
	locale?: string,
): string | null {
	const expiry = new Date(expiresAt);
	if (Number.isNaN(expiry.getTime())) {
		return null;
	}

	if (sameCalendarDay(expiry, now)) {
		return expiry.toLocaleTimeString(locale, {
			hour: "numeric",
			minute: "2-digit",
		});
	}
	return expiry.toLocaleString(locale, {
		weekday: "short",
		hour: "numeric",
		minute: "2-digit",
	});
}
