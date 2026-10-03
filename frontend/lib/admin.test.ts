import assert from "node:assert/strict";
import { test } from "node:test";

import { adminUrl } from "./admin.ts";

test("a subdomain site URL gains the admin path", () => {
	assert.equal(
		adminUrl("https://happy-tiny-goose.goopy.life"),
		"https://happy-tiny-goose.goopy.life/ghost/",
	);
});

test("a localhost site URL keeps its port", () => {
	assert.equal(adminUrl("http://localhost:2369"), "http://localhost:2369/ghost/");
});

test("a trailing slash does not double up", () => {
	assert.equal(
		adminUrl("https://happy-tiny-goose.goopy.life/"),
		"https://happy-tiny-goose.goopy.life/ghost/",
	);
});
