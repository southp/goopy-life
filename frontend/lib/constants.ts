// Shared constants used across the frontend.

export const GITHUB_ISSUES_URL = "https://github.com/southp/goopy-life/issues";
export const LOCALSTORAGE_KEY = "goopy_slug";
export const POLL_INTERVAL_MS = 2000;

// How often the CTA re-reads GET /capacity while it is waiting to be clicked.
// Deliberately slower than POLL_INTERVAL_MS: nobody is waiting on this number,
// and every visitor sitting on the page pays for the interval.
export const CAPACITY_POLL_INTERVAL_MS = 10000;
