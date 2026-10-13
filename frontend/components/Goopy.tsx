import type { AnimationEvent, CSSProperties } from "react";
import type { Mood } from "@/lib/mood";

// Placed and timed per index in globals.css.
const SPARKLES = [0, 1, 2, 3, 4];
const STEAM_PUFFS = [0, 1, 2];

// The goo the explosion throws out: evenly round the circle, every other drop
// flying further, so the splat is not a perfect ring.
const DROPS = Array.from({ length: 10 }, (_, i) => ({
	"--angle": `${i * 36}deg`,
	"--reach": i % 2 === 0 ? "5.5rem" : "4rem",
}) as CSSProperties);

interface GoopyProps {
	mood: Mood;
	/** Called when the explosion has played out. */
	onRebornEnd: () => void;
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
export default function Goopy({ mood, onRebornEnd }: GoopyProps) {
	// The decorations' animations bubble up here too; only the body's own
	// rebirth marks the end.
	function handleAnimationEnd(e: AnimationEvent) {
		if (e.animationName === "goopy-reborn") {
			onRebornEnd();
		}
	}

	return (
		<div className="goopy" data-mood={mood} aria-hidden="true">
			{/* Outside the body, which shrinks to nothing as these fly out. */}
			{mood === "reborn" && (
				<>
					<span className="goopy-burst" />
					<span className="goopy-drops">
						{DROPS.map((style, i) => (
							<span key={i} className="goopy-drop" style={style} />
						))}
					</span>
				</>
			)}
			<div className="goopy-body" onAnimationEnd={handleAnimationEnd}>
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
				{mood === "brewing" && (
					<span className="goopy-steam">
						{STEAM_PUFFS.map((i) => (
							<span key={i} className="goopy-puff" />
						))}
					</span>
				)}
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
