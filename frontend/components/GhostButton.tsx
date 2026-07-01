'use client';

import { useCallback, useEffect, useState } from 'react';
import { getGoopy, spawnGoopy } from '@/lib/api';
import { LOCALSTORAGE_KEY, POLL_INTERVAL_MS } from '@/lib/constants';
import type { AppState } from '@/lib/types';
import ErrorMessage from '@/components/ErrorMessage';
import IssueLink from '@/components/IssueLink';
import Spinner from '@/components/Spinner';

// The sole interactive element on the page: the "Ghost now!" CTA and its state
// machine (spawn → poll → done/failed, plus resume-from-localStorage and the
// expired-redirect handling). Everything else on the page is server-rendered.
export default function GhostButton() {
	const [state, setState] = useState<AppState>({ kind: "idle" });
	const [pollSlug, setPollSlug] = useState<string | null>(null);

	// Resolve the initial state from browser-only sources (the ?expired query param and
	// localStorage) after mount. This must run in an effect rather than a lazy useState
	// initializer: the page is statically prerendered, so reading window/localStorage at
	// render time would diverge from the server HTML and cause a hydration mismatch. The
	// set-state-in-effect rule is therefore suppressed for these post-mount reads.
	/* eslint-disable react-hooks/set-state-in-effect */
	useEffect(() => {
		const params = new URLSearchParams(window.location.search);
		if (params.has("expired")) {
			window.history.replaceState({}, "", "/");
			setState({ kind: "expired", slug: "" });
			return;
		}
		const saved = localStorage.getItem(LOCALSTORAGE_KEY);
		if (saved) {
			setState({ kind: "resuming", slug: saved });
		}
	}, []);
	/* eslint-enable react-hooks/set-state-in-effect */

	useEffect(() => {
		if (!pollSlug) {
			return;
		}

		const controller = new AbortController();
		let timeoutId: ReturnType<typeof setTimeout> | null = null;

		const tick = async () => {
			try {
				const data = await getGoopy(pollSlug, controller.signal);

				if (data.status === "Done") {
					setState({ kind: "done", slug: data.slug, url: data.url });
					setPollSlug(null);
					return;
				} else if (data.status !== "Spawning") {
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "failed", slug: pollSlug });
					setPollSlug(null);
					return;
				}
			} catch (err: unknown) {
				if (controller.signal.aborted) {
					return;
				}
				localStorage.removeItem(LOCALSTORAGE_KEY);
				const message = err instanceof Error ? err.message : "Unexpected error";
				setState({ kind: "error", message });
				setPollSlug(null);
				return;
			}
			timeoutId = setTimeout(tick, POLL_INTERVAL_MS);
		};

		// Check immediately — the slug already exists and the backend is spawning, so
		// waiting a full interval before the first GET is a guaranteed wasted delay.
		// tick() reschedules itself on each subsequent interval.
		tick();

		return () => {
			controller.abort();
			if (timeoutId !== null) {
				clearTimeout(timeoutId);
			}
		};
	}, [pollSlug]);

	const handleReset = useCallback(() => {
		setPollSlug(null);
		localStorage.removeItem(LOCALSTORAGE_KEY);
		setState({ kind: "idle" });
	}, []);

	// Depend only on the resuming slug (null when not resuming) so this effect fires
	// once per entry into "resuming" rather than re-running — and recreating its
	// AbortController — on every unrelated state transition.
	const resumingSlug = state.kind === "resuming" ? state.slug : null;

	useEffect(() => {
		if (resumingSlug === null) {
			return;
		}
		const slug = resumingSlug;
		const controller = new AbortController();

		getGoopy(slug, controller.signal).then((data) => {
			if (controller.signal.aborted) {
				return;
			}
			if (data.status === "Done") {
				if (data.is_expired) {
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "expired", slug });
				} else {
					setState({ kind: "done", slug: data.slug, url: data.url });
				}
			} else if (data.status !== "Spawning") {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "failed", slug });
			} else {
				setState({ kind: "spawning" });
				setPollSlug(slug);
			}
		}).catch((err: Error) => {
			if (controller.signal.aborted) {
				return;
			}
			if (err.message === "not_found") {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "idle" });
			} else {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "error", message: err.message });
			}
		});

		return () => controller.abort();
	}, [resumingSlug]);

	async function handleGhostNow() {
		setState({ kind: "spawning" });

		try {
			const { slug } = await spawnGoopy();
			localStorage.setItem(LOCALSTORAGE_KEY, slug);
			setPollSlug(slug);
		} catch (err: unknown) {
			const message = err instanceof Error ? err.message : "Unexpected error";
			setState({ kind: "error", message });
		}
	}

	switch (state.kind) {
		case "idle":
			return (
				<button className="go-button idle" onClick={handleGhostNow}>
					Ghost now!
				</button>
			);

		case "resuming":
			return (
				<>
					<Spinner />
					<p className="spawning-message">Checking…</p>
				</>
			);

		case "spawning":
			return (
				<>
					<Spinner />
					<p className="spawning-message">Spawning…</p>
				</>
			);

		case "done":
			return (
				<div className="go-button-done-message">
					<p>Your Ghost is ready at:</p>
					<a className="go-button-url" href={state.url}>
						{state.url}
					</a>
				</div>
			);

		case "expired":
			return (
				<div className="go-button-done-message">
					<p className="state-message">This goopy has expired.</p>
					<button className="go-button idle" onClick={handleReset}>
						Spawn a new one
					</button>
				</div>
			);

		case "failed":
			return (
				<div className="go-button-done-message">
					<p className="state-message">Goops! Spawning failed.</p>
					<button className="go-button idle" onClick={handleReset}>
						Try again!
					</button>
					<IssueLink prompt="Still stuck?" />
				</div>
			);

		case "error":
			return <ErrorMessage message={state.message} onReset={handleReset} />;
	}
}
