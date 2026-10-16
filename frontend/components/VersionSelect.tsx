import { versionLabel } from "@/lib/ghostVersions";
import type { GhostVersionsResponse } from "@/lib/types";

interface VersionSelectProps {
	offered: GhostVersionsResponse;
	value: string;
	onChange: (version: string) => void;
}

/**
 * Which Ghost the CTA spawns (#210), above the button.
 *
 * A native `<select>`: keyboard, screen readers and phones get their own
 * picker for free. Each option is the version and nothing else — no "latest",
 * no "legacy": which one is newest is the visitor's to read off the number,
 * and anything more would be a claim the server never made.
 *
 * Hung off the button's top edge, out of flow, for the reason the capacity
 * readout hangs off its bottom: `/ghost-versions` answers after the first
 * paint, and an in-flow control would push the button down under the
 * visitor's cursor when it arrived.
 */
export default function VersionSelect({ offered, value, onChange }: VersionSelectProps) {
	return (
		<div className="version-slot">
			<select
				className="version-select"
				aria-label="Ghost version"
				value={value}
				onChange={(e) => onChange(e.target.value)}
			>
				{offered.versions.map(({ version }) => (
					<option key={version} value={version}>
						{versionLabel(version)}
					</option>
				))}
			</select>
		</div>
	);
}
