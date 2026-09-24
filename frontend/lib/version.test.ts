import assert from "node:assert/strict";
import { test } from "node:test";

import { describeSha, UNKNOWN_SHA } from "./version.ts";

const REPO = "https://github.com/example/repo";
const SHA = "c50c932aa1b2c3d4e5f60718293a4b5c6d7e8f90";

test("a full commit id is shortened and linked to its commit", () => {
	assert.deepEqual(describeSha(SHA, REPO), {
		label: "c50c932",
		href: `${REPO}/commit/${SHA}`,
	});
});

test("a dirty build keeps its suffix and is not linked", () => {
	assert.deepEqual(describeSha(`${SHA}-dirty`, REPO), {
		label: "c50c932-dirty",
		href: null,
	});
});

test("unknown is shown as-is and not linked", () => {
	assert.deepEqual(describeSha(UNKNOWN_SHA, REPO), {
		label: UNKNOWN_SHA,
		href: null,
	});
});

test("a missing sha reads as unknown", () => {
	for (const missing of [undefined, null, ""]) {
		assert.deepEqual(describeSha(missing, REPO), {
			label: UNKNOWN_SHA,
			href: null,
		});
	}
});

test("anything that is not a full commit id is shown whole and not linked", () => {
	for (const odd of ["c50c932", "C50C932AA1B2C3D4E5F60718293A4B5C6D7E8F90", "not-a-sha"]) {
		assert.deepEqual(describeSha(odd, REPO), { label: odd, href: null });
	}
});
