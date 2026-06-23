"use client";

import { useCallback, useEffect, useRef, useState } from "react";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Mirrors the API's GoopyResponse shape. */
interface GoopyResponse {
	slug: string;
	status: string;
	url: string;
	created_at: string;
	expires_at: string;
}

/** Mirrors the API's ErrorResponse shape. */
interface ApiError {
	error: string;
	code: string;
}

type AppState =
	| { kind: "idle" }
	| { kind: "resuming"; slug: string }  // checking a saved slug on mount
	| { kind: "spawning" }
	| { kind: "done"; slug: string; url: string }
	| { kind: "expired"; slug: string }
	| { kind: "failed"; slug: string }
	| { kind: "error"; message: string };

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const GITHUB_ISSUES_URL = "https://github.com/southp/goopy-life/issues";
const LOCALSTORAGE_KEY = "goopy_slug";
const POLL_INTERVAL_MS = 2000;

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

function apiBase(): string {
	const base = process.env.NEXT_PUBLIC_API_URL ?? "";
	if (process.env.NODE_ENV !== "production" && !base) {
		console.warn("NEXT_PUBLIC_API_URL is not set — API calls will use relative paths");
	}
	return base;
}

async function extractError(res: Response): Promise<string> {
	try {
		const json: ApiError = await res.json();
		return json.error ?? `HTTP ${res.status}`;
	} catch {
		return `HTTP ${res.status}`;
	}
}

async function spawnGoopy(): Promise<{ slug: string }> {
	const res = await fetch(`${apiBase()}/goopies`, { method: "POST" });
	if (!res.ok) {
		throw new Error(await extractError(res));
	}
	return res.json();
}

async function getGoopy(slug: string, signal?: AbortSignal): Promise<GoopyResponse> {
	const res = await fetch(`${apiBase()}/goopies/${slug}`, { signal });
	if (res.status === 404) {
		throw new Error("not_found");
	}
	if (!res.ok) {
		throw new Error(await extractError(res));
	}
	return res.json();
}

// ---------------------------------------------------------------------------
// Error display
// ---------------------------------------------------------------------------

function ErrorMessage({ message, onReset }: { message: string; onReset: () => void }) {
	return (
		<div className="error-block">
			<p className="error-message">{message}</p>
			<p>
				Need help?{" "}
				<a
					href={GITHUB_ISSUES_URL}
					className="error-link"
					target="_blank"
					rel="noopener noreferrer"
				>
					Open an issue on GitHub
				</a>
			</p>
			<button className="go-button idle" onClick={onReset}>
				Try again
			</button>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main page component
// ---------------------------------------------------------------------------

export default function Home() {
	// Always start idle so SSR and the first client render agree.
	const [state, setState] = useState<AppState>({ kind: "idle" });
	const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

	// ── after hydration: resume a saved slug ──
	useEffect(() => {
		const saved = localStorage.getItem(LOCALSTORAGE_KEY);
		if (saved) setState({ kind: "resuming", slug: saved });
	}, []);

	// ── cleanup on unmount ──
	useEffect(() => {
		return () => {
			if (pollRef.current !== null) clearInterval(pollRef.current);
		};
	}, []);

	// ── reset helper ──
	const handleReset = useCallback(() => {
		if (pollRef.current !== null) {
			clearInterval(pollRef.current);
			pollRef.current = null;
		}
		localStorage.removeItem(LOCALSTORAGE_KEY);
		setState({ kind: "idle" });
	}, []);

	// ── start polling a slug until it reaches Done or Failed ──
	const startPolling = useCallback((slug: string) => {
		if (pollRef.current !== null) clearInterval(pollRef.current);

		pollRef.current = setInterval(async () => {
			try {
				const data = await getGoopy(slug);

				if (data.status === "Done") {
					clearInterval(pollRef.current!);
					pollRef.current = null;
					setState({ kind: "done", slug: data.slug, url: data.url });
				} else if (data.status === "Failed") {
					clearInterval(pollRef.current!);
					pollRef.current = null;
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "failed", slug });
				}
				// Still "Spawning" — keep polling
			} catch (err: unknown) {
				clearInterval(pollRef.current!);
				pollRef.current = null;
				const message = err instanceof Error ? err.message : "Unexpected error";
				setState({ kind: "error", message });
			}
		}, POLL_INTERVAL_MS);
	}, []);

	// ── resume: fetch the saved slug once to establish its current state ──
	useEffect(() => {
		if (state.kind !== "resuming") return;
		const { slug } = state;
		const controller = new AbortController();

		getGoopy(slug, controller.signal).then((data) => {
			if (data.status === "Done") {
				const expired = Date.now() > new Date(data.expires_at).getTime();
				if (expired) {
					localStorage.removeItem(LOCALSTORAGE_KEY);
					setState({ kind: "expired", slug });
				} else {
					setState({ kind: "done", slug: data.slug, url: data.url });
				}
			} else if (data.status === "Failed") {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "failed", slug });
			} else {
				// Still spawning — start polling
				startPolling(slug);
				setState({ kind: "spawning" });
			}
		}).catch((err: Error) => {
			if (controller.signal.aborted) return;
			if (err.message === "not_found") {
				// Stale slug in localStorage
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "idle" });
			} else {
				setState({ kind: "error", message: err.message });
			}
		});

		return () => controller.abort();
	}, [state, startPolling]);

	// ── "Ghost now!" click handler ──
	async function handleGhostNow() {
		setState({ kind: "spawning" });

		try {
			const { slug } = await spawnGoopy();
			localStorage.setItem(LOCALSTORAGE_KEY, slug);
			startPolling(slug);
		} catch (err: unknown) {
			const message = err instanceof Error ? err.message : "Unexpected error";
			setState({ kind: "error", message });
		}
	}

	// ── render CTA area ──
	function renderCta() {
		switch (state.kind) {
			case "idle":
				return (
					<button className="go-button idle" onClick={handleGhostNow}>
						Ghost now!
					</button>
				);

			case "resuming":
			case "spawning":
				return (
					<>
						<span className="spinner" aria-label="Loading" />
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
						<p className="state-message">Your previous site has expired.</p>
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
					</div>
				);

			case "error":
				return <ErrorMessage message={state.message} onReset={handleReset} />;
		}
	}

	return (
		<div className="page-wrapper">
			<main className="page-main">
				<span className="hero-emoji">💩</span>
				<div className="cta-area">{renderCta()}</div>
			</main>
		</div>
	);
}
