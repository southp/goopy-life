// Where a reloaded page picks up, given what gl-serv says about the goopy this
// browser remembers.
//
// Deliberately import-free (bar types), so `node --test` can load it without
// Next's module resolution.

import type { GoopyResponse } from "./types";

export type ResumeAt =
	// Ready and in date: show it.
	| "done"
	// Ready but past its expiry: say so, and forget it.
	| "expired"
	// Still booting: keep polling.
	| "spawning"
	// Being destroyed (#203) — the visitor reloaded mid-"Destroying…". Wait for
	// it to be gone, as the destroy would have, then offer a new one.
	| "leaving"
	// Anything else: it will not come back.
	| "failed";

export function resumeAt(goopy: Pick<GoopyResponse, "status" | "is_expired">): ResumeAt {
	switch (goopy.status) {
		case "Done":
			return goopy.is_expired ? "expired" : "done";
		case "Spawning":
			return "spawning";
		case "Despawning":
			return "leaving";
		default:
			return "failed";
	}
}
