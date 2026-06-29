import Accordion from "@/components/Accordion";

// Server-rendered "Terms of use" copy.
export default function TermsOfUse() {
	return (
		<Accordion title="Terms of use">
			<p>
				Goopy.life is a free service offered in good faith. By using it you agree to the
				following:
			</p>
			<ul className="tou-list">
				<li>No abuse — don&apos;t use your goopy to spam, scrape, or attack other systems.</li>
				<li>No illegal content — anything prohibited by applicable law is prohibited here.</li>
				<li>
					No adult or explicit material — goopies may not be used to host pornographic or
					otherwise not-safe-for-work content.
				</li>
				<li>
					We reserve the right to terminate any goopy, at any time, for any reason — or
					for no reason at all. Usually it&apos;s just the timer.
				</li>
			</ul>
			<p className="tou-footer">
				That&apos;s it. No legalese, no dark patterns. Just be a decent human.
			</p>
		</Accordion>
	);
}
