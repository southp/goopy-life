import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import {
	ApiRequestError,
	destroyGoopy,
	getGhostVersions,
	isLastWord,
	spawnGoopy,
	waitUntilGone,
} from "./api.ts";

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

// Retrying a refusal like these can never succeed, so the page must stop
// offering it; anything else may pass and is worth another try.
test("only a refusal retrying can't change is the last word", () => {
	assert.equal(isLastWord(new ApiRequestError("forbidden", "forbidden")), true);
	assert.equal(isLastWord(new ApiRequestError("instance is not running", "conflict")), true);

	assert.equal(isLastWord(new ApiRequestError("slow down", "too_many_requests")), false);
	assert.equal(isLastWord(new ApiRequestError("HTTP 502", null)), false);
	assert.equal(isLastWord(new Error("Failed to fetch")), false);
});

/** Answer each fetch with the next of `responses`, recording every call. */
function stubFetchSequence(responses: (() => Response)[]) {
	const calls: string[] = [];
	globalThis.fetch = (async (url: string) => {
		calls.push(url);
		const next = responses[Math.min(calls.length, responses.length) - 1];
		return next();
	}) as typeof fetch;
	return calls;
}

const despawning = () =>
	new Response(JSON.stringify({ slug: "a-b-c", status: "Despawning" }), { status: 200 });
const goneFromApi = () =>
	new Response(JSON.stringify({ error: "not found", code: "not_found" }), { status: 404 });

// Until the teardown ends the goopy still holds a slot; going idle earlier
// lets "start over" run into a full server.
test("waiting ends once the API says the goopy is gone", async () => {
	const calls = stubFetchSequence([despawning, despawning, goneFromApi]);

	await waitUntilGone("a-b-c", { intervalMs: 0 });

	assert.equal(calls.length, 3);
	assert.match(calls[0], /\/goopies\/a-b-c$/);
});

test("waiting ends when the teardown failed", async () => {
	const calls = stubFetchSequence([
		() => new Response(JSON.stringify({ slug: "a-b-c", status: "Failed" }), { status: 200 }),
	]);

	await waitUntilGone("a-b-c", { intervalMs: 0 });

	assert.equal(calls.length, 1);
});

test("waiting rides out a transient error", async () => {
	const calls = stubFetchSequence([
		() => new Response("Bad Gateway", { status: 502 }),
		() => new Response("<html>Not Found</html>", { status: 404 }),
		goneFromApi,
	]);

	await waitUntilGone("a-b-c", { intervalMs: 0 });

	assert.equal(calls.length, 3, "neither a 502 nor a foreign 404 ends the wait");
});

test("waiting stops when aborted", async () => {
	const controller = new AbortController();
	const calls = stubFetchSequence([
		() => {
			controller.abort();
			return despawning();
		},
	]);

	await assert.rejects(waitUntilGone("a-b-c", { intervalMs: 50, signal: controller.signal }));
	assert.equal(calls.length, 1);
});

// With no choice to offer, the spawn is byte-for-byte what it was before #210:
// no body and no Content-Type, so the browser sends it without a preflight.
test("a spawn without a version sends no body", async () => {
	const calls = stubFetch(201, { slug: "happy-tiny-goose", status: "Spawning" });

	await spawnGoopy();

	assert.equal(calls.length, 1);
	assert.match(calls[0].url, /\/goopies$/);
	assert.equal(calls[0].init?.method, "POST");
	assert.equal(calls[0].init?.body, undefined);
	assert.equal(calls[0].init?.headers, undefined);
});

test("a spawn with a version asks for it as JSON", async () => {
	const calls = stubFetch(201, { slug: "happy-tiny-goose", status: "Spawning" });

	await spawnGoopy("5.130.6");

	assert.equal(calls[0].init?.method, "POST");
	assert.deepEqual(calls[0].init?.headers, { "Content-Type": "application/json" });
	assert.deepEqual(JSON.parse(String(calls[0].init?.body)), { ghost_version: "5.130.6" });
});

test("a refused version surfaces its code", async () => {
	stubFetch(400, { error: "Ghost 4.48.0 is not offered on this host", code: "unknown_version" });

	await assert.rejects(spawnGoopy("4.48.0"), (err: unknown) => {
		assert.ok(err instanceof ApiRequestError);
		assert.equal(err.code, "unknown_version");
		return true;
	});
});

test("the version list is read from /ghost-versions", async () => {
	const offered = { default: "6.71.0", versions: [{ version: "6.71.0" }, { version: "5.130.6" }] };
	const calls = stubFetch(200, offered);

	assert.deepEqual(await getGhostVersions(), offered);
	assert.match(calls[0].url, /\/ghost-versions$/);
});
