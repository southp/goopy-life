// How the hero goopy looks, given what the page is doing (#208).
//
// Decoration only: the copy under the goopy says what is happening, and the
// goopy just acts it out. Kept separate from the component so the mapping can
// be tested without a DOM.
//
// Imports only import-free modules, by their `.ts` path, so `node --test` can
// load it without Next's module resolution.

import { isExpectedError } from "./errors.ts";
import type { AppState } from "./types";

export type Mood =
	// Nothing in flight: today's look.
	| "calm"
	// A goopy is booting: it wobbles like a pudding.
	| "wobbling"
	// The goopy is ready: rainbow shine, sparkles, a stronger glow.
	| "proud"
	// Something broke: tears, and a bandaid on its head.
	| "hurt"
	// The service said no by design (full, rate limited): tears, but no
	// bandaid — nothing is broken.
	| "sad"
	// The goopy is being destroyed: it turns red and steams.
	| "brewing"
	// The destroy finished: the goopy explodes, leaving a tiny one of itself
	// that inflates back to normal. Plays once, then back to calm.
	| "reborn";

// Two flags live outside AppState, so they are passed alongside it:
// - `destroying`: the destroy button on the done page has a destroy in flight
// - `justDestroyed`: a destroy has just finished, and the explosion has not
//   played out yet
export function moodFor(state: AppState, destroying = false, justDestroyed = false): Mood {
	switch (state.kind) {
		case "idle":
			return justDestroyed ? "reborn" : "calm";
		case "spawning":
			return "wobbling";
		case "done":
			return destroying ? "brewing" : "proud";
		// A destroy picked up after a reload.
		case "leaving":
			return "brewing";
		case "failed":
			return "hurt";
		case "error":
			return isExpectedError(state.code) ? "sad" : "hurt";
		default:
			return "calm";
	}
}
