import assert from "node:assert/strict";
import { test } from "node:test";

import { resumeAt } from "./resume.ts";

test("a ready goopy resumes as ready", () => {
	assert.equal(resumeAt({ status: "Done", is_expired: false }), "done");
});

test("a ready goopy past its expiry resumes as expired", () => {
	assert.equal(resumeAt({ status: "Done", is_expired: true }), "expired");
});

test("a booting goopy resumes polling", () => {
	assert.equal(resumeAt({ status: "Spawning", is_expired: false }), "spawning");
});

// The reason this exists: reloading mid-"Destroying…" used to land on
// "Goops! Spawning failed." for a goopy that was simply on its way out.
test("a goopy being destroyed resumes as leaving, not failed", () => {
	assert.equal(resumeAt({ status: "Despawning", is_expired: false }), "leaving");
});

test("anything else resumes as failed", () => {
	for (const status of ["Failed", "Archived", "Empty"] as const) {
		assert.equal(resumeAt({ status, is_expired: false }), "failed", status);
	}
});
