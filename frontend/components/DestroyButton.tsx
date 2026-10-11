'use client';

import { useState } from 'react';
import { destroyGoopy, isLastWord } from '@/lib/api';

interface DestroyButtonProps {
	slug: string;
	/** The despawn token this browser was issued when it spawned `slug`. */
	token: string;
	/** Called once the goopy is gone (or was already). */
	onDestroyed: () => void;
	/** Called when the server refuses this token for good, so it can be forgotten. */
	onTokenRejected: () => void;
}

// `refused` is terminal: the server said no in a way retrying cannot change.
type Phase = "offered" | "confirming" | "destroying" | "refused";

// Destroys the visitor's own goopy before it expires (#203), behind an inline
// confirmation: one stray click must not take a site down. Rendered only on the
// post-spawn page of the browser that spawned the goopy, since only that browser
// holds the token.
export default function DestroyButton({
	slug,
	token,
	onDestroyed,
	onTokenRejected,
}: DestroyButtonProps) {
	const [phase, setPhase] = useState<Phase>("offered");
	const [failed, setFailed] = useState(false);

	async function handleConfirm() {
		setPhase("destroying");
		setFailed(false);
		try {
			await destroyGoopy(slug, token);
			onDestroyed();
		} catch (err: unknown) {
			// Whatever the server said is for a log, not a visitor.
			if (isLastWord(err)) {
				// Offering the button again would only invite the same answer.
				setPhase("refused");
				onTokenRejected();
				return;
			}
			// May pass: the page stays as it was, with a line saying so.
			setFailed(true);
			setPhase("offered");
		}
	}

	switch (phase) {
		case "offered":
			return (
				<div className="destroy-area">
					{failed && (
						<p className="state-message" role="status">
							Couldn&apos;t destroy it. Try again in a moment.
						</p>
					)}
					<button className="destroy-button" onClick={() => setPhase("confirming")}>
						Destroy
					</button>
				</div>
			);

		case "confirming":
			return (
				<div className="destroy-area">
					<p className="state-message" role="status">
						Sure? This can&apos;t be undone.
					</p>
					<div className="destroy-actions">
						<button className="destroy-button confirm" onClick={handleConfirm}>
							Destroy
						</button>
						<button className="destroy-button" onClick={() => setPhase("offered")}>
							Keep it
						</button>
					</div>
				</div>
			);

		case "refused":
			return (
				<div className="destroy-area">
					<p className="state-message" role="status">
						This one can&apos;t be destroyed from here. It will expire on its own.
					</p>
				</div>
			);

		case "destroying":
			return (
				<div className="destroy-area">
					<p className="spawning-message" role="status">
						Destroying…
					</p>
				</div>
			);
	}
}
