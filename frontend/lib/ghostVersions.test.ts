import assert from "node:assert/strict";
import { test } from "node:test";

import { offersChoice, pickVersion, versionLabel, versionSegment } from "./ghostVersions.ts";
import type { GhostVersionsResponse } from "./types.ts";

const THREE: GhostVersionsResponse = {
	default: "6.71.0",
	versions: [{ version: "6.71.0" }, { version: "6.63.0" }, { version: "5.130.6" }],
};
const ONE: GhostVersionsResponse = { default: "6.63.0", versions: [{ version: "6.63.0" }] };

test("a choice is offered only when there are at least two versions", () => {
	assert.equal(offersChoice(THREE), true);
	assert.equal(offersChoice(ONE), false);
});

// Unknown (not fetched yet, or /ghost-versions unreachable) fails open: no
// selector, and the spawn takes the server's default.
test("an unknown list offers no choice", () => {
	assert.equal(offersChoice(null), false);
});

test("a list still loading offers no choice", () => {
	assert.equal(offersChoice("loading"), false);
});

test("the remembered version is preselected while it is still offered", () => {
	assert.equal(pickVersion(THREE, "5.130.6"), "5.130.6");
});

test("without a remembered version, the server's default is preselected", () => {
	assert.equal(pickVersion(THREE, null), "6.71.0");
});

// A version taken off the list since the last visit is not an error to show.
test("a remembered version the server dropped falls back to the default", () => {
	assert.equal(pickVersion(THREE, "6.67.0"), "6.71.0");
});

// One version means a plain button, and the request stays what it was before
// there was a choice: no body, so no CORS preflight.
test("one version on offer means no segment", () => {
	assert.equal(versionSegment(ONE, "6.63.0"), null);
});

test("an unknown list and no history means no segment", () => {
	assert.equal(versionSegment(null, null), null);
	assert.equal(versionSegment("loading", null), null);
});

test("with a choice, the segment lists every version and shows the pick", () => {
	assert.deepEqual(versionSegment(THREE, "5.130.6"), {
		options: ["6.71.0", "6.63.0", "5.130.6"],
		value: "5.130.6",
		pending: false,
	});
	assert.equal(versionSegment(THREE, null)?.value, "6.71.0");
});

// A returning visitor who clicks before the list lands asks for what the
// segment shows, not silently for the default.
test("before the list arrives, a remembered version holds the segment", () => {
	assert.deepEqual(versionSegment("loading", "5.130.6"), {
		options: ["5.130.6"],
		value: "5.130.6",
		pending: true,
	});
});

// A read that failed is not one still coming: holding the remembered version
// would leave a picker that never opens, and every spawn sending a body.
test("a failed read with a remembered version means no segment", () => {
	assert.equal(versionSegment(null, "5.130.6"), null);
});

test("a version is labelled by name, nothing more", () => {
	assert.equal(versionLabel("5.130.6"), "Ghost 5.130.6");
});
