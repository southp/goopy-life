'use client';

import { useCallback, useEffect, useState } from 'react';
import { adminUrl } from '@/lib/admin';
import { ApiRequestError, getGoopy, spawnGoopy, waitUntilGone } from '@/lib/api';
import { LOCALSTORAGE_KEY, POLL_INTERVAL_MS } from '@/lib/constants';
import { formatExpiry } from '@/lib/expiry';
import { moodFor } from '@/lib/mood';
import { resumeAt } from '@/lib/resume';
import { parseStoredGoopy, serializeStoredGoopy, type StoredGoopy } from '@/lib/session';
import { useCapacity } from '@/lib/useCapacity';
import type { AppState, GoopyResponse } from '@/lib/types';
import CapacityIndicator from '@/components/CapacityIndicator';
import DestroyButton from '@/components/DestroyButton';
import ErrorMessage from '@/components/ErrorMessage';
import Goopy from '@/components/Goopy';
import IssueLink from '@/components/IssueLink';
import Spinner from '@/components/Spinner';

/** Turn a thrown request failure into the error state, preserving the API code. */
function errorState(err: unknown): AppState {
	if (err instanceof ApiRequestError) {
		return { kind: "error", message: err.message, code: err.code };
	}
	const message = err instanceof Error ? err.message : "Unexpected error";
	return { kind: "error", message, code: null };
}

/** The done state for a ready instance, from either the poll or a resume. */
function doneState(data: GoopyResponse, token: string | null): AppState {
	return {
		kind: "done",
		slug: data.slug,
		url: data.url,
		expiresAt: data.expires_at,
		token,
	};
}

// The sole interactive element on the page: the hero goopy, the "Ghost now!"
// CTA under it, and their state machine (spawn → poll → done/failed, plus
// resume-from-localStorage and the expired-redirect handling). Everything else
// on the page is server-rendered.
export default function GhostButton() {
	const [state, setState] = useState<AppState>({ kind: "idle" });
	// The goopy being polled until it is ready. Carries the token so the done
	// state can offer the destroy button without re-reading localStorage.
	const [polling, setPolling] = useState<StoredGoopy | null>(null);
	// The destroy button has a destroy in flight. Its own state, since only the
	// goopy above it needs to know.
	const [destroying, setDestroying] = useState(false);

	// Only poll capacity while the button is actually offered. In every other
	// state the number is not on screen, and a spawning or finished visitor has
	// no use for it.
	const capacity = useCapacity(state.kind === "idle");
	// Unknown capacity (first poll not back, or /capacity unreachable) must not
	// disable the button: the 503 is the real guard, so we fail open.
	const isFull = capacity?.is_full ?? false;

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
		const saved = parseStoredGoopy(localStorage.getItem(LOCALSTORAGE_KEY));
		if (saved) {
			setState({ kind: "resuming", slug: saved.slug, token: saved.token });
		}
	}, []);
	/* eslint-enable react-hooks/set-state-in-effect */

	useEffect(() => {
		if (!polling) {
			return;
		}
		const pollSlug = polling.slug;

		const controller = new AbortController();
		let timeoutId: ReturnType<typeof setTimeout> | null = null;

		const tick = async () => {
			try {
				const data = await getGoopy(pollSlug, controller.signal);

				if (data.status === "Done") {
					setState(doneState(data, polling.token));
					setPolling(null);
					return;
				} else if (data.status !== "Spawning") {
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "failed", slug: pollSlug });
					setPolling(null);
					return;
				}
			} catch (err: unknown) {
				if (controller.signal.aborted) {
					return;
				}
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState(errorState(err));
				setPolling(null);
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
	}, [polling]);

	const handleReset = useCallback(() => {
		setPolling(null);
		setDestroying(false);
		localStorage.removeItem(LOCALSTORAGE_KEY);
		setState({ kind: "idle" });
	}, []);

	// The server refused this browser's token for good (#203). Keep the page —
	// the goopy is still running — but forget the token, so a reload does not
	// offer a button that can only fail. `state` is left alone so that the
	// destroy control can keep saying why it is gone.
	const handleTokenRejected = useCallback(() => {
		if (state.kind !== "done") {
			return;
		}
		localStorage.setItem(
			LOCALSTORAGE_KEY,
			serializeStoredGoopy({ slug: state.slug, token: null }),
		);
	}, [state]);

	// Depend only on the resuming slug and token (null when not resuming) so this
	// effect fires once per entry into "resuming" rather than re-running — and
	// recreating its AbortController — on every unrelated state transition.
	const resumingSlug = state.kind === "resuming" ? state.slug : null;
	const resumingToken = state.kind === "resuming" ? state.token : null;

	useEffect(() => {
		if (resumingSlug === null) {
			return;
		}
		const slug = resumingSlug;
		const token = resumingToken;
		const controller = new AbortController();

		getGoopy(slug, controller.signal).then((data) => {
			if (controller.signal.aborted) {
				return;
			}
			switch (resumeAt(data)) {
				case "done":
					setState(doneState(data, token));
					break;
				case "expired":
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "expired", slug });
					break;
				case "spawning":
					setState({ kind: "spawning" });
					setPolling({ slug, token });
					break;
				case "leaving":
					// Kept in localStorage until it is gone, so reloading again
					// lands here again rather than on a fresh page.
					setState({ kind: "leaving", slug });
					break;
				case "failed":
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "failed", slug });
					break;
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
				setState(errorState(err));
			}
		});

		return () => controller.abort();
	}, [resumingSlug, resumingToken]);

	// Finish what a destroy started before the reload: the goopy holds its slot
	// until the teardown ends, so offer a new one only once it is gone — as the
	// destroy button itself does (#203).
	const leavingSlug = state.kind === "leaving" ? state.slug : null;

	useEffect(() => {
		if (leavingSlug === null) {
			return;
		}
		const controller = new AbortController();
		waitUntilGone(leavingSlug, { intervalMs: POLL_INTERVAL_MS, signal: controller.signal })
			.then(handleReset)
			.catch(() => {
				// Aborted: this effect is being torn down, nothing to update.
			});
		return () => controller.abort();
	}, [leavingSlug, handleReset]);

	async function handleGhostNow() {
		setState({ kind: "spawning" });

		try {
			const { slug, despawn_token } = await spawnGoopy();
			const spawned: StoredGoopy = { slug, token: despawn_token ?? null };
			localStorage.setItem(LOCALSTORAGE_KEY, serializeStoredGoopy(spawned));
			setPolling(spawned);
		} catch (err: unknown) {
			// A capacity refusal needs no special handling here: the message from
			// the server is shown as-is, and "Try again" re-enters the idle state,
			// which re-reads /capacity immediately — so a genuinely full server
			// comes back with the button already disabled.
			setState(errorState(err));
		}
	}

	// The goopy and the CTA under it read the same state, so they live in one
	// island; the switch below only picks what goes under the goopy.
	function renderCta() {
		switch (state.kind) {
			case "idle":
				// The wrapper is not cosmetic: it is the positioning context the
				// capacity readout hangs off, which is what keeps the button still
				// when /capacity finally answers.
				return (
					<div className="cta-idle">
						<button
							className={`go-button ${isFull ? "disabled" : "idle"}`}
							onClick={handleGhostNow}
							disabled={isFull}
						>
							Ghost now!
						</button>
						<CapacityIndicator capacity={capacity} />
					</div>
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

			case "leaving":
				return (
					<>
						<Spinner />
						<p className="spawning-message">Destroying…</p>
					</>
				);

			case "done": {
				const availableUntil = formatExpiry(state.expiresAt, new Date());
				// Link the admin dashboard rather than the site: claiming the admin
				// account is the first thing anyone has to do with a fresh Ghost.
				const admin = adminUrl(state.url);
				return (
					<div className="go-button-done-message">
						<p>Your Ghost is ready at:</p>
						<a className="go-button-url" href={admin}>
							{admin}
						</a>
						{availableUntil !== null && (
							<p className="expiry-message">Available until {availableUntil}</p>
						)}
						{/* Only with a token: without one, the server would refuse. */}
						{state.token !== null && (
							<DestroyButton
								slug={state.slug}
								token={state.token}
								onDestroyed={handleReset}
								onTokenRejected={handleTokenRejected}
							onDestroyingChange={setDestroying}
							/>
						)}
					</div>
				);
			}

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
				return (
					<ErrorMessage
						message={state.message}
						code={state.code}
						onReset={handleReset}
					/>
				);
		}
	}

	return (
		<>
			<Goopy mood={moodFor(state, destroying)} />
			<div className="cta-area">{renderCta()}</div>
		</>
	);
}
