import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { test } from "node:test";

import { formatExpiry } from "./expiry.ts";

// Every date is built from local-time components, and the formatter renders in
// local time too, so these hold on a machine in any time zone. Mid-June keeps
// both days clear of any daylight-saving switch.

test("an expiry later the same day shows only the time", () => {
	const now = new Date(2026, 5, 16, 9, 42); // Tue 16 June, 09:42
	const expiresAt = new Date(2026, 5, 16, 17, 42);

	assert.equal(formatExpiry(expiresAt, now), "17:42");
});

test("an expiry on the next day adds the weekday", () => {
	const now = new Date(2026, 5, 16, 18, 15); // Tue 16 June, 18:15
	const expiresAt = new Date(2026, 5, 17, 2, 15); // Wed 17 June, 02:15

	assert.equal(formatExpiry(expiresAt, now), "Wed 02:15");
});

test("an ISO timestamp from the API is accepted", () => {
	const expiresAt = new Date(2026, 5, 16, 17, 42);
	const now = new Date(2026, 5, 16, 9, 42);

	assert.equal(formatExpiry(expiresAt.toISOString(), now), "17:42");
});

test("an unparseable timestamp yields nothing to show", () => {
	assert.equal(formatExpiry("not a date", new Date(2026, 5, 16)), null);
});

test("the time stays English when the default locale is not", () => {
	// A process's default locale is fixed at startup, so run the formatter in a
	// child whose environment says Traditional Chinese. The child also reports
	// its default locale, so the test cannot pass by never having switched.
	const script = `
		import { formatExpiry } from ${JSON.stringify(new URL("./expiry.ts", import.meta.url).href)};
		const now = new Date(2026, 5, 16, 9, 42);
		console.log(JSON.stringify({
			locale: Intl.DateTimeFormat().resolvedOptions().locale,
			sameDay: formatExpiry(new Date(2026, 5, 16, 17, 42), now),
			nextDay: formatExpiry(new Date(2026, 5, 17, 2, 15), now),
		}));
	`;
	const out = execFileSync(process.execPath, ["--input-type=module", "-e", script], {
		env: { ...process.env, LC_ALL: "zh_TW.UTF-8", LANG: "zh_TW.UTF-8" },
		encoding: "utf8",
	});

	assert.deepEqual(JSON.parse(out), {
		locale: "zh-TW",
		sameDay: "17:42",
		nextDay: "Wed 02:15",
	});
});
