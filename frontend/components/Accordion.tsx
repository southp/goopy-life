import type { ReactNode } from "react";

interface AccordionProps {
	title: string;
	defaultOpen?: boolean;
	children: ReactNode;
}

// A collapsible section built on the native <details>/<summary> elements. This keeps
// it a Server Component — no client JS, no hydration — so the content is present in
// the server-rendered HTML even while collapsed and the browser handles toggling.
export default function Accordion({ title, defaultOpen = false, children }: AccordionProps) {
	return (
		<details className="accordion" open={defaultOpen}>
			<summary className="accordion-summary">
				<span className="section-heading">{title}</span>
			</summary>
			<div className="accordion-body">{children}</div>
		</details>
	);
}
