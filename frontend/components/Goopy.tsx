/**
 * The hero goopy.
 *
 * Purely decorative — the copy under it says what is happening — so it is
 * hidden from assistive tech.
 *
 * Two layers, so that motion and colour can be styled independently: the
 * body moves, and only the glyph inside it is tinted. Anything later attached
 * to the body moves with the goopy without picking up its filter.
 */
export default function Goopy() {
	return (
		<div className="goopy" aria-hidden="true">
			<div className="goopy-body">
				<span className="goopy-glyph">💩</span>
			</div>
		</div>
	);
}
