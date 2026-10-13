import assert from "node:assert/strict";
import { test } from "node:test";

import { isExpectedError } from "./errors.ts";

test("capacity and rate-limit refusals are expected", () => {
	for (const code of ["server_full", "server_busy", "too_many_requests"]) {
		assert.equal(isExpectedError(code), true, code);
	}
});

test("any other code is a fault", () => {
	assert.equal(isExpectedError("internal"), false);
});

// No code means the body was not the API's error shape, e.g. a proxy's page:
// nothing trustworthy to call expected.
test("a missing code is a fault", () => {
	assert.equal(isExpectedError(null), false);
});
