import assert from "node:assert/strict";
import { test } from "node:test";

import { moodFor } from "./mood.ts";

test("a goopy at rest is calm", () => {
	assert.equal(moodFor({ kind: "idle" }), "calm");
});

// Resuming is a quick "Checking…", not a spawn: wobbling would promise one.
test("checking a remembered goopy stays calm", () => {
	assert.equal(moodFor({ kind: "resuming", slug: "a-b-c", token: null }), "calm");
});

// Expiry is the plan working as intended, not a fault.
test("an expired goopy stays calm", () => {
	assert.equal(moodFor({ kind: "expired", slug: "a-b-c" }), "calm");
});

test("a spawning goopy wobbles", () => {
	assert.equal(moodFor({ kind: "spawning" }), "wobbling");
});

const done = { kind: "done", slug: "a-b-c", url: "", expiresAt: "", token: "t" } as const;

test("a ready goopy is proud", () => {
	assert.equal(moodFor(done), "proud");
});

test("a goopy being destroyed brews", () => {
	assert.equal(moodFor(done, true), "brewing");
});

// Reloaded mid-"Destroying…": the same teardown, so the same look.
test("a destroy resumed after a reload brews too", () => {
	assert.equal(moodFor({ kind: "leaving", slug: "a-b-c" }), "brewing");
});

test("a failed spawn hurts", () => {
	assert.equal(moodFor({ kind: "failed", slug: "a-b-c" }), "hurt");
});

test("a fault hurts", () => {
	assert.equal(moodFor({ kind: "error", message: "boom", code: "internal" }), "hurt");
	assert.equal(moodFor({ kind: "error", message: "HTTP 502", code: null }), "hurt");
});

// A full server is not a fault, so no bandaid — the same line ErrorMessage
// draws when it drops the "file an issue" prompt.
test("an expected refusal is only sad", () => {
	assert.equal(moodFor({ kind: "error", message: "full", code: "server_full" }), "sad");
});

test("a goopy that was just destroyed is reborn", () => {
	assert.equal(moodFor({ kind: "idle" }, false, true), "reborn");
});

// The flag only means something on the way back to idle: anything else the
// visitor does next takes over at once.
test("a new spawn interrupts the rebirth", () => {
	assert.equal(moodFor({ kind: "spawning" }, false, true), "wobbling");
});
