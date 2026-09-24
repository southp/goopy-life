import type { ShaDisplay } from "@/lib/version";

interface ShaLinkProps {
	name: string;
	display: ShaDisplay;
}

// `<name> <sha>`, the sha linked to its commit when it names one exactly.
export default function ShaLink({ name, display }: ShaLinkProps) {
	return (
		<span>
			{name}{" "}
			{display.href ? (
				<a
					href={display.href}
					className="version-sha"
					target="_blank"
					rel="noopener noreferrer"
				>
					{display.label}
				</a>
			) : (
				<span className="version-sha">{display.label}</span>
			)}
		</span>
	);
}
