import type { ReactNode } from "react";

interface AccordionProps {
	title: string;
	children: ReactNode;
}

// A collapsible section built on the native <details>/<summary> elements. This keeps
// it a Server Component — no client JS, no hydration — so the content is present in
// the server-rendered HTML even while collapsed and the browser handles toggling.
// Renders collapsed by default (native <details> with no `open` attribute).
export default function Accordion({ title, children }: AccordionProps) {
	return (
		<details className="accordion">
			<summary className="accordion-summary">
				<span className="section-heading">{title}</span>
			</summary>
			<div className="accordion-body">{children}</div>
		</details>
	);
}
