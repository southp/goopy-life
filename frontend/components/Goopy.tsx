import type { Mood } from "@/lib/mood";

// Placed and timed per index in globals.css.
const SPARKLES = [0, 1, 2, 3, 4];

interface GoopyProps {
	mood: Mood;
}

/**
 * The hero goopy, acting out what the page is doing (#208).
 *
 * Purely decorative — the copy under it says what is happening — so it is
 * hidden from assistive tech.
 *
 * Two layers, so that motion and colour can be styled independently: the
 * body moves, and only the glyph inside it is tinted. Anything attached to the
 * body moves with the goopy without picking up its filter. Each mood is styled
 * off `data-mood` in globals.css.
 */
export default function Goopy({ mood }: GoopyProps) {
	return (
		<div className="goopy" data-mood={mood} aria-hidden="true">
			<div className="goopy-body">
				<span className="goopy-glyph">💩</span>
				{mood === "proud" && (
					<>
						<span className="goopy-shine" />
						<span className="goopy-sparkles">
							{SPARKLES.map((i) => (
								<span key={i} className="goopy-sparkle" />
							))}
						</span>
					</>
				)}
				{(mood === "hurt" || mood === "sad") && (
					<>
						<span className="goopy-tear" />
						<span className="goopy-tear" />
					</>
				)}
				{mood === "hurt" && <Bandaid />}
			</div>
		</div>
	);
}

// A small cross of sticking plaster, for the side of its head.
function Bandaid() {
	return (
		<svg className="goopy-bandaid" viewBox="0 0 40 40">
			<g fill="#f1c27d" stroke="#c98f4f" strokeWidth="1.5">
				<rect x="3" y="15" width="34" height="10" rx="5" transform="rotate(45 20 20)" />
				<rect x="3" y="15" width="34" height="10" rx="5" transform="rotate(-45 20 20)" />
			</g>
			<rect x="15" y="15" width="10" height="10" rx="2" fill="#e3a868" transform="rotate(45 20 20)" />
			<g fill="#c98f4f">
				<circle cx="9" cy="9" r="1" />
				<circle cx="31" cy="9" r="1" />
				<circle cx="9" cy="31" r="1" />
				<circle cx="31" cy="31" r="1" />
			</g>
		</svg>
	);
}
