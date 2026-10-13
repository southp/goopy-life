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
	| "sad";

export function moodFor(state: AppState): Mood {
	switch (state.kind) {
		case "spawning":
			return "wobbling";
		case "done":
			return "proud";
		case "failed":
			return "hurt";
		case "error":
			return isExpectedError(state.code) ? "sad" : "hurt";
		default:
			return "calm";
	}
}
