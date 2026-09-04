// Shared constants used across the frontend.

export const GITHUB_ISSUES_URL = "https://github.com/southp/goopy-life/issues";
export const LOCALSTORAGE_KEY = "goopy_slug";
export const POLL_INTERVAL_MS = 2000;

// How often the CTA re-reads GET /capacity while it is waiting to be clicked.
// Deliberately slower than POLL_INTERVAL_MS: nobody is waiting on this number,
// and every visitor sitting on the page pays for the interval.
export const CAPACITY_POLL_INTERVAL_MS = 10000;

// Shown whenever a spawn cannot be served for lack of a slot — both ahead of the
// click (the disabled CTA) and after one that lost a race (the 503). One string,
// so the two paths cannot drift into two different voices. Says nothing about
// caps, instances or statuses: a visitor only cares whether a slot is free.
export const NO_FREE_SLOT_MESSAGE = "All slots are taken right now. Try again later.";
