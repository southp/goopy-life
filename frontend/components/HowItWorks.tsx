import Accordion from "@/components/Accordion";

interface HowItWorksProps {
	lifeInDays: number | null;
	storageQuotaMb: number | null;
}

// Server-rendered "How it works" copy. Config values are nullable — when the
// build-time fetch failed we render `--` placeholders rather than fabricated numbers.
export default function HowItWorks({ lifeInDays, storageQuotaMb }: HowItWorksProps) {
	const lifeLabel =
		lifeInDays === null
			? "--"
			: `${lifeInDays} ${lifeInDays === 1 ? "day" : "days"}`;
	const storageLabel = storageQuotaMb === null ? "--" : `${storageQuotaMb} MB`;

	return (
		<Accordion title="How it works">
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
				<strong>{lifeLabel}</strong> and gets a <strong>{storageLabel}</strong> storage
				allowance. After that it quietly evaporates — no backups, no exports, no lingering
				data. Think of it as a sandcastle at high tide: beautiful, temporary, completely
				intentional.
			</p>
			<ul className="explainer-list">
				<li>No sign-up. No email. No password.</li>
				<li>Lifetime: <strong>{lifeLabel}</strong></li>
				<li>Disk quota: <strong>{storageLabel}</strong></li>
				<li>Backups: none. Plan accordingly.</li>
			</ul>
		</Accordion>
	);
}
