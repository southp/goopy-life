import Accordion from "@/components/Accordion";
import { formatLifetime } from "@/lib/lifetime";

interface HowItWorksProps {
	lifeInHours: number | null;
	storageQuotaMb: number | null;
}

// Server-rendered "How it works" copy. Config values are nullable — when the
// build-time fetch failed we render `--` placeholders rather than fabricated numbers.
export default function HowItWorks({ lifeInHours, storageQuotaMb }: HowItWorksProps) {
	const lifeLabel = lifeInHours === null ? "--" : formatLifetime(lifeInHours);
	const storageLabel = storageQuotaMb === null ? "--" : `${storageQuotaMb} MB`;

	return (
		<Accordion title="How it works" defaultOpen>
			<p>
				goopy.life is an ephemeral {' '}
				<a className="inline-link" href="https://ghost.org" target="_blank" rel="noopener noreferrer">
					<strong>Ghost</strong>
				</a>{' '}
				sandboxing service inspired by {' '}
				<a className="inline-link" href="https://poopy.life" target="_blank" rel="noopener noreferrer">
					poopy.life.
				</a>{' '}
				By one click, you got your Ghost instance up and running,{' '}
				with a domain name that reads too funny for any serious production uses.
			</p>
			<p>
				By being <strong>ephemeral</strong>, it means your Ghost instance lives for only a limited time.
				After that it quietly evaporates. Like a transient traveler to this world, proudly strides, leaving no trace.
			</p>
			<ul className="explainer-list">
				<li>Lifetime: <strong>{lifeLabel}</strong></li>
				<li>Disk quota: <strong>{storageLabel}</strong></li>
				<li>Backups: <strong>none</strong>. Seriously, there is none.</li>
			</ul>
		</Accordion>
	);
}
