import { versionLabel, type VersionSegment } from "@/lib/ghostVersions";

interface VersionSelectProps {
	segment: VersionSegment;
	// The button above is disabled (no free slot): nothing to choose for.
	disabled: boolean;
	onChange: (version: string) => void;
}

/**
 * Which Ghost the CTA spawns (#210), at the head of the line under the button.
 *
 * A native `<select>`: keyboard, screen readers and phones get their own
 * picker for free. An outline pill in full-white text: the one thing on that
 * line to act on, so it is the brightest thing on it, and still quieter than
 * the button.
 *
 * Each option is the version and nothing else — no "latest", no "legacy":
 * which one is newest is the visitor's to read off the number, and anything
 * more would be a claim the server never made.
 *
 * Disabled while `pending`: it then holds the remembered version alone, which
 * is still what a click asks for, and becomes choosable when the list arrives.
 * It keeps its look meanwhile — only "no free slot" greys it out — so that a
 * list arriving a beat late does not flick it from grey to white.
 */
export default function VersionSelect({ segment, disabled, onChange }: VersionSelectProps) {
	return (
		<select
			className={`version-select${segment.pending ? " pending" : ""}`}
			aria-label="Ghost version"
			value={segment.value}
			disabled={disabled || segment.pending}
			onChange={(e) => onChange(e.target.value)}
		>
			{segment.options.map((version) => (
				<option key={version} value={version}>
					{versionLabel(version)}
				</option>
			))}
		</select>
	);
}
