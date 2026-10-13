import type { Mood } from "@/lib/mood";

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
			</div>
		</div>
	);
}
