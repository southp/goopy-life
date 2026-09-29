import assert from "node:assert/strict";
import { test } from "node:test";

import { formatLifetime } from "./lifetime.ts";

test("a single hour is singular", () => {
	assert.equal(formatLifetime(1), "1 hour");
});

test("several hours are plural", () => {
	assert.equal(formatLifetime(8), "8 hours");
});

test("hours that are not whole days stay in hours", () => {
	assert.equal(formatLifetime(36), "36 hours");
});

test("whole days read as days", () => {
	assert.equal(formatLifetime(24), "1 day");
	assert.equal(formatLifetime(72), "3 days");
});
