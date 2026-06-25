import type { Metadata } from "next";

export const metadata: Metadata = {
	title: "Goopy expired — Goopy.Life",
};

export default function ExpiredPage() {
	return (
		<div className="page-wrapper">
			<main className="page-main">
				<span className="hero-emoji">💩</span>
				<p className="expired-heading">This goopy has expired.</p>
				<p className="expired-message">
					Ephemeral Ghost instances on Goopy.Life have a limited lifespan.
					This one has run its course. Want a fresh one?
				</p>
				<a className="go-home-link" href="https://goopy.life">
					Ghost now!
				</a>
			</main>
		</div>
	);
}
