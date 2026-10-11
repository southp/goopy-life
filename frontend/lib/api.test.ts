import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import { ApiRequestError, destroyGoopy } from "./api.ts";

const realFetch = globalThis.fetch;
afterEach(() => {
	globalThis.fetch = realFetch;
});

/** Answer every fetch with `status` and `body`, recording what was asked. */
function stubFetch(status: number, body: unknown = {}) {
	const calls: { url: string; init?: RequestInit }[] = [];
	globalThis.fetch = (async (url: string, init?: RequestInit) => {
		calls.push({ url, init });
		return new Response(JSON.stringify(body), { status });
	}) as typeof fetch;
	return calls;
}

test("destroy sends a DELETE carrying the token as a bearer credential", async () => {
	const calls = stubFetch(202, { slug: "happy-tiny-goose", status: "Despawning" });

	await destroyGoopy("happy-tiny-goose", "abc123");

	assert.equal(calls.length, 1);
	assert.match(calls[0].url, /\/goopies\/happy-tiny-goose$/);
	assert.equal(calls[0].init?.method, "DELETE");
	assert.deepEqual(calls[0].init?.headers, { Authorization: "Bearer abc123" });
});

// Already gone is what the visitor asked for — a second click, or the sweeper
// got there first.
test("destroying a goopy that is already gone succeeds", async () => {
	stubFetch(404, { error: "not found", code: "not_found" });

	await destroyGoopy("happy-tiny-goose", "abc123");
});

// Forgetting the token on a 404 nobody can vouch for would strand a goopy that
// may still be running, holding a slot for hours.
test("a 404 that is not the API's own is an error, not \"gone\"", async () => {
	globalThis.fetch = (async () =>
		new Response("<html>Not Found</html>", { status: 404 })) as typeof fetch;

	await assert.rejects(destroyGoopy("happy-tiny-goose", "abc123"), (err: unknown) => {
		assert.ok(err instanceof ApiRequestError);
		assert.equal(err.code, null);
		return true;
	});
});

test("a refusal surfaces with the API's code", async () => {
	stubFetch(403, { error: "forbidden", code: "forbidden" });

	await assert.rejects(destroyGoopy("happy-tiny-goose", "wrong"), (err: unknown) => {
		assert.ok(err instanceof ApiRequestError);
		assert.equal(err.code, "forbidden");
		return true;
	});
});
