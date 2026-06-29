export const dynamic = 'force-static';

import GhostButton from '@/components/GhostButton';
import HowItWorks from '@/components/HowItWorks';
import TermsOfUse from '@/components/TermsOfUse';
import { fetchConfig } from '@/lib/config';

// Server Component. The page structure and copy are rendered server-side and present
// in the static HTML before any interactivity; the only client island is the
// <GhostButton /> CTA.
export default async function Home() {
	const config = await fetchConfig();

	return (
		<div className="page-wrapper">
			<main className="page-main">
				<section className="hero-block">
					<span className="hero-emoji">💩</span>
					<div className="cta-area">
						<GhostButton />
					</div>
				</section>

				<div className="info-sections">
					<HowItWorks
						lifeInDays={config.life_in_days}
						storageQuotaMb={config.storage_quota_mb}
					/>
					<TermsOfUse />
				</div>
			</main>
		</div>
	);
}
