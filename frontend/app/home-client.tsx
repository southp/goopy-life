'use client';

import { useCallback, useEffect, useState } from 'react';

type GoopyStatus = "Spawning" | "Done" | "Failed" | "Despawning" | "Archived" | "Empty";

interface GoopyResponse {
	slug: string;
	status: GoopyStatus;
	url: string;
	created_at: string;
	expires_at: string;
}

interface ApiError {
	error: string;
	code: string;
}

type AppState =
	| { kind: "idle" }
	| { kind: "resuming"; slug: string }
	| { kind: "spawning" }
	| { kind: "done"; slug: string; url: string }
	| { kind: "expired"; slug: string }
	| { kind: "failed"; slug: string }
	| { kind: "error"; message: string };

const GITHUB_ISSUES_URL = "https://github.com/southp/goopy-life/issues";
const LOCALSTORAGE_KEY = "goopy_slug";
const POLL_INTERVAL_MS = 2000;

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

interface HomeClientProps {
	lifeInDays: number;
	storageQuotaMb: number;
}

export default function HomeClient({ lifeInDays, storageQuotaMb }: HomeClientProps) {
	const [state, setState] = useState<AppState>({ kind: "idle" });
	const [pollSlug, setPollSlug] = useState<string | null>(null);

	useEffect(() => {
		const params = new URLSearchParams(window.location.search);
		if (params.has("expired")) {
			window.history.replaceState({}, "", "/");
			setState({ kind: "expired", slug: "" });
			return;
		}
		const saved = localStorage.getItem(LOCALSTORAGE_KEY);
		if (saved) setState({ kind: "resuming", slug: saved });
	}, []);

	useEffect(() => {
		if (!pollSlug) return;

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
				if (controller.signal.aborted) return;
				localStorage.removeItem(LOCALSTORAGE_KEY);
				const message = err instanceof Error ? err.message : "Unexpected error";
				setState({ kind: "error", message });
				setPollSlug(null);
				return;
			}
			timeoutId = setTimeout(tick, POLL_INTERVAL_MS);
		};

		timeoutId = setTimeout(tick, POLL_INTERVAL_MS);

		return () => {
			controller.abort();
			if (timeoutId !== null) clearTimeout(timeoutId);
		};
	}, [pollSlug]);

	const handleReset = useCallback(() => {
		setPollSlug(null);
		localStorage.removeItem(LOCALSTORAGE_KEY);
		setState({ kind: "idle" });
	}, []);

	useEffect(() => {
		if (state.kind !== "resuming") return;
		const { slug } = state;
		const controller = new AbortController();

		getGoopy(slug, controller.signal).then((data) => {
			if (controller.signal.aborted) return;
			if (data.status === "Done") {
				const expired = Date.now() > new Date(data.expires_at).getTime();
				if (expired) {
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
			if (controller.signal.aborted) return;
			if (err.message === "not_found") {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "idle" });
			} else {
				localStorage.removeItem(LOCALSTORAGE_KEY);
				setState({ kind: "error", message: err.message });
			}
		});

		return () => controller.abort();
	}, [state]);

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

	function renderCta() {
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
						<span className="spinner" aria-label="Loading" />
						<p className="spawning-message">Checking…</p>
					</>
				);

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

				<section className="explainer-section">
					<h2 className="section-heading">How it works</h2>
					<p>
						A <strong>goopy</strong> is a freshly spun-up{' '}
						<a className="inline-link" href="https://ghost.org" target="_blank" rel="noopener noreferrer">
							Ghost
						</a>{' '}
						instance — yours, immediately, no account required. Hit the button, wait a few
						seconds, and you land inside a fully functional Ghost admin. Do whatever you like:
						kick the tyres, write a draft, show a client.
					</p>
					<p>
						The catch? It&apos;s <strong>ephemeral</strong>. Your goopy lives for{' '}
						<strong>{lifeInDays} {lifeInDays === 1 ? 'day' : 'days'}</strong> and gets a{' '}
						<strong>{storageQuotaMb} MB</strong> storage allowance. After that it quietly
						evaporates — no backups, no exports, no lingering data. Think of it as a sandcastle
						at high tide: beautiful, temporary, completely intentional.
					</p>
					<ul className="explainer-list">
						<li>No sign-up. No email. No password.</li>
						<li>Lifetime: <strong>{lifeInDays} {lifeInDays === 1 ? 'day' : 'days'}</strong></li>
						<li>Disk quota: <strong>{storageQuotaMb} MB</strong></li>
						<li>Backups: none. Plan accordingly.</li>
					</ul>
				</section>

				<section className="tou-section">
					<h2 className="section-heading">Terms of use</h2>
					<p>
						Goopy.life is a free service offered in good faith. By using it you agree to the
						following:
					</p>
					<ul className="tou-list">
						<li>No abuse — don&apos;t use your goopy to spam, scrape, or attack other systems.</li>
						<li>No illegal content — anything prohibited by applicable law is prohibited here.</li>
						<li>
							We reserve the right to terminate any goopy, at any time, for any reason — or
							for no reason at all. Usually it&apos;s just the timer.
						</li>
					</ul>
					<p className="tou-footer">
						That&apos;s it. No legalese, no dark patterns. Just be a decent human.
					</p>
				</section>
			</main>
		</div>
	);
}
