import assert from "node:assert/strict";
import { test } from "node:test";

import { parseStoredGoopy, serializeStoredGoopy } from "./session.ts";

test("a stored slug and token round-trip", () => {
	const goopy = { slug: "happy-tiny-goose", token: "f".repeat(64) };

	assert.deepEqual(parseStoredGoopy(serializeStoredGoopy(goopy)), goopy);
});

test("a spawn with no token round-trips without one", () => {
	const goopy = { slug: "happy-tiny-goose", token: null };

	assert.deepEqual(parseStoredGoopy(serializeStoredGoopy(goopy)), goopy);
});

// A visitor mid-session when this ships has the old entry. They keep their
// page, but never get a destroy button they could not use.
test("a bare slug from before tokens resumes without one", () => {
	assert.deepEqual(parseStoredGoopy("happy-tiny-goose"), {
		slug: "happy-tiny-goose",
		token: null,
	});
});

test("nothing stored reads as nothing", () => {
	assert.equal(parseStoredGoopy(null), null);
	assert.equal(parseStoredGoopy(""), null);
});

test("a damaged entry reads as nothing rather than throwing", () => {
	for (const raw of ["{", "{}", '{"slug":""}', '{"slug":42}', "null", '{"token":"x"}']) {
		assert.equal(parseStoredGoopy(raw), null, raw);
	}
});

test("an empty or non-string token reads as none", () => {
	assert.deepEqual(parseStoredGoopy('{"slug":"a-b-c","token":""}'), {
		slug: "a-b-c",
		token: null,
	});
	assert.deepEqual(parseStoredGoopy('{"slug":"a-b-c","token":7}'), {
		slug: "a-b-c",
		token: null,
	});
});
