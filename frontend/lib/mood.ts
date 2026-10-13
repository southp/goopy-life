// How the hero goopy looks, given what the page is doing (#208).
//
// Decoration only: the copy under the goopy says what is happening, and the
// goopy just acts it out. Kept separate from the component so the mapping can
// be tested without a DOM.
//
// Deliberately import-free (bar types), so `node --test` can load it without
// Next's module resolution.

import type { AppState } from "./types";

export type Mood =
	// Nothing in flight: today's look.
	| "calm"
	// A goopy is booting: it wobbles like a pudding.
	| "wobbling"
	// The goopy is ready: rainbow shine, sparkles, a stronger glow.
	| "proud";

export function moodFor(state: AppState): Mood {
	switch (state.kind) {
		case "spawning":
			return "wobbling";
		case "done":
			return "proud";
		default:
			return "calm";
	}
}
