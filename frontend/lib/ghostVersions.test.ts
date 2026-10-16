import assert from "node:assert/strict";
import { test } from "node:test";

import {
	type GhostVersionsResponse,
	offersChoice,
	pickVersion,
	versionLabel,
	versionToRequest,
} from "./ghostVersions.ts";

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

// One version means the request stays what it was before there was a choice:
// no body, so no CORS preflight.
test("nothing is requested when there is no choice", () => {
	assert.equal(versionToRequest(ONE, "6.63.0"), null);
	assert.equal(versionToRequest(null, "5.130.6"), null);
});

test("with a choice, the preselected version is requested", () => {
	assert.equal(versionToRequest(THREE, "5.130.6"), "5.130.6");
	assert.equal(versionToRequest(THREE, null), "6.71.0");
});

test("a version is labelled by name, nothing more", () => {
	assert.equal(versionLabel("5.130.6"), "Ghost 5.130.6");
});
