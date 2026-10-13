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
	// that inflates back to normal. Plays once, and its last frame looks calm,
	// so it can hold until the visitor moves on.
	| "reborn";

// `justDestroyed`: a destroy has just finished, and the visitor has not moved
// on yet. It lives outside AppState, so it is passed alongside.
export function moodFor(state: AppState, justDestroyed = false): Mood {
	switch (state.kind) {
		case "idle":
			return justDestroyed ? "reborn" : "calm";
		// A quick "Checking…", not a spawn: wobbling would promise one.
		case "resuming":
			return "calm";
		// The plan working as intended, not a fault.
		case "expired":
			return "calm";
		case "spawning":
			return "wobbling";
		case "done":
			return "proud";
		// A destroy underway, whether just accepted or picked up after a reload.
		case "leaving":
			return "brewing";
		case "failed":
			return "hurt";
		case "error":
			return isExpectedError(state.code) ? "sad" : "hurt";
		default: {
			// A new AppState kind stops compiling here until it is given a mood.
			const unhandled: never = state;
			return unhandled;
		}
	}
}
