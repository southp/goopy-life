// Where a Ghost instance's admin dashboard lives, given the site URL the API
// returns. Ghost serves its admin under `/ghost/` on the site's own origin.
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution.

export function adminUrl(siteUrl: string): string {
	return `${siteUrl.replace(/\/+$/, "")}/ghost/`;
}
