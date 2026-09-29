import assert from "node:assert/strict";
import { test } from "node:test";

import { formatExpiry } from "./expiry.ts";

// Every date is built from local-time components, and the formatter renders in
// local time too, so these hold on a machine in any time zone. The locale is
// pinned so the 24-hour clock and the weekday spelling are fixed. Mid-June keeps
// both days clear of any daylight-saving switch.
const LOCALE = "en-GB";

test("an expiry later the same day shows only the time", () => {
	const now = new Date(2026, 5, 16, 9, 42); // Tue 16 June, 09:42
	const expiresAt = new Date(2026, 5, 16, 17, 42);

	assert.equal(formatExpiry(expiresAt, now, LOCALE), "17:42");
});

test("an expiry on the next day adds the weekday", () => {
	const now = new Date(2026, 5, 16, 18, 15); // Tue 16 June, 18:15
	const expiresAt = new Date(2026, 5, 17, 2, 15); // Wed 17 June, 02:15

	assert.equal(formatExpiry(expiresAt, now, LOCALE), "Wed 02:15");
});

test("an ISO timestamp from the API is accepted", () => {
	const expiresAt = new Date(2026, 5, 16, 17, 42);
	const now = new Date(2026, 5, 16, 9, 42);

	assert.equal(formatExpiry(expiresAt.toISOString(), now, LOCALE), "17:42");
});

test("an unparseable timestamp yields nothing to show", () => {
	assert.equal(formatExpiry("not a date", new Date(2026, 5, 16), LOCALE), null);
});
